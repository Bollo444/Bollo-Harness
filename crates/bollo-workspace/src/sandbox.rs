//! Sandbox capability probing.
//!
//! Claims must be backed by an executed probe. When enforcement is unavailable
//! the probe reports `false` and the policy layer refuses `workspace_auto`
//! instead of silently running on the host.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use bollo_protocol::vocab::{Platform, SandboxCapabilities};

use crate::process::{run, ChildSandbox, ExecRequest, ExecStatus};

pub fn probe() -> SandboxCapabilities {
    match Platform::current() {
        Platform::Linux => probe_linux(),
        Platform::Macos => SandboxCapabilities {
            platform: Platform::Macos,
            filesystem_containment: false,
            network_denied: false,
            backend: "macOS beta: no validated sandbox backend in this build".into(),
        },
        Platform::Windows => probe_windows(),
        Platform::Other => SandboxCapabilities {
            platform: Platform::Other,
            filesystem_containment: false,
            network_denied: false,
            backend: "unsupported platform".into(),
        },
    }
}

/// Executed probe used where enforcement actually matters (workspace-mode
/// startup and `bollo doctor`): on Windows it runs the AppContainer containment
/// check and reports the measured flags. Containment is only reported when the
/// check passed *and* this build routes brokered children through the container
/// (see [`create_workspace_sandbox`]); otherwise the flags stay false and
/// workspace mode is refused.
pub fn verified_probe() -> SandboxCapabilities {
    match Platform::current() {
        Platform::Windows => verified_probe_windows(),
        _ => probe(),
    }
}

/// Host paths a contained `cargo`/`rustc` run must be able to read, and the
/// credential stores that must stay unreadable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolchainAccess {
    /// Directory trees granted read+execute to the shared toolchain
    /// capability: the Rust toolchain and the cargo binary/cache trees.
    pub read_roots: Vec<PathBuf>,
    /// Individual files granted read+execute: cargo's `config.toml`, which is a
    /// configuration file (not a credential store) that cargo refuses to build
    /// with when it cannot read it.
    pub read_files: Vec<PathBuf>,
    /// Credential stores (`.cargo/credentials.toml` and the legacy
    /// `credentials`). They are kept *outside* every granted root by
    /// construction, which is what keeps them unreadable: measured on Windows,
    /// a capability SID gets access through allow ACEs only, so a deny ACE does
    /// not override an inherited one. A store that would fall inside a granted
    /// root makes sandbox creation refuse the run (`credential_conflict`).
    pub excluded_files: Vec<PathBuf>,
}

impl ToolchainAccess {
    /// The first credential store that falls inside `root`, if any. Callers
    /// refuse the run rather than grant a tree that contains a store: the
    /// grant would otherwise make the store readable through inheritance.
    pub fn credential_conflict(&self, root: &Path) -> Option<&Path> {
        let mut prefix = normalized(root);
        if !prefix.is_empty() {
            prefix.push('\\');
        }
        self.excluded_files
            .iter()
            .find(|store| normalized(store).starts_with(&prefix))
            .map(PathBuf::as_path)
    }
}

/// Case-insensitive, separator-trimmed form for containment comparisons:
/// Windows path identity is case-insensitive, and a directory prefix must not
/// match a sibling whose name merely starts with the same text.
fn normalized(path: &Path) -> String {
    let mut text = path.to_string_lossy().replace('/', "\\").to_lowercase();
    while text.ends_with('\\') {
        text.pop();
    }
    text
}

/// Create the host enforcement sandbox for `workspace`, when the host needs one
/// and can provide it. Windows returns an AppContainer whose stable capability
/// grants carry read access to the host toolchain and modify access to the
/// canonical workspace; other platforms return `None`, leaving their existing
/// broker behavior unchanged.
///
/// Any grant failure is fatal: a run whose toolchain cannot be made readable is
/// refused rather than launched to fail later with a confusing access error.
pub fn create_workspace_sandbox(workspace: &Path) -> Result<Option<Box<dyn ChildSandbox>>, String> {
    #[cfg(windows)]
    {
        let access = toolchain_access();
        // Fail closed when a credential store would end up inside a granted
        // root (for example a CARGO_HOME that lives inside the workspace
        // itself): the grant would make the store readable, and a deny ACE
        // cannot take it back.
        for root in &access.read_roots {
            if let Some(store) = access.credential_conflict(root) {
                return Err(format!(
                    "credential store {} lies inside the toolchain grant {}; refusing workspace mode",
                    store.display(),
                    root.display()
                ));
            }
        }
        if let Some(store) = access.credential_conflict(workspace) {
            return Err(format!(
                "credential store {} lies inside the workspace; refusing workspace mode",
                store.display()
            ));
        }
        let mut container = crate::sandbox_win::AppContainer::create_workspace(workspace)?;
        for root in &access.read_roots {
            container
                .grant_toolchain_read(root)
                .map_err(|err| format!("toolchain read grant on {}: {err}", root.display()))?;
        }
        for file in &access.read_files {
            container
                .grant_toolchain_read_file(file)
                .map_err(|err| format!("toolchain read grant on {}: {err}", file.display()))?;
        }
        container
            .grant_workspace_modify(workspace)
            .map_err(|err| format!("workspace modify grant on {}: {err}", workspace.display()))?;
        Ok(Some(Box::new(container)))
    }
    #[cfg(not(windows))]
    {
        let _ = workspace;
        Ok(None)
    }
}

/// Discover the host toolchain locations a contained build must read. Paths are
/// environment-derived on purpose: `RUSTUP_HOME`/`CARGO_HOME` when set,
/// otherwise the standard per-user locations. A container never inherits the
/// host environment, so the default locations are covered next to the
/// effective ones, and the credential stores under both are excluded. Missing
/// trees are skipped: there is nothing to grant.
///
/// The homes are not the whole answer: a managed host (a CI runner image, a
/// toolcache install) can keep the toolchain the host itself runs somewhere
/// else. [`effective_toolchain_roots`] asks the host's own tools where that
/// is — the `rustc --print sysroot` answer and the cargo/rustc executable
/// directories — so a contained build never dies with an access error for a
/// toolchain the host runs fine.
///
/// The MSVC toolset ([`build_tool_environment`]) needs no grant of its own:
/// a Visual Studio/Windows SDK installation under Program Files already gives
/// Application Packages read+execute (measured on this host: the ACE is
/// inherited onto the toolset and SDK trees), and an unelevated user could not
/// write an ACE there anyway. What the container lacks is the *environment*
/// that names those directories, which is why the discovery exists.
///
/// The cargo home itself is deliberately never a grant root: only its
/// `bin`, `registry` and `git` subtrees are, so `credentials.toml` sits outside
/// every grant. `config.toml` is granted as a single file.
pub fn toolchain_access() -> ToolchainAccess {
    let profile = std::env::var_os("USERPROFILE").map(PathBuf::from);
    let mut access = toolchain_access_from(
        env_path("RUSTUP_HOME"),
        env_path("CARGO_HOME"),
        profile.as_ref().map(|home| home.join(".rustup")),
        profile.as_ref().map(|home| home.join(".cargo")),
    );
    merge_roots(&mut access, effective_toolchain_roots());
    access
}

/// The MSVC build-tool environment a contained build needs, and the host
/// directories it references.
///
/// A contained `cargo build` of a binary needs a native linker, and on Windows
/// `rustc` finds MSVC the way a Developer Command Prompt does: from
/// `VCINSTALLDIR` plus the tool directories that command prompt puts on
/// `PATH`, and from `LIB`/`INCLUDE` for the libraries and headers the linker
/// feeds on. None of that works inside the AppContainer on its own — the
/// registry views the Visual Studio discovery uses are unreadable there, and
/// the container inherits no host environment — so the host discovers the
/// environment once and shares it with contained children (`sandbox_win`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildToolEnvironment {
    /// The allowlisted variables a Developer Command Prompt exports, normalized
    /// to uppercase keys (Windows environment lookup is case-insensitive).
    pub variables: BTreeMap<String, String>,
    /// The directories those variables put on the build's search paths, when
    /// they live inside the Visual C++ / Windows SDK installation trees. They
    /// are reported rather than granted: the container reads them through the
    /// Application Packages ACE the installation already carries, and the
    /// unelevated host user cannot write an ACE there. A host whose SDK lives
    /// outside such a tree would need the ACL widened by its owner instead.
    pub roots: Vec<PathBuf>,
}

/// The keys a Developer Command Prompt exports that a contained MSVC build
/// needs. Discovery reads the whole host environment, so only this explicit
/// allowlist crosses into the container: every entry is a location or a
/// tool-identity string, never a credential.
#[cfg(windows)]
const BUILD_TOOL_VARIABLES: &[&str] = &[
    "PATH",
    "LIB",
    "LIBPATH",
    "INCLUDE",
    "VCINSTALLDIR",
    "VCToolsInstallDir",
    "VCToolsRedistDir",
    "VCToolsVersion",
    "VSINSTALLDIR",
    "VisualStudioVersion",
    "WindowsSdkDir",
    "WindowsSdkBinPath",
    "WindowsSdkVerBinPath",
    "WindowsSDKLibVersion",
    "WindowsSDKVersion",
    "UniversalCRTSdkDir",
    "UCRTVersion",
    "NETFXSDKDir",
    "VSCMD_ARG_HOST_ARCH",
    "VSCMD_ARG_TGT_ARCH",
    "VSCMD_ARG_VCVARS_SPECTRE",
    "VSCMD_VER",
];

/// The host's build-tool environment, discovered once per process (the
/// discovery spawns the Visual Studio command script, which is too expensive to
/// repeat per child). Empty when the host has no discoverable MSVC build tools:
/// contained linking then behaves as before rather than refusing to run.
pub fn build_tool_environment() -> BuildToolEnvironment {
    static DISCOVERED: OnceLock<BuildToolEnvironment> = OnceLock::new();
    DISCOVERED
        .get_or_init(discover_build_tool_environment)
        .clone()
}

fn discover_build_tool_environment() -> BuildToolEnvironment {
    #[cfg(windows)]
    {
        // A host already inside a Developer Command Prompt answers directly:
        // the variables are in this process and no script needs to run.
        let mut variables = build_tool_variables(std::env::vars());
        if !variables.contains_key("VCINSTALLDIR") {
            variables = developer_prompt_script()
                .and_then(|script| capture_developer_prompt(&script))
                .unwrap_or_default();
        }
        let roots = build_tool_roots(&variables);
        BuildToolEnvironment { variables, roots }
    }
    #[cfg(not(windows))]
    {
        BuildToolEnvironment::default()
    }
}

/// Keep the allowlisted keys, normalized to uppercase, dropping anything else
/// the host environment carries (including credentials).
#[cfg(windows)]
fn build_tool_variables(
    entries: impl Iterator<Item = (String, String)>,
) -> BTreeMap<String, String> {
    let mut variables = BTreeMap::new();
    for (key, value) in entries {
        if value.is_empty() {
            continue;
        }
        if BUILD_TOOL_VARIABLES
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(&key))
        {
            variables.insert(key.to_ascii_uppercase(), value);
        }
    }
    variables
}

/// Parse a `set` dump: `KEY=VALUE` per line, first `=` splits, allowlist
/// applies exactly as for the host environment.
#[cfg(windows)]
fn parse_build_tool_environment(text: &str) -> BTreeMap<String, String> {
    build_tool_variables(text.lines().filter_map(|line| line.split_once('=')).map(
        |(key, value)| {
            (
                key.trim().to_string(),
                value.trim_end_matches('\r').to_string(),
            )
        },
    ))
}

/// Directories the discovered variables put on the build's search paths and
/// that live inside the Visual C++ / Windows SDK installation trees: the
/// linker and its DLLs, the import libraries, the SDK libraries and binaries.
/// Entries belonging to other Visual Studio components (MSBuild, Team Tools,
/// the IDE) are dropped, so the list describes the linker's own dependency
/// closure rather than the installation. It is evidence for diagnostics and
/// tests, not a grant list: the container already reads these trees.
#[cfg(windows)]
fn build_tool_roots(variables: &BTreeMap<String, String>) -> Vec<PathBuf> {
    let mut anchors: Vec<PathBuf> = Vec::new();
    for key in [
        "VCTOOLSINSTALLDIR",
        "VCINSTALLDIR",
        "WINDOWSSDKDIR",
        "UNIVERSALCRTSDKDIR",
        "NETFXSDKDIR",
    ] {
        if let Some(value) = variables.get(key) {
            let anchor = PathBuf::from(value);
            if anchor.is_dir() && !anchors.contains(&anchor) {
                anchors.push(anchor);
            }
        }
    }
    let mut roots: Vec<PathBuf> = Vec::new();
    for key in ["PATH", "LIB", "LIBPATH", "INCLUDE"] {
        let Some(value) = variables.get(key) else {
            continue;
        };
        for entry in std::env::split_paths(value) {
            if entry.is_dir()
                && anchors.iter().any(|anchor| under(&entry, anchor))
                && !roots.contains(&entry)
            {
                roots.push(entry);
            }
        }
    }
    roots
}

/// Prefix-boundary containment on normalized paths: `C:\Kits\10` contains
/// `C:\Kits\10\lib` but not `C:\Kits\100`.
#[cfg(windows)]
fn under(path: &Path, anchor: &Path) -> bool {
    let anchor = normalized(anchor);
    if anchor.is_empty() {
        return false;
    }
    let path = normalized(path);
    path == anchor || path.starts_with(&format!("{anchor}\\"))
}

/// `vcvars64.bat` of the newest installation that carries the x64 build tools.
/// `vswhere.exe` ships with the Visual Studio installer and answers on the
/// host; a session that already knows its installation (`VSINSTALLDIR`) is
/// answered without spawning anything.
#[cfg(windows)]
fn developer_prompt_script() -> Option<PathBuf> {
    let script = |install: &Path| {
        install
            .join("VC")
            .join("Auxiliary")
            .join("Build")
            .join("vcvars64.bat")
    };
    if let Some(install) = env_path("VSINSTALLDIR") {
        let script = script(&install);
        if script.is_file() {
            return Some(script);
        }
    }
    let program_files = env_path("ProgramFiles(x86)").or_else(|| env_path("ProgramFiles"))?;
    let vswhere = program_files
        .join("Microsoft Visual Studio")
        .join("Installer")
        .join("vswhere.exe");
    if !vswhere.is_file() {
        return None;
    }
    let output = std::process::Command::new(&vswhere)
        .args([
            "-latest",
            "-products",
            "*",
            "-requires",
            "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
            "-property",
            "installationPath",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let install = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()?
        .trim()
        .to_string();
    if install.is_empty() {
        return None;
    }
    let script = script(Path::new(&install));
    script.is_file().then_some(script)
}

/// Export a Developer Command Prompt environment by running its own
/// `vcvars64.bat` on the host. The script exits non-zero when optional
/// components are missing while still exporting a usable environment, so the
/// variables decide; its banner and warnings are not parsed.
#[cfg(windows)]
fn capture_developer_prompt(script: &Path) -> Option<BTreeMap<String, String>> {
    use std::os::windows::process::CommandExt;
    let command = env_path("COMSPEC")
        .or_else(|| env_path("SystemRoot").map(|root| root.join("System32").join("cmd.exe")))?;
    let mut process = std::process::Command::new(&command);
    process.arg("/D").arg("/C");
    // Verbatim, so cmd sees exactly one quoted `call ... && set` line.
    process.raw_arg(format!("call \"{}\" >NUL && set", script.display()));
    let output = process.output().ok()?;
    let variables = parse_build_tool_environment(&String::from_utf8_lossy(&output.stdout));
    (!variables.is_empty()).then_some(variables)
}

/// Add discovered roots to `access`, keeping existing directories only and
/// dropping duplicates: discovery depends on the host's tool layout, the grant
/// list must not.
fn merge_roots(access: &mut ToolchainAccess, extras: Vec<PathBuf>) {
    for root in extras {
        if root.is_dir() && !access.read_roots.contains(&root) {
            access.read_roots.push(root);
        }
    }
}

/// Roots of the toolchain the *host* actually runs, as the host's own tools
/// report it: `rustc --print sysroot` (a rustup toolchain answers with its
/// toolchain directory, a standalone unpack with its install directory) plus
/// the directories the cargo/rustc executables live in (`RUSTC`/`CARGO` when
/// set, otherwise the first hit on `PATH`).
fn effective_toolchain_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut push = |path: PathBuf| {
        if path.is_dir() && !roots.contains(&path) {
            roots.push(path);
        }
    };
    if let Some(rustc) = host_executable("RUSTC", "rustc") {
        if let Some(sysroot) = command_stdout(&rustc, &["--print", "sysroot"]) {
            push(PathBuf::from(sysroot));
        }
    }
    for (variable, name) in [("RUSTC", "rustc"), ("CARGO", "cargo")] {
        if let Some(exe) = host_executable(variable, name) {
            if let Some(dir) = exe.parent() {
                push(dir.to_path_buf());
            }
        }
    }
    roots
}

/// The executable the host would launch for `variable`/`name`: the explicit
/// variable first (cargo sets `CARGO` for the test processes it spawns), then
/// `PATH` with the platform executable suffix.
fn host_executable(variable: &str, name: &str) -> Option<PathBuf> {
    if let Some(explicit) = env_path(variable).filter(|path| path.is_file()) {
        return Some(explicit);
    }
    find_on_path(&format!("{name}{}", std::env::consts::EXE_SUFFIX)).map(PathBuf::from)
}

/// First line of `exe args`' stdout on the host, when the command succeeds.
fn command_stdout(exe: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(exe).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().next()?.trim().to_string();
    (!line.is_empty()).then_some(line)
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn toolchain_access_from(
    rustup_home: Option<PathBuf>,
    cargo_home: Option<PathBuf>,
    default_rustup: Option<PathBuf>,
    default_cargo: Option<PathBuf>,
) -> ToolchainAccess {
    let mut read_roots: Vec<PathBuf> = Vec::new();
    let mut read_files: Vec<PathBuf> = Vec::new();
    let mut excluded_files: Vec<PathBuf> = Vec::new();
    // The rustup home (toolchains, settings, downloads) holds no credential
    // store, so it is granted whole.
    for root in [rustup_home, default_rustup].into_iter().flatten() {
        if root.is_dir() && !read_roots.contains(&root) {
            read_roots.push(root);
        }
    }
    let mut cargo_homes: Vec<PathBuf> = Vec::new();
    for home in [cargo_home, default_cargo].into_iter().flatten() {
        if !cargo_homes.contains(&home) {
            cargo_homes.push(home);
        }
    }
    for home in cargo_homes {
        for subtree in ["bin", "registry", "git"] {
            let root = home.join(subtree);
            if root.is_dir() && !read_roots.contains(&root) {
                read_roots.push(root);
            }
        }
        let config = home.join("config.toml");
        if config.is_file() && !read_files.contains(&config) {
            read_files.push(config);
        }
        for name in ["credentials.toml", "credentials"] {
            let file = home.join(name);
            if file.is_file() && !excluded_files.contains(&file) {
                excluded_files.push(file);
            }
        }
    }
    ToolchainAccess {
        read_roots,
        read_files,
        excluded_files,
    }
}

fn probe_linux() -> SandboxCapabilities {
    let Some(bwrap) = find_on_path("bwrap") else {
        return SandboxCapabilities {
            platform: Platform::Linux,
            filesystem_containment: false,
            network_denied: false,
            backend: "bubblewrap not found; no verified enforcement".into(),
        };
    };
    // A real probe: mount namespace + read-only root + network unshare. The
    // full guarantee matrix (write denial, symlink/rename resistance,
    // protected state) is a Phase-0 obligation, so the note stays explicit.
    let request = ExecRequest {
        argv: vec![
            bwrap,
            "--unshare-net".into(),
            "--ro-bind".into(),
            "/".into(),
            "/".into(),
            "--".into(),
            "/bin/true".into(),
        ],
        cwd: PathBuf::from("/"),
        timeout: Duration::from_secs(5),
        env: BTreeMap::new(),
        max_output_bytes: 4096,
    };
    let outcome = run(&request);
    if outcome.status == ExecStatus::Exited && outcome.exit_code == Some(0) {
        SandboxCapabilities {
            platform: Platform::Linux,
            filesystem_containment: true,
            network_denied: true,
            backend: "bubblewrap (basic namespace probe passed)".into(),
        }
    } else {
        SandboxCapabilities {
            platform: Platform::Linux,
            filesystem_containment: false,
            network_denied: false,
            backend: format!(
                "bubblewrap probe failed: {:?} exit={:?}",
                outcome.status, outcome.exit_code
            ),
        }
    }
}

#[cfg(windows)]
fn probe_windows() -> SandboxCapabilities {
    SandboxCapabilities {
        platform: Platform::Windows,
        // The cheap probe makes no claim; workspace-mode startup replaces this
        // with `verified_probe()` before validation, so a run never proceeds on
        // an unexecuted claim.
        filesystem_containment: false,
        network_denied: false,
        backend: "Windows AppContainer: substrate present; workspace mode verifies it with an \
                 executed check (`bollo doctor` shows the evidence)"
            .into(),
    }
}

#[cfg(windows)]
fn verified_probe_windows() -> SandboxCapabilities {
    match crate::sandbox_win::selfcheck_containment() {
        Ok(report)
            if report.write_inside_ok && report.write_outside_denied && report.network_denied =>
        {
            SandboxCapabilities {
                platform: Platform::Windows,
                filesystem_containment: true,
                network_denied: true,
                backend: "Windows AppContainer (executed check passed: writes outside granted \
                         roots denied; outbound network including loopback denied; brokered \
                         children are launched inside the container)"
                    .into(),
            }
        }
        Ok(report) => SandboxCapabilities {
            platform: Platform::Windows,
            filesystem_containment: false,
            network_denied: false,
            backend: format!(
                "Windows AppContainer: executed check failed ({}); refusing workspace mode",
                report.detail
            ),
        },
        Err(err) => SandboxCapabilities {
            platform: Platform::Windows,
            filesystem_containment: false,
            network_denied: false,
            backend: format!(
                "Windows AppContainer: mechanism unavailable ({err}); refusing workspace mode"
            ),
        },
    }
}

#[cfg(not(windows))]
fn probe_windows() -> SandboxCapabilities {
    SandboxCapabilities {
        platform: Platform::Windows,
        filesystem_containment: false,
        network_denied: false,
        backend: "Windows probe is not reachable on this host platform".into(),
    }
}

#[cfg(not(windows))]
fn verified_probe_windows() -> SandboxCapabilities {
    probe_windows()
}

pub(crate) fn find_on_path(binary: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toolchain_access_grants_caches_and_excludes_credentials() {
        let base =
            std::env::temp_dir().join(format!("bollo-toolchain-access-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let rustup = base.join("rustup");
        let cargo = base.join("cargo");
        let override_cargo = base.join("override-cargo");
        for home in [&rustup, &cargo, &override_cargo] {
            std::fs::create_dir_all(home).unwrap();
        }
        for home in [&cargo, &override_cargo] {
            for subtree in ["bin", "registry", "git"] {
                std::fs::create_dir_all(home.join(subtree)).unwrap();
            }
            std::fs::write(home.join("config.toml"), "# config\n").unwrap();
            std::fs::write(home.join("credentials.toml"), "token = \"secret\"\n").unwrap();
            std::fs::write(home.join("credentials"), "legacy\n").unwrap();
        }

        let access = toolchain_access_from(
            Some(rustup.clone()),
            Some(cargo.clone()),
            Some(base.join("absent-rustup")),
            Some(base.join("absent-cargo")),
        );
        assert_eq!(
            access.read_roots,
            vec![
                rustup.clone(),
                cargo.join("bin"),
                cargo.join("registry"),
                cargo.join("git"),
            ]
        );
        assert_eq!(access.read_files, vec![cargo.join("config.toml")]);
        assert_eq!(
            access.excluded_files,
            vec![cargo.join("credentials.toml"), cargo.join("credentials")]
        );
        // The exclusion is structural: no credential store may sit inside a
        // granted root, because Windows grants capability-SID access through
        // allow ACEs only (a deny ACE does not override an inherited one).
        for excluded in &access.excluded_files {
            assert!(
                access
                    .read_roots
                    .iter()
                    .all(|root| !excluded.starts_with(root)),
                "{} is inside a granted root",
                excluded.display()
            );
        }

        // A store inside a granted root is a refusal, not a grant: the root
        // ACE would make the store readable through inheritance.
        assert!(access.credential_conflict(&rustup).is_none());
        assert!(access
            .credential_conflict(&cargo.join("registry"))
            .is_none());
        assert_eq!(
            access.credential_conflict(&cargo),
            Some(cargo.join("credentials.toml").as_path())
        );
        assert!(access.credential_conflict(&base).is_some());
        // Containment is case-insensitive and prefix-boundary aware.
        assert!(access
            .credential_conflict(&PathBuf::from(cargo.to_string_lossy().to_uppercase()))
            .is_some());
        assert!(access.credential_conflict(&base.join("carg")).is_none());

        // An environment override is covered next to the default locations,
        // and credentials are excluded under both.
        let overridden = toolchain_access_from(
            None,
            Some(override_cargo.clone()),
            None,
            Some(cargo.clone()),
        );
        assert!(overridden.read_roots.contains(&override_cargo.join("bin")));
        assert!(overridden.read_roots.contains(&cargo.join("registry")));
        assert!(overridden
            .excluded_files
            .contains(&override_cargo.join("credentials.toml")));
        assert!(overridden
            .excluded_files
            .contains(&cargo.join("credentials")));

        // Missing trees are skipped: there is nothing to grant.
        let absent = toolchain_access_from(
            Some(base.join("absent-rustup")),
            Some(base.join("absent-cargo")),
            None,
            None,
        );
        assert!(absent.read_roots.is_empty());
        assert!(absent.read_files.is_empty());
        assert!(absent.excluded_files.is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Discovery reads the whole host environment, so only explicitly
    /// allowlisted keys may cross; a credential-looking variable must not.
    #[test]
    #[cfg(windows)]
    fn build_tool_parsing_keeps_locations_only() {
        let parsed = parse_build_tool_environment(concat!(
            "VCINSTALLDIR=C:\\BuildTools\\VC\\\r\n",
            "Path=C:\\BuildTools\\VC\\Tools\\MSVC\\14.51.36231\\bin\\HostX64\\x64;",
            "C:\\Windows\\System32\r\n",
            "VSCMD_ARG_TGT_ARCH=x64\r\n",
            "ANTHROPIC_API_KEY=sk-do-not-copy\r\n",
            "CARGO_REGISTRY_TOKEN=do-not-copy\r\n",
            "no-equals-sign\r\n",
        ));
        assert!(parsed.contains_key("VCINSTALLDIR"));
        assert!(parsed.contains_key("PATH"));
        assert_eq!(parsed.get("VSCMD_ARG_TGT_ARCH"), Some(&"x64".to_string()));
        assert!(
            !parsed.keys().any(|key| key.contains("ANTHROPIC")),
            "a credential-bearing variable crossed the allowlist: {parsed:?}"
        );
        assert!(!parsed
            .values()
            .any(|value| value.contains("sk-do-not-copy")));
        assert!(!parsed.values().any(|value| value.contains("do-not-copy")));
        // Host-environment harvest applies the same allowlist.
        let harvested = build_tool_variables(
            [
                ("VCINSTALLDIR".to_string(), "C:\\vc\\".to_string()),
                ("path".to_string(), "C:\\bin".to_string()),
                ("OPENAI_API_KEY".to_string(), "sk-secret".to_string()),
                ("EMPTY".to_string(), String::new()),
            ]
            .into_iter(),
        );
        assert_eq!(harvested.get("PATH"), Some(&"C:\\bin".to_string()));
        assert_eq!(harvested.len(), 2, "{harvested:?}");
    }

    /// The granted directories are exactly the build search paths inside the
    /// Visual C++/Windows SDK installations: the linker's dependency closure,
    /// not the installation.
    #[test]
    #[cfg(windows)]
    fn build_tool_roots_keep_the_linker_closure_only() {
        let base = std::env::temp_dir().join(format!("bollo-build-tools-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let tools = base
            .join("BuildTools")
            .join("VC")
            .join("Tools")
            .join("MSVC");
        let kits = base.join("Kits").join("10");
        let dirs = [
            tools
                .join("14.51.36231")
                .join("bin")
                .join("HostX64")
                .join("x64"),
            tools.join("14.51.36231").join("lib").join("x64"),
            tools.join("14.51.36231").join("include"),
            kits.join("bin").join("10.0.26100.0").join("x64"),
            kits.join("lib").join("10.0.26100.0").join("um").join("x64"),
        ];
        for dir in &dirs {
            std::fs::create_dir_all(dir).unwrap();
        }
        // Another Visual Studio component: on PATH, inside the installation,
        // but not part of a link.
        let msbuild = base
            .join("BuildTools")
            .join("MSBuild")
            .join("Current")
            .join("Bin");
        std::fs::create_dir_all(&msbuild).unwrap();
        // A sibling outside the Windows Kits root that shares its name prefix.
        let kits_sibling = base.join("Kits").join("100");
        std::fs::create_dir_all(&kits_sibling).unwrap();

        let mut variables = BTreeMap::new();
        variables.insert(
            "VCTOOLSINSTALLDIR".to_string(),
            format!("{}\\", tools.join("14.51.36231").display()),
        );
        variables.insert("WINDOWSSDKDIR".to_string(), format!("{}\\", kits.display()));
        let path = std::env::join_paths([
            dirs[0].clone(),
            msbuild.clone(),
            kits_sibling.clone(),
            dirs[3].clone(),
        ])
        .unwrap();
        variables.insert("PATH".to_string(), path.to_string_lossy().into_owned());
        let lib = std::env::join_paths([dirs[1].clone(), dirs[4].clone()]).unwrap();
        variables.insert("LIB".to_string(), lib.to_string_lossy().into_owned());
        variables.insert(
            "INCLUDE".to_string(),
            dirs[2].to_string_lossy().into_owned(),
        );

        let roots = build_tool_roots(&variables);
        for want in &dirs {
            assert!(
                roots.contains(want),
                "{} missing from {roots:#?}",
                want.display()
            );
        }
        assert!(
            !roots.contains(&msbuild),
            "an unrelated Visual Studio component was granted: {roots:#?}"
        );
        assert!(
            !roots.contains(&kits_sibling),
            "a sibling of the kits root was granted: {roots:#?}"
        );
        // No discovery is not a grant: empty variables yield no roots.
        assert!(build_tool_roots(&BTreeMap::new()).is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn effective_roots_are_merged_only_when_they_exist() {
        let base =
            std::env::temp_dir().join(format!("bollo-effective-roots-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let present = base.join("sysroot");
        std::fs::create_dir_all(&present).unwrap();
        let mut access = ToolchainAccess::default();
        merge_roots(
            &mut access,
            vec![present.clone(), base.join("absent"), present.clone()],
        );
        assert_eq!(access.read_roots, vec![present]);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn discovery_follows_the_host_toolchain() {
        // The host's own answer is the authority: when this machine runs a
        // rustc, its reported sysroot must be discovered, so a toolchain kept
        // outside the default homes is still granted (the failure that put
        // this test here: a runner's cargo resolved a rustc outside every
        // grant and died with `Access is denied`).
        let discovered = effective_toolchain_roots();
        for root in &discovered {
            assert!(root.is_dir(), "{} is not a directory", root.display());
        }
        let Some(rustc) = host_executable("RUSTC", "rustc") else {
            return;
        };
        let Some(sysroot) = command_stdout(&rustc, &["--print", "sysroot"]) else {
            return;
        };
        let sysroot = normalized(Path::new(sysroot.trim()));
        assert!(
            discovered.iter().any(|root| normalized(root) == sysroot),
            "sysroot {sysroot} missing from discovered roots: {discovered:#?}"
        );
    }

    #[test]
    fn probe_reports_current_platform_and_never_overclaims() {
        let caps = probe();
        assert_eq!(caps.platform, Platform::current());
        if cfg!(windows) {
            assert!(!caps.filesystem_containment);
            assert!(!caps.network_denied);
            assert!(
                caps.backend.contains("Windows AppContainer"),
                "backend must carry executed mechanism evidence: {}",
                caps.backend
            );
        }
        // The probe result must be internally consistent.
        if caps.supports_workspace_auto() {
            assert!(caps.filesystem_containment && caps.network_denied);
        }
        // The verified probe executes the mechanism check on Windows (and on
        // Linux runs the bubblewrap probe). Whatever it reports must be
        // internally consistent and never a partial claim.
        let verified = verified_probe();
        assert_eq!(verified.platform, Platform::current());
        if cfg!(windows) {
            assert!(
                verified.backend.contains("Windows AppContainer"),
                "verified probe must carry executed evidence: {}",
                verified.backend
            );
        }
        if verified.supports_workspace_auto() {
            assert!(verified.filesystem_containment && verified.network_denied);
        }
    }
}
