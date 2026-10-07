//! Windows AppContainer containment backend (substrate).
//!
//! A per-user AppContainer profile is created for a sandbox, the granted root
//! receives a Modify ACE for the container SID, and the child is launched with
//! zero capabilities. Measured on a standard, non-elevated account (Windows 11
//! 26200): writes outside granted roots and outbound network (including
//! loopback) are denied, while writes inside the granted root succeed.
//!
//! This module is the enforcement backend. `sandbox::create_workspace_sandbox`
//! builds a container and grants Modify on the workspace root once
//! workspace mode verified containment at startup; the execution broker then
//! launches every `exec`, `git` and trusted-hook child through it. The cheap
//! runtime `probe()` makes no claim of its own: the capability flags come from
//! the executed check (`verified_probe`), so `workspace` mode cannot run
//! uncontained on the host.

use std::collections::BTreeMap;
use std::ffi::{c_void, OsStr};
use std::fs::File;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{FromRawHandle, RawHandle};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};

use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSidToSidW, GetNamedSecurityInfoW, SetEntriesInAclW,
    SetNamedSecurityInfoW, EXPLICIT_ACCESS_W, GRANT_ACCESS, NO_MULTIPLE_TRUSTEE, REVOKE_ACCESS,
    SE_FILE_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile,
};
use windows_sys::Win32::Security::{
    AclSizeInformation, DeriveCapabilitySidsFromName, EqualSid, FreeSid, GetAce, GetAclInformation,
    GetLengthSid, ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION, CONTAINER_INHERIT_ACE,
    DACL_SECURITY_INFORMATION, INHERIT_ONLY_ACE, NO_INHERITANCE, OBJECT_INHERIT_ACE,
    PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_DELETE_CHILD, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::SystemServices::{ACCESS_ALLOWED_ACE_TYPE, SE_GROUP_ENABLED};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, ResumeThread, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject, CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    EXTENDED_STARTUPINFO_PRESENT, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

use crate::process::{
    base_environment, drain, filesystem_path, ChildSandbox, ExecOutcome, ExecRequest, ExecStatus,
};
use crate::sandbox::find_on_path;

/// `CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE`: containers and their
/// contents (files and subdirectories) inherit the entry.
const SUB_CONTAINERS_AND_OBJECTS_INHERIT: u32 = CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE;

/// Read + execute: a contained build must be able to read the toolchain and
/// its caches and to execute the binaries it reads.
const READ_EXECUTE: u32 = FILE_GENERIC_READ | FILE_GENERIC_EXECUTE;

/// Modify within one grant: read/write/execute plus delete, because a build
/// replaces and renames the files it owns (`FILE_GENERIC_WRITE` alone cannot
/// rename a file over an existing target).
const MODIFY_RIGHTS: u32 =
    FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE | FILE_DELETE_CHILD;

/// The device Rust's std opens for a child's stdin whenever a spawn asks for
/// null stdio (`Stdio::null()`, and `Command::output()`, which nulls stdin so a
/// child cannot read the parent's). The open is issued by the process that
/// spawns, so a *contained* process needs the device reachable from inside the
/// container: a contained `cargo` asks for it immediately, probing `rustc -vV`
/// through `output()`, and a contained `cmd` asks for it too.
const NUL_DEVICE: &str = r"\\.\NUL";

/// Rights a null-stdio open needs: `FILE_GENERIC_READ` for stdin and
/// `FILE_GENERIC_WRITE` for stdout/stderr. Both include `SYNCHRONIZE`.
const NUL_RIGHTS: u32 = FILE_GENERIC_READ | FILE_GENERIC_WRITE;

/// The well-known AppContainer SIDs a lowbox access check also matches: ALL
/// APPLICATION PACKAGES (`S-1-15-2-1`) and ALL RESTRICTED APPLICATION PACKAGES
/// (`S-1-15-2-2`). A host whose device already names either one (the Windows 11
/// default) needs no entry for this container.
const ANY_PACKAGE_SIDS: [&str; 2] = ["S-1-15-2-1", "S-1-15-2-2"];

/// Capability shared by every workspace run on this host: the read grants on
/// the Rust toolchain and package caches. It is stable across runs, so the
/// one-time walk that covers pre-existing files is reused instead of repeated.
pub const TOOLCHAIN_CAPABILITY: &str = "bollo.toolchain.read.v1";

/// A derived capability SID set, owned as copied bytes (the arrays the
/// derivation API returns are freed immediately). Capabilities are the stable
/// grant principals here: the container's own SID is per run, so grants
/// attached to it could never be reused, and re-walking whole toolchain trees
/// every run is not viable (measured at roughly 350 us per object; the package
/// registry alone is tens of thousands of files).
pub struct CapabilitySet {
    sids: Vec<Vec<u8>>,
    strings: Vec<String>,
}

impl CapabilitySet {
    /// Derive the capability SID(s) for a stable name. The derivation is
    /// deterministic, so the same name yields the same SIDs in every process.
    pub fn derive(name: &str) -> Result<Self, String> {
        let name_wide = wide(name);
        let mut group_sids: *mut PSID = std::ptr::null_mut();
        let mut group_count: u32 = 0;
        let mut capability_sids: *mut PSID = std::ptr::null_mut();
        let mut capability_count: u32 = 0;
        let derived = unsafe {
            DeriveCapabilitySidsFromName(
                name_wide.as_ptr(),
                &mut group_sids,
                &mut group_count,
                &mut capability_sids,
                &mut capability_count,
            )
        };
        if derived == 0 {
            return Err(format!(
                "DeriveCapabilitySidsFromName({name}): {}",
                unsafe { GetLastError() }
            ));
        }
        let mut sids: Vec<Vec<u8>> = Vec::new();
        let mut strings: Vec<String> = Vec::new();
        unsafe {
            for index in 0..capability_count as usize {
                let sid = *capability_sids.add(index);
                if sid.is_null() {
                    continue;
                }
                let length = GetLengthSid(sid) as usize;
                let bytes = std::slice::from_raw_parts(sid as *const u8, length).to_vec();
                if let Some(text) = sid_to_string(bytes.as_ptr() as PSID) {
                    strings.push(text);
                }
                sids.push(bytes);
                FreeSid(sid);
            }
            // The group SIDs describe the package identity, which is not part
            // of this design: only capability SIDs are granted and carried.
            for index in 0..group_count as usize {
                let sid = *group_sids.add(index);
                if !sid.is_null() {
                    FreeSid(sid);
                }
            }
            if !group_sids.is_null() {
                LocalFree(group_sids as *mut c_void);
            }
            if !capability_sids.is_null() {
                LocalFree(capability_sids as *mut c_void);
            }
        }
        if sids.is_empty() {
            return Err(format!("capability {name} derived no SIDs"));
        }
        Ok(Self { sids, strings })
    }

    /// SID pointers, valid for as long as the set lives.
    fn sid_ptrs(&self) -> Vec<PSID> {
        self.sids
            .iter()
            .map(|bytes| bytes.as_ptr() as PSID)
            .collect()
    }

    /// SID strings, for evidence and tests.
    pub fn strings(&self) -> &[String] {
        &self.strings
    }
}

/// What a root grant had to do. `already_granted` means the marker ACE for
/// every capability SID was already present, so nothing was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantOutcome {
    pub root: PathBuf,
    pub already_granted: bool,
}

/// A live per-user AppContainer profile. Dropping it revokes the per-run
/// grants it recorded and deletes the profile; capability grants are stable by
/// design and outlive the run (see [`AppContainer::create_workspace`]).
pub struct AppContainer {
    name: String,
    sid: PSID,
    sid_string: String,
    capabilities: Vec<CapabilitySet>,
    grants: Vec<PathBuf>,
    /// What [`AppContainer::grant_null_device`] did, recorded for evidence:
    /// `None` until it runs, `Ok(GrantOutcome)` with either answer, or the
    /// error that stopped it.
    null_device: Option<Result<GrantOutcome, String>>,
}

impl AppContainer {
    /// Create a fresh per-user AppContainer profile. Works for standard,
    /// non-elevated accounts.
    pub fn create() -> Result<Self, String> {
        let name = format!("Bollo.Sandbox.{}.{}", std::process::id(), nonce());
        let name_wide = wide(&name);
        let mut sid: PSID = std::ptr::null_mut();
        let hresult = unsafe {
            CreateAppContainerProfile(
                name_wide.as_ptr(),
                wide("Bollo sandbox container").as_ptr(),
                wide("Bollo AppContainer sandbox").as_ptr(),
                std::ptr::null(),
                0,
                &mut sid,
            )
        };
        if hresult < 0 {
            return Err(format!("CreateAppContainerProfile: 0x{hresult:08x}"));
        }
        let sid_string = sid_to_string(sid)
            .ok_or_else(|| "AppContainer SID could not be formatted".to_string())?;
        Ok(Self {
            name,
            sid,
            sid_string,
            capabilities: Vec::new(),
            grants: Vec::new(),
            null_device: None,
        })
    }

    /// Create the container used for workspace-mode runs: a fresh per-run
    /// AppContainer profile (the containment identity, which carries no grants
    /// of its own) plus two stable derived capabilities that do carry them.
    ///
    /// * the shared toolchain capability ([`TOOLCHAIN_CAPABILITY`]): read
    ///   grants on the Rust toolchain and package caches, so a contained
    ///   `cargo`/`rustc` can build. Credential files inside those trees stay
    ///   unreadable (see [`AppContainer::deny_toolchain_read`]).
    /// * a per-workspace capability derived from the canonical workspace path:
    ///   modify inside that workspace only. Two workspaces never share a
    ///   capability, so a container for one can never reach the other's grant.
    pub fn create_workspace(workspace: &Path) -> Result<Self, String> {
        let canonical = std::fs::canonicalize(workspace)
            .map_err(|err| format!("canonicalize {}: {err}", workspace.display()))?;
        let mut container = Self::create()?;
        container
            .capabilities
            .push(CapabilitySet::derive(TOOLCHAIN_CAPABILITY)?);
        container
            .capabilities
            .push(CapabilitySet::derive(&workspace_capability_name(
                &canonical,
            ))?);
        // Recorded rather than fatal: a container the NUL device refuses can
        // still be built and run, it just cannot spawn a child that asks for
        // null stdio. The workspace tests assert the recorded answer instead of
        // leaving the difference to be guessed from a contained tool's error.
        container.null_device = Some(container.grant_null_device());
        Ok(container)
    }

    pub fn sid_string(&self) -> &str {
        &self.sid_string
    }

    /// Capability SID strings this container carries, for evidence and tests.
    pub fn capability_strings(&self) -> Vec<String> {
        self.capabilities
            .iter()
            .flat_map(|set| set.strings().iter().cloned())
            .collect()
    }

    /// Make the NUL device reachable from inside the container, once, and
    /// report what that took.
    ///
    /// [`NUL_DEVICE`] is not a tree like a toolchain grant: it is where Rust's
    /// std sends a child's stdin when a spawn asks for null stdio, and the open
    /// is issued by the *contained* process when it spawns a child of its own.
    /// A host whose device DACL names an Application Packages SID (the Windows
    /// 11 default) needs nothing. A host whose DACL names only the machine's own
    /// principals gets one non-inheritable ACE for this container's per-run SID,
    /// recorded in `grants` and revoked on drop. The device discards writes and
    /// reports end-of-file, so the ACE carries no data and no authority.
    ///
    /// Extending a device DACL needs `WRITE_DAC` on it, so a host that both
    /// denies Application Packages and runs the harness unelevated cannot be
    /// contained and buildable at once; it is named here instead of surfacing
    /// later as an opaque `Access is denied` from a contained `cargo`.
    pub fn grant_null_device(&mut self) -> Result<GrantOutcome, String> {
        let outcome = ensure_null_access(Path::new(NUL_DEVICE), self.sid)?;
        if !outcome.already_granted {
            self.grants.push(outcome.root.clone());
        }
        Ok(outcome)
    }

    /// What [`AppContainer::grant_null_device`] did, or the error that stopped
    /// it. `None` until it runs: [`AppContainer::create_workspace`] runs it,
    /// [`AppContainer::create`] does not.
    pub fn null_device(&self) -> Option<&Result<GrantOutcome, String>> {
        self.null_device.as_ref()
    }

    fn capability(&self, index: usize) -> Result<&CapabilitySet, String> {
        self.capabilities.get(index).ok_or_else(|| {
            "container has no workspace capabilities; use create_workspace".to_string()
        })
    }

    /// Grant the host toolchain tree read+execute to the shared toolchain
    /// capability. One inheritable ACE on the root is enough: Windows
    /// propagates it to every existing descendant (measured: recursive, at
    /// native speed), and objects created later inherit from their parent, so
    /// no tree walk is needed. A root that already carries the marker ACE is
    /// left untouched, which is what makes the stable capability cheap on
    /// every run after the first.
    ///
    /// Objects whose DACL is protected (`icacls /inheritance:d`) do not accept
    /// inherited ACEs and stay ungranted; that is a documented boundary, not a
    /// silent failure of containment.
    pub fn grant_toolchain_read(&mut self, root: &Path) -> Result<GrantOutcome, String> {
        let sids = self.capability(0)?.sid_ptrs();
        grant_root(root, &sids, READ_EXECUTE)
    }

    /// Grant read+execute on one file to the shared toolchain capability
    /// (cargo's `config.toml`: a file, not a tree, and not a credential store).
    pub fn grant_toolchain_read_file(&mut self, file: &Path) -> Result<(), String> {
        if !file.is_file() {
            return Ok(());
        }
        for sid in self.capability(0)?.sid_ptrs() {
            set_access(file, sid, GRANT_ACCESS, READ_EXECUTE, NO_INHERITANCE)?;
        }
        Ok(())
    }

    /// Grant one workspace tree read/write/execute (plus delete) to this
    /// workspace's capability, so contained builds can read existing sources
    /// and update an existing `target/` directory. Like the toolchain grant,
    /// this is one inheritable root ACE: propagation covers what already
    /// exists, inheritance covers what comes later.
    pub fn grant_workspace_modify(&mut self, root: &Path) -> Result<GrantOutcome, String> {
        let sids = self.capability(1)?.sid_ptrs();
        grant_root(root, &sids, MODIFY_RIGHTS)
    }

    /// Grant read/write/execute (inherited) on `path` to the container's own
    /// per-run SID, so contained children can work there. Recorded for revoke
    /// on drop. Workspace-mode runs use the capability grants above instead.
    pub fn grant_modify(&mut self, path: &Path) -> Result<(), String> {
        set_access(
            path,
            self.sid,
            GRANT_ACCESS,
            MODIFY_RIGHTS,
            SUB_CONTAINERS_AND_OBJECTS_INHERIT,
        )?;
        self.grants.push(path.to_path_buf());
        Ok(())
    }
}

impl ChildSandbox for AppContainer {
    /// Run a brokered child inside the container with pipe capture, filtered
    /// environment, deadline enforcement and bounded reaping. Semantics mirror
    /// [`crate::process::run_with_stdin`]; only the containment differs.
    fn run_with_stdin(&self, request: &ExecRequest, stdin_bytes: Option<&[u8]>) -> ExecOutcome {
        let sid_attributes: Vec<SID_AND_ATTRIBUTES> = self
            .capabilities
            .iter()
            .flat_map(|set| set.sid_ptrs())
            .map(|sid| SID_AND_ATTRIBUTES {
                Sid: sid,
                Attributes: SE_GROUP_ENABLED as u32,
            })
            .collect();
        run_in_container(self.sid, &sid_attributes, request, stdin_bytes)
    }
}

impl Drop for AppContainer {
    fn drop(&mut self) {
        for path in std::mem::take(&mut self.grants) {
            let _ = set_access(&path, self.sid, REVOKE_ACCESS, 0, NO_INHERITANCE);
        }
        unsafe {
            FreeSid(self.sid);
            let name = wide(&self.name);
            let _ = DeleteAppContainerProfile(name.as_ptr());
        }
    }
}

/// Executed mechanism check on temp roots (used by the platform probe and by
/// tests). Create -> grant -> contained writes -> contained network attempt;
/// everything is torn down afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainmentReport {
    pub write_inside_ok: bool,
    pub write_outside_denied: bool,
    pub network_denied: bool,
    pub detail: String,
}

pub fn selfcheck_containment() -> Result<ContainmentReport, String> {
    // One executed check per process: the mechanism cannot change mid-process,
    // and callers (composition roots, API test suites) may probe many times.
    static SELFCHECK: std::sync::OnceLock<Result<ContainmentReport, String>> =
        std::sync::OnceLock::new();
    SELFCHECK.get_or_init(run_selfcheck).clone()
}

fn run_selfcheck() -> Result<ContainmentReport, String> {
    let system_root =
        std::env::var("SystemRoot").map_err(|_| "SystemRoot is not set".to_string())?;
    let cmd = PathBuf::from(system_root).join("System32").join("cmd.exe");
    if !cmd.is_file() {
        return Err(format!("{} not found", cmd.display()));
    }
    // Unique per call: concurrent checks (e.g. two probes in one process) must
    // not share roots or clean each other up mid-run.
    let root = std::env::temp_dir().join(format!(
        "bollo-appcontainer-check-{}-{}",
        std::process::id(),
        nonce()
    ));
    let granted = root.join("granted");
    let outside = root.join("outside");
    std::fs::create_dir_all(&granted)
        .map_err(|err| format!("create {}: {err}", granted.display()))?;
    std::fs::create_dir_all(&outside)
        .map_err(|err| format!("create {}: {err}", outside.display()))?;

    let mut container = AppContainer::create()?;
    container.grant_modify(&granted)?;

    let inside_target = granted.join("inside.txt");
    let inside = container.run(&ExecRequest {
        argv: vec![
            cmd.display().to_string(),
            "/C".into(),
            "echo".into(),
            "ok>".into(),
            inside_target.display().to_string(),
        ],
        cwd: granted.clone(),
        timeout: Duration::from_secs(15),
        env: BTreeMap::new(),
        max_output_bytes: 4096,
    });
    let write_inside_ok = inside.succeeded() && inside_target.is_file();

    let outside_target = outside.join("outside.txt");
    let outside_run = container.run(&ExecRequest {
        argv: vec![
            cmd.display().to_string(),
            "/C".into(),
            "echo".into(),
            "no>".into(),
            outside_target.display().to_string(),
        ],
        cwd: granted.clone(),
        timeout: Duration::from_secs(15),
        env: BTreeMap::new(),
        max_output_bytes: 4096,
    });
    let write_outside_denied = !outside_run.succeeded() && !outside_target.exists();

    let (network_denied, network_detail) = match find_on_path("curl.exe") {
        Some(curl) => {
            let listener = std::net::TcpListener::bind("127.0.0.1:0")
                .map_err(|err| format!("loopback listener: {err}"))?;
            let port = listener
                .local_addr()
                .map_err(|err| format!("loopback listener address: {err}"))?
                .port();
            std::thread::spawn(move || loop {
                let _ = listener.accept();
            });
            let address = format!("127.0.0.1:{port}");
            let control_ok = address
                .parse()
                .ok()
                .and_then(|addr| {
                    std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(1)).ok()
                })
                .is_some();
            let curl_run = container.run(&ExecRequest {
                argv: vec![
                    curl,
                    "--connect-timeout".into(),
                    "0.5".into(),
                    "--max-time".into(),
                    "1".into(),
                    "-s".into(),
                    "-o".into(),
                    granted.join("curl-body.txt").display().to_string(),
                    format!("http://{address}/"),
                ],
                cwd: granted.clone(),
                timeout: Duration::from_secs(10),
                env: BTreeMap::new(),
                max_output_bytes: 4096,
            });
            if control_ok {
                (
                    !curl_run.succeeded(),
                    format!(
                        "loopback control ok, contained curl exit={:?}",
                        curl_run.exit_code
                    ),
                )
            } else {
                (false, "loopback control failed (inconclusive)".to_string())
            }
        }
        None => (
            false,
            "curl.exe not found; network denial not verified".to_string(),
        ),
    };

    let inside_note = format!(
        "inside(status={:?} exit={:?} file={} len={:?} stderr={:?})",
        inside.status,
        inside.exit_code,
        inside_target.is_file(),
        std::fs::metadata(&inside_target).map(|meta| meta.len()),
        inside.stderr.trim()
    );
    let outside_note = format!(
        "outside(status={:?} exit={:?} file={})",
        outside_run.status,
        outside_run.exit_code,
        outside_target.exists()
    );
    drop(container);
    let _ = std::fs::remove_dir_all(&root);
    let detail = format!(
        "write_inside={write_inside_ok} [{inside_note}]; write_outside_denied={write_outside_denied} [{outside_note}]; network_denied={network_denied} ({network_detail})"
    );
    Ok(ContainmentReport {
        write_inside_ok,
        write_outside_denied,
        network_denied,
        detail,
    })
}

fn run_in_container(
    sid: PSID,
    sid_attributes: &[SID_AND_ATTRIBUTES],
    request: &ExecRequest,
    stdin_bytes: Option<&[u8]>,
) -> ExecOutcome {
    if request.argv.is_empty() {
        return ExecOutcome::failed_to_start("argv must not be empty".into());
    }
    let started = Instant::now();
    unsafe {
        // A job object is the kill switch over the whole child tree. Closing
        // the job (`KILL_ON_JOB_CLOSE`) reaps any grandchild the direct child
        // leaves behind, and a deadline terminates the tree instead of only
        // the process the broker spawned.
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return ExecOutcome::failed_to_start(format!("CreateJobObjectW: {}", GetLastError()));
        }
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) == 0
        {
            let error = GetLastError();
            CloseHandle(job);
            return ExecOutcome::failed_to_start(format!("SetInformationJobObject: {error}"));
        }

        let mut out_read: HANDLE = std::ptr::null_mut();
        let mut out_write: HANDLE = std::ptr::null_mut();
        let mut err_read: HANDLE = std::ptr::null_mut();
        let mut err_write: HANDLE = std::ptr::null_mut();
        let mut in_read: HANDLE = std::ptr::null_mut();
        let mut in_write: HANDLE = std::ptr::null_mut();
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: 1,
        };
        let pipes = [
            CreatePipe(&mut out_read, &mut out_write, &attributes, 0),
            CreatePipe(&mut err_read, &mut err_write, &attributes, 0),
            CreatePipe(&mut in_read, &mut in_write, &attributes, 0),
        ];
        if pipes.contains(&0) {
            for handle in [out_read, out_write, err_read, err_write, in_read, in_write] {
                if !handle.is_null() {
                    CloseHandle(handle);
                }
            }
            CloseHandle(job);
            return ExecOutcome::failed_to_start(format!("CreatePipe: {}", GetLastError()));
        }
        // Parent-side ends must not leak into the child.
        SetHandleInformation(out_read, HANDLE_FLAG_INHERIT, 0);
        SetHandleInformation(err_read, HANDLE_FLAG_INHERIT, 0);
        SetHandleInformation(in_write, HANDLE_FLAG_INHERIT, 0);

        // Capability SIDs ride along in the token; their grants are what make
        // the toolchain and the workspace reachable. The array must outlive
        // CreateProcessW, so it is owned here for the whole call.
        let mut capability_attributes: Vec<SID_AND_ATTRIBUTES> = sid_attributes.to_vec();
        let mut caps = SECURITY_CAPABILITIES {
            AppContainerSid: sid,
            Capabilities: if capability_attributes.is_empty() {
                std::ptr::null_mut()
            } else {
                capability_attributes.as_mut_ptr()
            },
            CapabilityCount: capability_attributes.len() as u32,
            Reserved: 0,
        };
        // Two attributes: the AppContainer security capabilities and the
        // explicit list of handles that may cross into the child.
        let mut size: usize = 0;
        InitializeProcThreadAttributeList(std::ptr::null_mut(), 2, 0, &mut size);
        if size == 0 {
            close_all(&[out_read, out_write, err_read, err_write, in_read, in_write]);
            CloseHandle(job);
            return ExecOutcome::failed_to_start(format!(
                "attribute list size: {}",
                GetLastError()
            ));
        }
        let mut list_memory = vec![0u8; size];
        let list = list_memory.as_mut_ptr() as *mut c_void;
        if InitializeProcThreadAttributeList(list, 2, 0, &mut size) == 0 {
            close_all(&[out_read, out_write, err_read, err_write, in_read, in_write]);
            CloseHandle(job);
            return ExecOutcome::failed_to_start(format!(
                "InitializeProcThreadAttributeList: {}",
                GetLastError()
            ));
        }
        let updated = UpdateProcThreadAttribute(
            list,
            0,
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
            &mut caps as *mut _ as *const c_void,
            std::mem::size_of::<SECURITY_CAPABILITIES>(),
            std::ptr::null_mut(),
            std::ptr::null(),
        );
        if updated == 0 {
            DeleteProcThreadAttributeList(list);
            close_all(&[out_read, out_write, err_read, err_write, in_read, in_write]);
            CloseHandle(job);
            return ExecOutcome::failed_to_start(format!(
                "UpdateProcThreadAttribute: {}",
                GetLastError()
            ));
        }
        // Restrict inheritance to exactly the three stdio handles. Without an
        // explicit list, `bInheritHandles = TRUE` would pass every inheritable
        // handle this process holds into the container, including handles to
        // files outside the grant.
        let stdio_handles = [out_write, err_write, in_read];
        let listed = UpdateProcThreadAttribute(
            list,
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            stdio_handles.as_ptr() as *const c_void,
            std::mem::size_of_val(&stdio_handles),
            std::ptr::null_mut(),
            std::ptr::null(),
        );
        if listed == 0 {
            DeleteProcThreadAttributeList(list);
            close_all(&[out_read, out_write, err_read, err_write, in_read, in_write]);
            CloseHandle(job);
            return ExecOutcome::failed_to_start(format!(
                "UpdateProcThreadAttribute(handle list): {}",
                GetLastError()
            ));
        }

        let mut startup: STARTUPINFOEXW = std::mem::zeroed();
        startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = in_read;
        startup.StartupInfo.hStdOutput = out_write;
        startup.StartupInfo.hStdError = err_write;
        startup.lpAttributeList = list;

        let environment = environment_block(request);
        let mut line = command_line(&request.argv);
        let cwd = wide(&filesystem_path(&request.cwd).to_string_lossy());
        let mut info: PROCESS_INFORMATION = std::mem::zeroed();
        let created = CreateProcessW(
            std::ptr::null(),
            line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            EXTENDED_STARTUPINFO_PRESENT
                | CREATE_NO_WINDOW
                | CREATE_UNICODE_ENVIRONMENT
                | CREATE_SUSPENDED,
            environment.as_ptr() as *const c_void,
            cwd.as_ptr(),
            &startup.StartupInfo,
            &mut info,
        );
        DeleteProcThreadAttributeList(list);
        if created == 0 {
            close_all(&[out_read, out_write, err_read, err_write, in_read, in_write]);
            CloseHandle(job);
            return ExecOutcome::failed_to_start(format!("CreateProcessW: {}", GetLastError()));
        }
        // The child starts suspended so it is inside the job before it can
        // run: every descendant it spawns is then covered by the tree kill.
        if AssignProcessToJobObject(job, info.hProcess) == 0 {
            let error = GetLastError();
            TerminateProcess(info.hProcess, 1);
            CloseHandle(info.hThread);
            CloseHandle(info.hProcess);
            close_all(&[out_read, out_write, err_read, err_write, in_read, in_write]);
            CloseHandle(job);
            return ExecOutcome::failed_to_start(format!(
                "AssignProcessToJobObject: {error}; refusing to run an unkillable child tree"
            ));
        }
        if ResumeThread(info.hThread) == u32::MAX {
            let error = GetLastError();
            TerminateJobObject(job, 1);
            CloseHandle(info.hThread);
            CloseHandle(info.hProcess);
            close_all(&[out_read, out_write, err_read, err_write, in_read, in_write]);
            CloseHandle(job);
            return ExecOutcome::failed_to_start(format!("ResumeThread: {error}"));
        }
        // The child owns its copies; close the parent-side duplicates.
        CloseHandle(out_write);
        CloseHandle(err_write);
        CloseHandle(in_read);
        // The stdin write end stays open only when there is a payload: the
        // writer thread owns it and dropping the handle closes the pipe, so
        // the child sees EOF. With no payload the child sees EOF immediately.
        let stdin_writer = match stdin_bytes {
            Some(bytes) => {
                let file = File::from_raw_handle(in_write as RawHandle);
                let owned = bytes.to_vec();
                Some(std::thread::spawn(move || {
                    use std::io::Write;
                    let mut file = file;
                    let _ = file.write_all(&owned);
                    let _ = file.flush();
                }))
            }
            None => {
                CloseHandle(in_write);
                None
            }
        };

        let out_file = File::from_raw_handle(out_read as RawHandle);
        let err_file = File::from_raw_handle(err_read as RawHandle);
        let cap = request.max_output_bytes;
        let out_thread = std::thread::spawn(move || drain(out_file, cap));
        let err_thread = std::thread::spawn(move || drain(err_file, cap));

        let deadline = started + request.timeout;
        let mut timed_out = false;
        loop {
            match WaitForSingleObject(info.hProcess, 10) {
                WAIT_OBJECT_0 => break,
                WAIT_TIMEOUT => {
                    if Instant::now() >= deadline {
                        timed_out = true;
                        // Kill the whole tree, not just the direct child.
                        TerminateJobObject(job, 1);
                        break;
                    }
                }
                _ => {
                    timed_out = Instant::now() >= deadline;
                    break;
                }
            }
        }
        let reap_deadline = Instant::now() + Duration::from_secs(2);
        let mut reaped = false;
        loop {
            match WaitForSingleObject(info.hProcess, 10) {
                WAIT_OBJECT_0 => {
                    reaped = true;
                    break;
                }
                WAIT_TIMEOUT => {
                    if Instant::now() >= reap_deadline {
                        break;
                    }
                }
                _ => break,
            }
        }
        let mut code: u32 = 0;
        let got_code = GetExitCodeProcess(info.hProcess, &mut code);
        CloseHandle(info.hThread);
        CloseHandle(info.hProcess);
        // The direct child is done. A descendant that inherited the stdio pipes
        // would otherwise keep the drains (and this run) open indefinitely, so
        // terminate whatever is left: a contained run does not leave background
        // children behind.
        TerminateJobObject(job, 1);
        let (stdout, out_truncated) = out_thread.join().unwrap_or_default();
        let (stderr, err_truncated) = err_thread.join().unwrap_or_default();
        if let Some(writer) = stdin_writer {
            let _ = writer.join();
        }
        // Closing the job reaps any grandchild still alive (`KILL_ON_JOB_CLOSE`)
        // so a contained run never leaves processes behind.
        CloseHandle(job);
        ExecOutcome {
            status: if timed_out {
                ExecStatus::TimedOut
            } else {
                ExecStatus::Exited
            },
            exit_code: if timed_out || got_code == 0 {
                None
            } else {
                Some(code as i32)
            },
            stdout,
            stderr,
            truncated: out_truncated || err_truncated,
            duration_ms: started.elapsed().as_millis() as u64,
            reaped,
        }
    }
}

fn close_all(handles: &[HANDLE]) {
    for handle in handles {
        if !handle.is_null() {
            unsafe {
                CloseHandle(*handle);
            }
        }
    }
}

/// Build the UTF-16 environment block: base allowlist, the host's discovered
/// build-tool environment, then explicit request overrides — never
/// credential-bearing variables.
///
/// The build-tool variables are what a Developer Command Prompt exports; a
/// contained child cannot discover MSVC on its own (the Visual Studio registry
/// views are unreadable inside the AppContainer), so without them rustc cannot
/// find a real linker and picks whatever `link.exe` is first on `PATH`.
fn environment_block(request: &ExecRequest) -> Vec<u16> {
    let mut env = base_environment();
    for (key, value) in &crate::sandbox::build_tool_environment().variables {
        overwrite(&mut env, key, value);
    }
    for (key, value) in &request.env {
        overwrite(&mut env, key, value);
    }
    let mut block: Vec<u16> = Vec::new();
    for (key, value) in env {
        block.extend(OsStr::new(&format!("{key}={value}")).encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}

/// Set `key` to `value`, removing an existing entry that differs only in case:
/// the OS compares environment keys case-insensitively, so keeping both would
/// leave the effective value unspecified.
fn overwrite(env: &mut BTreeMap<String, String>, key: &str, value: &str) {
    if let Some(existing) = env.keys().find(|k| k.eq_ignore_ascii_case(key)).cloned() {
        env.remove(&existing);
    }
    env.insert(key.to_string(), value.to_string());
}

fn command_line(argv: &[String]) -> Vec<u16> {
    let mut line = String::new();
    for (index, arg) in argv.iter().enumerate() {
        if index > 0 {
            line.push(' ');
        }
        append_quoted(&mut line, arg);
    }
    wide(&line)
}

/// Windows argv quoting: backslashes are literal except before a quote, where
/// they must be doubled; a quote itself is escaped.
fn append_quoted(line: &mut String, arg: &str) {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        line.push_str(arg);
        return;
    }
    line.push('"');
    let mut backslashes = 0usize;
    for ch in arg.chars() {
        match ch {
            '\\' => {
                backslashes += 1;
                line.push('\\');
            }
            '"' => {
                for _ in 0..=backslashes {
                    line.push('\\');
                }
                backslashes = 0;
                line.push('"');
            }
            _ => {
                backslashes = 0;
                line.push(ch);
            }
        }
    }
    for _ in 0..backslashes {
        line.push('\\');
    }
    line.push('"');
}

fn wide(text: &str) -> Vec<u16> {
    OsStr::new(text).encode_wide().chain(Some(0)).collect()
}

fn nonce() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

fn sid_to_string(sid: PSID) -> Option<String> {
    let mut raw: *mut u16 = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut raw) } == 0 || raw.is_null() {
        return None;
    }
    let mut len = 0usize;
    while unsafe { *raw.add(len) } != 0 {
        len += 1;
    }
    let text = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(raw, len) });
    unsafe {
        LocalFree(raw as *mut c_void);
    }
    Some(text)
}

/// Grant or revoke one SID on `path`, merging with the object's current DACL
/// so unrelated ACEs (the owner's, SYSTEM's, inherited ones) survive.
/// Replacing the whole DACL would strip them on a real workspace.
fn set_access(path: &Path, sid: PSID, mode: i32, rights: u32, flags: u32) -> Result<(), String> {
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: rights,
        grfAccessMode: mode,
        grfInheritance: flags,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: sid as *mut u16,
        },
    };
    let path_wide = wide(&path.to_string_lossy());
    let mut old_dacl: *mut ACL = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let fetched = unsafe {
        GetNamedSecurityInfoW(
            path_wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut old_dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if fetched != 0 {
        return Err(format!("GetNamedSecurityInfoW: {fetched}"));
    }
    let mut new_acl: *mut ACL = std::ptr::null_mut();
    let built = unsafe { SetEntriesInAclW(1, &entry, old_dacl, &mut new_acl) };
    if built != 0 {
        unsafe {
            LocalFree(descriptor as *mut c_void);
        }
        return Err(format!("SetEntriesInAclW: {built}"));
    }
    let applied = unsafe {
        SetNamedSecurityInfoW(
            path_wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            new_acl,
            std::ptr::null(),
        )
    };
    unsafe {
        LocalFree(new_acl as *mut c_void);
        LocalFree(descriptor as *mut c_void);
    }
    if applied != 0 {
        return Err(format!("SetNamedSecurityInfoW: {applied}"));
    }
    Ok(())
}

/// Stable capability name for one canonical workspace. FNV-1a is spelled out
/// here because `DefaultHasher` is allowed to change between Rust releases,
/// which would orphan every previous grant for the same workspace.
fn workspace_capability_name(workspace: &Path) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in workspace.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("bollo.workspace.{hash:016x}.modify.v1")
}

/// Grant `rights` on `root` to every SID in `sids`: one inheritable ACE per
/// SID, no descendants touched by hand. The system propagates the ACE to the
/// existing tree and inheritance covers later additions. If the root already
/// carries the marker ACEs, nothing is written.
fn grant_root(root: &Path, sids: &[PSID], rights: u32) -> Result<GrantOutcome, String> {
    let metadata =
        std::fs::symlink_metadata(root).map_err(|err| format!("{}: {err}", root.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "{} is a reparse point; refusing to grant through it",
            root.display()
        ));
    }
    if !metadata.is_dir() {
        return Err(format!("{} is not a directory", root.display()));
    }
    if acl_grants_all(root, sids, rights, true)? {
        return Ok(GrantOutcome {
            root: root.to_path_buf(),
            already_granted: true,
        });
    }
    for sid in sids {
        set_access(
            root,
            *sid,
            GRANT_ACCESS,
            rights,
            SUB_CONTAINERS_AND_OBJECTS_INHERIT,
        )?;
    }
    Ok(GrantOutcome {
        root: root.to_path_buf(),
        already_granted: false,
    })
}

/// Ensure `device` admits the container's token for null stdio: check first,
/// and write exactly one non-inheritable ACE for `sid` when nothing covers it.
fn ensure_null_access(device: &Path, sid: PSID) -> Result<GrantOutcome, String> {
    if null_access_granted(device, sid)? {
        return Ok(GrantOutcome {
            root: device.to_path_buf(),
            already_granted: true,
        });
    }
    set_access(device, sid, GRANT_ACCESS, NUL_RIGHTS, NO_INHERITANCE).map_err(|err| {
        format!(
            "{} admits no Application Packages SID and could not be granted to this \
             container: {err} (extending a device DACL needs WRITE_DAC, so the harness \
             must run elevated on this host, or {NUL_DEVICE} must grant {} itself)",
            device.display(),
            ANY_PACKAGE_SIDS[0],
        )
    })?;
    Ok(GrantOutcome {
        root: device.to_path_buf(),
        already_granted: false,
    })
}

/// True when the container's token already reaches `path` for null stdio:
/// either this container's own SID or a well-known Application Packages SID
/// carries the rights on the object itself.
fn null_access_granted(path: &Path, sid: PSID) -> Result<bool, String> {
    let mut trustees: Vec<Vec<u8>> = vec![sid_bytes(sid)];
    for text in ANY_PACKAGE_SIDS {
        trustees.push(sid_from_string(text)?);
    }
    read_dacl(path, |dacl| {
        trustees.iter().any(|bytes| unsafe {
            acl_contains(
                dacl,
                bytes.as_ptr() as PSID,
                ACCESS_ALLOWED_ACE_TYPE as u8,
                NUL_RIGHTS,
                false,
            )
        })
    })
}

/// Copy a SID's bytes, so the borrow of the caller's SID stays out of the DACL
/// closure.
fn sid_bytes(sid: PSID) -> Vec<u8> {
    let length = unsafe { GetLengthSid(sid) } as usize;
    unsafe { std::slice::from_raw_parts(sid as *const u8, length).to_vec() }
}

/// A SID from its string form. `ConvertStringSidToSidW` allocates the SID with
/// `LocalAlloc`; the bytes are copied out and the allocation freed here.
fn sid_from_string(text: &str) -> Result<Vec<u8>, String> {
    let text_wide = wide(text);
    let mut sid: PSID = std::ptr::null_mut();
    let converted = unsafe { ConvertStringSidToSidW(text_wide.as_ptr(), &mut sid) };
    if converted == 0 || sid.is_null() {
        return Err(format!("ConvertStringSidToSidW({text}): {}", unsafe {
            GetLastError()
        }));
    }
    let bytes = sid_bytes(sid);
    unsafe {
        LocalFree(sid as *mut c_void);
    }
    Ok(bytes)
}

/// Read `path`'s DACL and hand it to `inspect`; the security descriptor is
/// freed afterwards, so the pointer must not escape.
fn read_dacl<T>(path: &Path, inspect: impl FnOnce(*const ACL) -> T) -> Result<T, String> {
    let path_wide = wide(&path.to_string_lossy());
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let fetched = unsafe {
        GetNamedSecurityInfoW(
            path_wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if fetched != 0 {
        return Err(format!(
            "GetNamedSecurityInfoW({}): {fetched}",
            path.display()
        ));
    }
    let result = inspect(dacl);
    unsafe { LocalFree(descriptor as *mut c_void) };
    Ok(result)
}

/// True when every SID in `sids` already has a matching grant on `path`.
fn acl_grants_all(
    path: &Path,
    sids: &[PSID],
    rights: u32,
    require_inheritance: bool,
) -> Result<bool, String> {
    read_dacl(path, |dacl| {
        sids.iter().all(|sid| unsafe {
            acl_contains(
                dacl,
                *sid,
                ACCESS_ALLOWED_ACE_TYPE as u8,
                rights,
                require_inheritance,
            )
        })
    })
}

/// Inspect one DACL for an ACE of `ace_type` whose mask covers `rights` and
/// whose trustee is `sid`. With `require_inheritance`, the ACE must also be
/// inheritable by both files and subdirectories (and apply to the object
/// itself, not be inherit-only).
unsafe fn acl_contains(
    dacl: *const ACL,
    sid: PSID,
    ace_type: u8,
    rights: u32,
    require_inheritance: bool,
) -> bool {
    if dacl.is_null() {
        return false;
    }
    let mut info: ACL_SIZE_INFORMATION = std::mem::zeroed();
    if GetAclInformation(
        dacl,
        &mut info as *mut _ as *mut c_void,
        std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
        AclSizeInformation,
    ) == 0
    {
        return false;
    }
    for index in 0..info.AceCount {
        let mut ace: *mut c_void = std::ptr::null_mut();
        if GetAce(dacl, index, &mut ace) == 0 || ace.is_null() {
            continue;
        }
        let header = ace as *const ACE_HEADER;
        if (*header).AceType != ace_type {
            continue;
        }
        // ACCESS_ALLOWED_ACE and ACCESS_DENIED_ACE share a layout: header,
        // access mask, then the variable-length SID.
        let entry = ace as *const ACCESS_ALLOWED_ACE;
        if (*entry).Mask & rights != rights {
            continue;
        }
        let entry_sid = std::ptr::addr_of!((*entry).SidStart) as PSID;
        if EqualSid(sid, entry_sid) == 0 {
            continue;
        }
        if require_inheritance {
            let flags = (*header).AceFlags;
            let inherit = (CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE) as u8;
            if flags & inherit != inherit || flags & INHERIT_ONLY_ACE as u8 != 0 {
                continue;
            }
        }
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_derivation_is_stable_and_workspace_specific() {
        let first = CapabilitySet::derive(TOOLCHAIN_CAPABILITY).unwrap();
        let second = CapabilitySet::derive(TOOLCHAIN_CAPABILITY).unwrap();
        assert_eq!(first.strings(), second.strings());
        assert!(first
            .strings()
            .iter()
            .all(|sid| sid.starts_with("S-1-15-3-")));
        assert_eq!(
            workspace_capability_name(Path::new(r"C:\projects\one")),
            workspace_capability_name(Path::new(r"C:\projects\one"))
        );
        assert_ne!(
            workspace_capability_name(Path::new(r"C:\projects\one")),
            workspace_capability_name(Path::new(r"C:\projects\two"))
        );
    }

    fn system_cmd() -> PathBuf {
        PathBuf::from(std::env::var("SystemRoot").unwrap())
            .join("System32")
            .join("cmd.exe")
    }

    /// Console-like argv words after `/C`: the broker must not wrap the whole
    /// command in one quoted argument, because cmd's own parser does not use
    /// CRT quote rules and would misread embedded quotes.
    fn request(cmd: &Path, words: &[&str], cwd: &Path, timeout_secs: u64) -> ExecRequest {
        let mut argv = vec![cmd.display().to_string(), "/C".into()];
        argv.extend(words.iter().map(|word| word.to_string()));
        ExecRequest {
            argv,
            cwd: cwd.to_path_buf(),
            timeout: Duration::from_secs(timeout_secs),
            env: BTreeMap::new(),
            max_output_bytes: 65536,
        }
    }

    struct Roots {
        root: PathBuf,
        granted: PathBuf,
        outside: PathBuf,
    }

    impl Roots {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir()
                .join(format!("bollo-sandbox-test-{}-{tag}", std::process::id()));
            let granted = root.join("granted");
            let outside = root.join("outside");
            std::fs::create_dir_all(&granted).unwrap();
            std::fs::create_dir_all(&outside).unwrap();
            Self {
                root,
                granted,
                outside,
            }
        }
    }

    impl Drop for Roots {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn contained_child_is_confined_to_granted_roots() {
        let roots = Roots::new("confined");
        let mut container = AppContainer::create().unwrap();
        assert!(container.sid_string().starts_with("S-1-15-2-"));
        container.grant_modify(&roots.granted).unwrap();
        let cmd = system_cmd();

        let inside = roots.granted.join("in.txt");
        let inside_run = container.run(&request(
            &cmd,
            &["echo", "ok>", &inside.display().to_string()],
            &roots.granted,
            15,
        ));
        assert_eq!(
            inside_run.status,
            ExecStatus::Exited,
            "stderr: {}",
            inside_run.stderr
        );
        assert_eq!(
            inside_run.exit_code,
            Some(0),
            "stderr: {}",
            inside_run.stderr
        );
        assert!(inside.is_file());

        let outside_file = roots.outside.join("out.txt");
        let outside_run = container.run(&request(
            &cmd,
            &["echo", "no>", &outside_file.display().to_string()],
            &roots.granted,
            15,
        ));
        assert_eq!(outside_run.status, ExecStatus::Exited);
        assert_ne!(outside_run.exit_code, Some(0));
        assert!(
            !outside_file.exists(),
            "contained child wrote outside the granted root"
        );
    }

    #[test]
    fn contained_child_output_is_captured_and_bounded() {
        let roots = Roots::new("output");
        let mut container = AppContainer::create().unwrap();
        container.grant_modify(&roots.granted).unwrap();
        let cmd = system_cmd();

        let run = container.run(&request(
            &cmd,
            &["echo", "hello-from-container"],
            &roots.granted,
            15,
        ));
        assert_eq!(run.status, ExecStatus::Exited);
        assert!(run.stdout.contains("hello-from-container"));
        assert!(run.reaped);

        let mut bounded = request(
            &cmd,
            &[
                "for",
                "/L",
                "%i",
                "in",
                "(1,1,2000)",
                "do",
                "@echo",
                "0123456789",
            ],
            &roots.granted,
            15,
        );
        bounded.max_output_bytes = 256;
        let run = container.run(&bounded);
        assert!(run.truncated);
        assert!(run.stdout.len() <= 256);
    }

    #[test]
    fn contained_child_timeout_is_enforced_and_reported() {
        let roots = Roots::new("timeout");
        let mut container = AppContainer::create().unwrap();
        container.grant_modify(&roots.granted).unwrap();
        let cmd = system_cmd();

        let run = container.run(&request(
            &cmd,
            &["for", "/L", "%i", "in", "(1,1,200000000)", "do", "@rem"],
            &roots.granted,
            1,
        ));
        assert_eq!(run.status, ExecStatus::TimedOut);
        assert!(run.exit_code.is_none());
        assert!(run.reaped);
    }

    #[test]
    fn selfcheck_verifies_filesystem_and_network_containment() {
        let report = selfcheck_containment().expect("mechanism self-check");
        assert!(
            report.write_inside_ok && report.write_outside_denied,
            "{}",
            report.detail
        );
        if find_on_path("curl.exe").is_some() {
            assert!(report.network_denied, "{}", report.detail);
        }
    }

    #[test]
    fn contained_child_receives_stdin() {
        let roots = Roots::new("stdin");
        let mut container = AppContainer::create().unwrap();
        container.grant_modify(&roots.granted).unwrap();
        let cmd = system_cmd();

        // `more` echoes its stdin to stdout, so a successful round trip proves
        // the contained child received the hook-style payload.
        let run = container.run_with_stdin(
            &request(&cmd, &["more"], &roots.granted, 15),
            Some(b"hello-from-stdin\n"),
        );
        assert_eq!(run.status, ExecStatus::Exited, "stderr: {}", run.stderr);
        assert!(
            run.stdout.contains("hello-from-stdin"),
            "stdout: {:?} stderr: {:?}",
            run.stdout,
            run.stderr
        );
    }

    #[test]
    fn contained_child_resolves_relative_paths_in_the_broker_cwd() {
        let roots = Roots::new("relative");
        let mut container = AppContainer::create().unwrap();
        container.grant_modify(&roots.granted).unwrap();
        let cmd = system_cmd();
        // The broker receives canonical paths, which carry the Windows verbatim
        // prefix. cmd treats `\\?\` as an unsupported UNC current directory and
        // silently falls back to the Windows directory, so the broker must hand
        // children a usable form or relative writes land outside the grant.
        let canonical = std::fs::canonicalize(&roots.granted).unwrap();
        let run = container.run(&request(&cmd, &["echo", "rel>", "rel.txt"], &canonical, 15));
        assert_eq!(
            run.exit_code,
            Some(0),
            "stdout: {:?} stderr: {:?}",
            run.stdout,
            run.stderr
        );
        assert!(roots.granted.join("rel.txt").is_file());
    }

    #[test]
    fn revoke_preserves_other_grants() {
        let roots = Roots::new("merge");
        let mut first = AppContainer::create().unwrap();
        let mut second = AppContainer::create().unwrap();
        first.grant_modify(&roots.granted).unwrap();
        second.grant_modify(&roots.granted).unwrap();
        // Dropping the second container revokes only its own SID. The first
        // container's grant must survive the DACL edit: a replacing ACL write
        // would have erased it and this write would be denied.
        drop(second);
        let cmd = system_cmd();
        let inside = roots.granted.join("survivor.txt");
        let run = first.run(&request(
            &cmd,
            &["echo", "ok>", &inside.display().to_string()],
            &roots.granted,
            15,
        ));
        assert_eq!(run.exit_code, Some(0), "stderr: {}", run.stderr);
        assert!(inside.is_file());
    }

    /// Replace `path`'s DACL with one protected ACE for `trustee`, so the
    /// fixture carries exactly what the runner-measured device carries and
    /// nothing else: no inherited entry survives the write. The owner keeps
    /// `WRITE_DAC`, so the same write reverses it without elevation.
    fn set_protected_dacl(path: &Path, trustee: &str, mask: u32) -> Result<(), String> {
        let sid = sid_from_string(trustee)?;
        let entry = EXPLICIT_ACCESS_W {
            grfAccessPermissions: mask,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: NO_INHERITANCE,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: std::ptr::null_mut(),
                MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_UNKNOWN,
                ptstrName: sid.as_ptr() as *mut u16,
            },
        };
        let mut acl: *mut ACL = std::ptr::null_mut();
        let built = unsafe { SetEntriesInAclW(1, &entry, std::ptr::null(), &mut acl) };
        if built != 0 {
            return Err(format!("SetEntriesInAclW({trustee}): {built}"));
        }
        let path_wide = wide(&path.to_string_lossy());
        // `PROTECTED`: the replaced DACL inherits nothing, which is how a
        // fixture reproduces a device that grants no Application Packages.
        const PROTECTED: u32 = 0x8000_0000;
        let applied = unsafe {
            SetNamedSecurityInfoW(
                path_wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                acl,
                std::ptr::null(),
            )
        };
        unsafe { LocalFree(acl as *mut c_void) };
        if applied != 0 {
            return Err(format!("SetNamedSecurityInfoW({trustee}): {applied}"));
        }
        Ok(())
    }

    #[test]
    fn null_access_is_granted_when_no_application_packages_entry_covers_the_child() {
        let roots = Roots::new("null-grant");
        let container = AppContainer::create().unwrap();
        let target = roots.granted.join("null-target");
        std::fs::write(&target, b"placeholder").unwrap();
        // The runner-measured device shape: `Everyone` carries the rights and
        // the container still cannot open it, because a lowbox token is not
        // covered by an ACE that names `Everyone`.
        set_protected_dacl(&target, "S-1-1-0", NUL_RIGHTS | DELETE).unwrap();
        assert!(!null_access_granted(&target, container.sid).unwrap());

        let outcome = ensure_null_access(&target, container.sid).unwrap();
        assert!(!outcome.already_granted);
        assert_eq!(outcome.root, target);
        assert!(null_access_granted(&target, container.sid).unwrap());
        // The grant is one ACE for this container's own SID, and revoking it
        // (what `Drop` does) puts the object back to denying the container.
        assert!(container.sid_string().starts_with("S-1-15-2-"));
        set_access(&target, container.sid, REVOKE_ACCESS, 0, NO_INHERITANCE).unwrap();
        assert!(!null_access_granted(&target, container.sid).unwrap());
    }

    #[test]
    fn null_access_is_left_alone_when_application_packages_already_have_it() {
        let roots = Roots::new("null-already");
        let container = AppContainer::create().unwrap();
        let target = roots.granted.join("null-target");
        std::fs::write(&target, b"placeholder").unwrap();
        // This host's shape: the Application Packages SID carries the rights.
        set_protected_dacl(&target, "S-1-15-2-1", NUL_RIGHTS | DELETE).unwrap();

        let outcome = ensure_null_access(&target, container.sid).unwrap();
        assert!(outcome.already_granted);
        assert!(null_access_granted(&target, container.sid).unwrap());
        assert!(
            !read_dacl(&target, |dacl| unsafe {
                acl_contains(
                    dacl,
                    container.sid,
                    ACCESS_ALLOWED_ACE_TYPE as u8,
                    NUL_RIGHTS,
                    false,
                )
            })
            .unwrap(),
            "a covered container must not get an ACE of its own"
        );
    }

    #[test]
    fn workspace_container_records_its_null_device_answer() {
        let roots = Roots::new("null-record");
        let container = AppContainer::create_workspace(&roots.root).unwrap();
        let recorded = container
            .null_device()
            .expect("a workspace container records the NUL device check");
        println!("NUL device: {recorded:?}");
        assert!(
            recorded
                .as_ref()
                .map(|outcome| outcome.root == Path::new(NUL_DEVICE))
                .unwrap_or(false),
            "the record must name the device it checked: {recorded:?}"
        );
    }

    #[test]
    fn bare_container_leaves_the_null_device_unrecorded() {
        let mut container = AppContainer::create().unwrap();
        assert!(container.null_device().is_none());
        // The real device is still checkable on demand; the answer is returned
        // to the caller and only `create_workspace` keeps it.
        let answer = container.grant_null_device();
        println!("{NUL_DEVICE}: {answer:?}");
        assert!(container.null_device().is_none());
    }
}
