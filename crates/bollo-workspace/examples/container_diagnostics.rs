//! Container diagnostics: the CI capture for the runner-only contained-rustc
//! failure.
//!
//! `cargo` inside the AppContainer cannot execute `rustc` on a GitHub runner
//! (`Access is denied`, os error 5) while the same contained build succeeds on
//! an ordinary Windows host. The containment test's own probes are identical on
//! both hosts, so the capture that decides the question has to record state the
//! test does not: how each host resolves `rustc`, what the container token
//! actually carries, the DACL *and mandatory label* of every candidate, what
//! raw Win32 open/execute calls return inside the container, what contained
//! `cargo build -v` reports, and the host's execution gates (Defender ASR,
//! WDAC, SRP, AppLocker).
//!
//! Parent mode (default): capture the host, build an AppContainer for a temp
//! workspace, grant the host toolchain exactly like a workspace-mode run, copy
//! this executable into the granted workspace, run it as the in-container
//! child, then merge both reports into `--out` (default
//! `container-diagnostics.json`) and print them.
//!
//! Child mode (`--child`): run the same probes from inside the container and
//! write JSON to `--report`.
//!
//! Nothing here fails a build: every probe records its own error, and the
//! parent writes a report even when host discovery or container creation
//! fails. The artifact, not the exit status, is the deliverable.

#[cfg(windows)]
mod diag {
    use std::collections::BTreeMap;
    use std::ffi::c_void;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use serde_json::{json, Value};

    use bollo_workspace::process::{ChildSandbox, ExecRequest};
    use bollo_workspace::sandbox::{build_tool_environment, toolchain_access};
    use bollo_workspace::sandbox_win::AppContainer;

    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, LocalFree, FILETIME, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertSidToStringSidW,
        GetNamedSecurityInfoW, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::Security::{
        GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TokenAppContainerSid,
        TokenCapabilities, TokenGroups, TokenIntegrityLevel, TokenIsAppContainer, TokenUser,
        DACL_SECURITY_INFORMATION, LABEL_SECURITY_INFORMATION, OBJECT_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR, PSID, TOKEN_APPCONTAINER_INFORMATION, TOKEN_GROUPS,
        TOKEN_INFORMATION_CLASS, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetFileAttributesW, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, INVALID_FILE_ATTRIBUTES, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Diagnostics::Debug::{
        FormatMessageW, FORMAT_MESSAGE_FROM_SYSTEM, FORMAT_MESSAGE_IGNORE_INSERTS,
    };
    use windows_sys::Win32::System::JobObjects::{
        IsProcessInJob, JobObjectExtendedLimitInformation, QueryInformationJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
        JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
    };
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegEnumValueW, RegOpenKeyExW, RegQueryValueExW, HKEY,
        HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY, REG_DWORD, REG_EXPAND_SZ, REG_MULTI_SZ,
        REG_QWORD, REG_SZ, REG_VALUE_TYPE,
    };
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess, GetExitCodeProcess,
        InitializeProcThreadAttributeList, IsWow64Process, OpenProcessToken,
        UpdateProcThreadAttribute, WaitForSingleObject, CREATE_BREAKAWAY_FROM_JOB,
        CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT,
        PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, STARTF_USESTDHANDLES,
        STARTUPINFOEXW, STARTUPINFOW,
    };

    /// Probe output never grows past this; the report is evidence, not a dump.
    const CAPTURE_LIMIT: usize = 12_000;

    pub fn run() {
        let args: Vec<String> = std::env::args().collect();
        if args.iter().any(|arg| arg == "--child") {
            child_mode(&args);
        } else {
            parent_mode(&args);
        }
    }

    // ------------------------------------------------------------------
    // Argument helpers
    // ------------------------------------------------------------------

    fn arg_value(args: &[String], flag: &str) -> Option<String> {
        let index = args.iter().position(|arg| arg == flag)?;
        args.get(index + 1).cloned()
    }

    fn arg_values(args: &[String], flag: &str) -> Vec<String> {
        let mut values = Vec::new();
        let mut index = 0;
        while index + 1 < args.len() {
            if args[index] == flag {
                values.push(args[index + 1].clone());
                index += 2;
            } else {
                index += 1;
            }
        }
        values
    }

    // ------------------------------------------------------------------
    // Parent: host capture, container run, merge
    // ------------------------------------------------------------------

    fn parent_mode(args: &[String]) {
        let out = arg_value(args, "--out").unwrap_or_else(|| "container-diagnostics.json".into());
        let candidates = discover_candidates();
        let mut report = json!({
            "tool": "container_diagnostics",
            "mode": "parent",
            "unix_time": SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
            "host": host_report(),
            "candidates": candidates.iter().map(|c| c.display().to_string()).collect::<Vec<_>>(),
            "host_probes": candidates.iter().map(|c| exec_probe(c)).collect::<Vec<_>>(),
            "parent_chains": chains_for(&candidates),
        });

        report["container"] = match run_child(&candidates) {
            Ok(value) => value,
            Err(err) => json!({"error": err}),
        };

        let text = pretty(&report);
        match std::fs::write(&out, &text) {
            Ok(()) => println!("container diagnostics written to {out}"),
            Err(err) => eprintln!("could not write {out}: {err}"),
        }
        println!("{text}");
    }

    fn run_child(candidates: &[PathBuf]) -> Result<Value, String> {
        let base =
            std::env::temp_dir().join(format!("bollo-container-diag-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let workspace = base.join("ws");
        let toy = workspace.join("toy");
        let toy_forced = workspace.join("toy-forced");
        let result = (|| -> Result<Value, String> {
            std::fs::create_dir_all(&workspace)
                .map_err(|err| format!("create {}: {err}", workspace.display()))?;
            write_toy(&toy)?;
            write_toy(&toy_forced)?;

            let mut container = AppContainer::create_workspace(&workspace)?;
            let access = toolchain_access();
            for root in &access.read_roots {
                container
                    .grant_toolchain_read(root)
                    .map_err(|err| format!("grant {}: {err}", root.display()))?;
            }
            for file in &access.read_files {
                container
                    .grant_toolchain_read_file(file)
                    .map_err(|err| format!("grant {}: {err}", file.display()))?;
            }
            container.grant_workspace_modify(&workspace)?;

            let child_exe = workspace.join("container_diag_child.exe");
            let current = std::env::current_exe().map_err(|err| format!("current_exe: {err}"))?;
            std::fs::copy(&current, &child_exe)
                .map_err(|err| format!("copy {}: {err}", child_exe.display()))?;

            let child_report = workspace.join("child-report.json");
            let mut argv = vec![
                child_exe.display().to_string(),
                "--child".into(),
                "--report".into(),
                child_report.display().to_string(),
                "--toy".into(),
                toy.display().to_string(),
                "--toy-forced".into(),
                toy_forced.display().to_string(),
                "--cargo".into(),
                host_cargo().display().to_string(),
            ];
            for candidate in candidates {
                argv.push("--candidate".into());
                argv.push(candidate.display().to_string());
            }
            for sid in container.capability_strings() {
                argv.push("--capability".into());
                argv.push(sid);
            }
            if let Some(rustc) = real_rustc(candidates) {
                argv.push("--rustc".into());
                argv.push(rustc.display().to_string());
                argv.push("--matrix".into());
                argv.push(rustc.display().to_string());
            }

            let run = container.run(&ExecRequest {
                argv,
                cwd: workspace.clone(),
                timeout: Duration::from_secs(600),
                env: BTreeMap::new(),
                max_output_bytes: 4 * 1024 * 1024,
            });
            let parsed = std::fs::read_to_string(&child_report)
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(&text).ok());
            let mut merged = json!({
                "exit_code": run.exit_code,
                "status": format!("{:?}", run.status),
                "duration_ms": run.duration_ms,
                "stdout": truncate(&run.stdout, CAPTURE_LIMIT),
                "stderr": truncate(&run.stderr, CAPTURE_LIMIT),
                "report": parsed,
            });
            if merged["report"].is_null() {
                merged["report_error"] = json!(format!(
                    "child report missing at {}",
                    child_report.display()
                ));
            }
            Ok(merged)
        })();
        let _ = std::fs::remove_dir_all(&base);
        result
    }

    fn write_toy(dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(dir.join("src"))
            .map_err(|err| format!("create {}: {err}", dir.display()))?;
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"toy\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[workspace]\n",
        )
        .map_err(|err| format!("write toy Cargo.toml: {err}"))?;
        std::fs::write(
            dir.join("src").join("lib.rs"),
            "pub fn answer() -> u32 { 42 }\n",
        )
        .map_err(|err| format!("write toy lib.rs: {err}"))?;
        Ok(())
    }

    fn host_cargo() -> PathBuf {
        if let Ok(cargo) = std::env::var("CARGO") {
            let candidate = PathBuf::from(cargo);
            if candidate.is_file() {
                return candidate;
            }
        }
        let path = std::env::var_os("PATH").unwrap_or_default();
        std::env::split_paths(&path)
            .map(|dir| dir.join("cargo.exe"))
            .find(|candidate| candidate.is_file())
            .unwrap_or_else(|| PathBuf::from("cargo.exe"))
    }

    /// The rustup-managed real toolchain binary among the candidates, used for
    /// the forced-`RUSTC` build that separates "resolution picked the wrong
    /// file" from "this file cannot execute here".
    fn real_rustc(candidates: &[PathBuf]) -> Option<PathBuf> {
        candidates
            .iter()
            .find(|candidate| {
                let text = candidate.to_string_lossy().to_lowercase();
                text.contains("\\toolchains\\") && text.ends_with("rustc.exe")
            })
            .cloned()
            .or_else(|| {
                candidates.iter().find_map(|candidate| {
                    let name = candidate.file_name()?.to_string_lossy().to_lowercase();
                    (name == "rustc.exe").then(|| candidate.clone())
                })
            })
    }

    fn discover_candidates() -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = Vec::new();
        let mut push = |path: PathBuf| {
            if path.is_file() && !found.iter().any(|existing| same_path(existing, &path)) {
                found.push(path);
            }
        };
        for variable in ["RUSTC", "CARGO"] {
            if let Some(value) = std::env::var_os(variable) {
                push(PathBuf::from(value));
            }
        }
        for name in ["rustc", "cargo", "rustup"] {
            for line in where_lines(name) {
                push(PathBuf::from(line));
            }
        }
        for line in where_lines("rustc") {
            let candidate = PathBuf::from(line);
            if let Some(parent) = candidate.parent() {
                push(parent.join("rustc.exe"));
                push(parent.join("rustup.exe"));
            }
        }
        if let Some(sysroot) = rustc_sysroot() {
            push(sysroot.join("bin").join("rustc.exe"));
        }
        let access = toolchain_access();
        for root in &access.read_roots {
            for leaf in ["rustc.exe", "cargo.exe", "rustup.exe"] {
                push(root.join(leaf));
            }
            for leaf in ["rustc.exe", "cargo.exe"] {
                push(root.join("bin").join(leaf));
            }
        }
        if let Some(path) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path) {
                for leaf in ["rustc.exe", "cargo.exe"] {
                    push(dir.join(leaf));
                }
            }
        }
        found.truncate(16);
        found
    }

    fn where_lines(name: &str) -> Vec<String> {
        let output = std::process::Command::new("where").arg(name).output();
        match output {
            Ok(output) => String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(|line| line.trim().to_string())
                .filter(|line| !line.is_empty())
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    fn rustc_sysroot() -> Option<PathBuf> {
        let output = std::process::Command::new("rustc")
            .args(["--print", "sysroot"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let line = String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()?
            .trim()
            .to_string();
        (!line.is_empty()).then(|| PathBuf::from(line))
    }

    fn same_path(left: &Path, right: &Path) -> bool {
        left.to_string_lossy().to_lowercase() == right.to_string_lossy().to_lowercase()
    }

    // ------------------------------------------------------------------
    // Host report
    // ------------------------------------------------------------------

    fn host_report() -> Value {
        let access = toolchain_access();
        let build_tools = build_tool_environment();
        json!({
            "whoami_user": command_lines("whoami", &["/user"]),
            "whoami_groups": command_lines("whoami", &["/groups"]),
            "token": token_report(),
            "arch": arch_report(),
            "env": env_subset(),
            "where_rustc": command_lines("where", &["rustc"]),
            "where_cargo": command_lines("where", &["cargo"]),
            "rustc_version": command_lines("rustc", &["-vV"]),
            "cargo_version": command_lines("cargo", &["-vV"]),
            "rustup_which_rustc": command_lines("rustup", &["which", "rustc"]),
            "rustup_toolchain_list": command_lines("rustup", &["toolchain", "list"]),
            "toolchain_access": {
                "read_roots": access.read_roots.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
                "read_files": access.read_files.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
                "excluded_files": access.excluded_files.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            },
            "build_tool_variables": build_tools.variables.keys().collect::<Vec<_>>(),
            "policy": policy_report(),
            "code_integrity": code_integrity_report(),
        })
    }

    fn command_lines(program: &str, args: &[&str]) -> Value {
        match std::process::Command::new(program).args(args).output() {
            Ok(output) => json!({
                "exit": output.status.code(),
                "stdout": truncate(&String::from_utf8_lossy(&output.stdout), 4000),
                "stderr": truncate(&String::from_utf8_lossy(&output.stderr), 2000),
            }),
            Err(err) => json!({"error": err.to_string(), "raw": err.raw_os_error()}),
        }
    }

    fn env_subset() -> Value {
        let mut values = serde_json::Map::new();
        for key in [
            "PATH",
            "PATHEXT",
            "COMSPEC",
            "RUSTUP_HOME",
            "CARGO_HOME",
            "USERPROFILE",
            "TEMP",
            "TMP",
            "SystemRoot",
            "PROCESSOR_ARCHITECTURE",
            "PROCESSOR_IDENTIFIER",
            "NUMBER_OF_PROCESSORS",
        ] {
            if let Ok(value) = std::env::var(key) {
                values.insert(key.to_string(), json!(value));
            }
        }
        Value::Object(values)
    }

    fn arch_report() -> Value {
        let mut wow64: i32 = 0;
        let queried = unsafe { IsWow64Process(GetCurrentProcess(), &mut wow64) };
        json!({
            "pointer_bits": std::mem::size_of::<usize>() * 8,
            "processor_architecture": std::env::var("PROCESSOR_ARCHITECTURE").ok(),
            "processor_identifier": std::env::var("PROCESSOR_IDENTIFIER").ok(),
            "wow64_query_ok": queried != 0,
            "wow64": wow64 != 0,
        })
    }

    // ------------------------------------------------------------------
    // Child: the same probes, executed inside the container
    // ------------------------------------------------------------------

    fn child_mode(args: &[String]) {
        let report_path = arg_value(args, "--report").unwrap_or_else(|| "child-report.json".into());
        let matrix_path = arg_value(args, "--matrix").map(PathBuf::from);
        let candidates: Vec<PathBuf> = arg_values(args, "--candidate")
            .into_iter()
            .map(PathBuf::from)
            .collect();
        let expected_capabilities = arg_values(args, "--capability");
        let cargo = arg_value(args, "--cargo").map(PathBuf::from);
        let rustc = arg_value(args, "--rustc").map(PathBuf::from);
        let toy = arg_value(args, "--toy").map(PathBuf::from);
        let toy_forced = arg_value(args, "--toy-forced").map(PathBuf::from);

        let capability_match = capability_match(&expected_capabilities);
        let report = json!({
            "mode": "child",
            "current_exe": std::env::current_exe().map(|p| p.display().to_string()).ok(),
            "cwd": std::env::current_dir().map(|p| p.display().to_string()).ok(),
            "whoami_user": command_lines("whoami", &["/user"]),
            "whoami_groups": command_lines("whoami", &["/groups"]),
            "token": token_report(),
            "expected_capabilities": expected_capabilities,
            "capability_match": capability_match,
            "arch": arch_report(),
            "env_all": env_all(),
            "where_rustc": command_lines("where", &["rustc"]),
            "where_cargo": command_lines("where", &["cargo"]),
            "path_resolution": path_resolution(),
            "parent_chains": chains_for(&candidates),
            "probes": candidates.iter().map(|c| exec_probe(c)).collect::<Vec<_>>(),
            "job": job_report(),
            "process_matrix": matrix_path.as_deref().map(process_matrix),
            "cargo": cargo_report(cargo.as_deref(), rustc.as_deref(), toy.as_deref(), toy_forced.as_deref()),
            "policy": policy_report(),
            "code_integrity": code_integrity_report(),
        });

        let text = pretty(&report);
        match std::fs::write(&report_path, &text) {
            Ok(()) => println!("child report written to {report_path}"),
            Err(err) => println!("child could not write {report_path}: {err}"),
        }
    }

    /// Every variable the container passes in. This is the environment block
    /// the broker builds, so by design it never carries credentials; printing
    /// it is itself the evidence that the allowlist held.
    fn env_all() -> Value {
        let mut values = serde_json::Map::new();
        for (key, value) in std::env::vars() {
            values.insert(key, json!(value));
        }
        Value::Object(values)
    }

    fn path_resolution() -> Value {
        let mut entries = Vec::new();
        if let Some(path) = std::env::var_os("PATH") {
            for (index, dir) in std::env::split_paths(&path).enumerate() {
                entries.push(json!({
                    "index": index,
                    "dir": dir.display().to_string(),
                    "rustc": dir.join("rustc.exe").is_file(),
                    "cargo": dir.join("cargo.exe").is_file(),
                    "rustup": dir.join("rustup.exe").is_file(),
                }));
            }
        }
        json!(entries)
    }

    fn cargo_report(
        cargo: Option<&Path>,
        rustc: Option<&Path>,
        toy: Option<&Path>,
        toy_forced: Option<&Path>,
    ) -> Value {
        let resolved = std::env::var_os("PATH")
            .map(|path| {
                std::env::split_paths(&path)
                    .map(|dir| dir.join("cargo.exe"))
                    .find(|candidate| candidate.is_file())
            })
            .flatten();
        let program = cargo
            .map(Path::to_path_buf)
            .or_else(|| resolved.clone())
            .unwrap_or_else(|| PathBuf::from("cargo.exe"));
        let mut report = json!({
            "program": program.display().to_string(),
            "path_resolved": resolved.map(|p| p.display().to_string()),
            "vV": run_capture(&program, &["-vV"], None, &[]),
        });
        if let Some(toy) = toy {
            report["build_verbose"] =
                run_capture(&program, &["build", "-v", "--offline"], Some(toy), &[]);
        }
        if let (Some(toy_forced), Some(rustc)) = (toy_forced, rustc) {
            report["build_verbose_forced_rustc"] = run_capture(
                &program,
                &["build", "-v", "--offline"],
                Some(toy_forced),
                &[("RUSTC", &rustc.display().to_string())],
            );
            report["forced_rustc"] = json!(rustc.display().to_string());
        }
        report
    }

    fn run_capture(
        program: &Path,
        args: &[&str],
        cwd: Option<&Path>,
        env_extra: &[(&str, &str)],
    ) -> Value {
        let mut command = std::process::Command::new(program);
        command.args(args);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        for (key, value) in env_extra {
            command.env(key, value);
        }
        match command.output() {
            Ok(output) => json!({
                "exit": output.status.code(),
                "stdout": truncate(&String::from_utf8_lossy(&output.stdout), CAPTURE_LIMIT),
                "stderr": truncate(&String::from_utf8_lossy(&output.stderr), CAPTURE_LIMIT),
            }),
            Err(err) => json!({"error": err.to_string(), "raw": err.raw_os_error()}),
        }
    }

    // ------------------------------------------------------------------
    // Per-candidate probes
    // ------------------------------------------------------------------

    fn exec_probe(path: &Path) -> Value {
        let name = wide(&path.to_string_lossy());
        let attributes = unsafe { GetFileAttributesW(name.as_ptr()) };
        let exists = attributes != INVALID_FILE_ATTRIBUTES;
        json!({
            "path": path.display().to_string(),
            "exists": exists,
            "attributes": if exists { Some(attributes) } else { None },
            "reparse_point": exists && (attributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0,
            "size": std::fs::metadata(path).ok().map(|meta| meta.len()),
            "sddl_dacl": sddl(path, DACL_SECURITY_INFORMATION),
            "sddl_label": sddl(path, LABEL_SECURITY_INFORMATION),
            "open_read": open_probe(&name, FILE_GENERIC_READ),
            "open_execute": open_probe(&name, FILE_GENERIC_EXECUTE),
            "open_read_execute": open_probe(&name, FILE_GENERIC_READ | FILE_GENERIC_EXECUTE),
            "raw_create_process": raw_exec(path),
            "std_version": std_version(path),
            "cmd_version": cmd_version(path),
            "hardlinks": hardlink_lines(path),
        })
    }

    fn open_probe(name: &[u16], access: u32) -> Value {
        unsafe {
            let handle = CreateFileW(
                name.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            );
            if handle == INVALID_HANDLE_VALUE {
                return json!({"error": code_text(GetLastError())});
            }
            CloseHandle(handle);
            json!("ok")
        }
    }

    /// Exact `CreateProcessW` with `lpApplicationName` set to the candidate:
    /// no PATH search, no cmd, no shell. The raw `GetLastError` is what tells
    /// an image-level denial from a resolution problem.
    fn raw_exec(path: &Path) -> Value {
        unsafe {
            let name = wide(&path.to_string_lossy());
            let mut line = wide(&format!("\"{}\" --version", path.display()));
            let mut startup: STARTUPINFOW = std::mem::zeroed();
            startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
            let mut info: PROCESS_INFORMATION = std::mem::zeroed();
            let created = CreateProcessW(
                name.as_ptr(),
                line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                CREATE_NO_WINDOW,
                std::ptr::null(),
                std::ptr::null(),
                &startup,
                &mut info,
            );
            if created == 0 {
                return json!({"error": code_text(GetLastError())});
            }
            let wait = WaitForSingleObject(info.hProcess, 30_000);
            let mut code: u32 = 0;
            let got = GetExitCodeProcess(info.hProcess, &mut code);
            CloseHandle(info.hThread);
            CloseHandle(info.hProcess);
            json!({
                "created": true,
                "wait_completed": wait == WAIT_OBJECT_0,
                "exit": if got != 0 { Some(code) } else { None },
            })
        }
    }

    fn std_version(path: &Path) -> Value {
        match std::process::Command::new(path).arg("--version").output() {
            Ok(output) => json!({
                "exit": output.status.code(),
                "stdout": truncate(&String::from_utf8_lossy(&output.stdout), 300),
                "stderr": truncate(&String::from_utf8_lossy(&output.stderr), 300),
            }),
            Err(err) => json!({"error": err.to_string(), "raw": err.raw_os_error()}),
        }
    }

    fn cmd_version(path: &Path) -> Value {
        let cmd = std::env::var("SystemRoot")
            .map(|root| PathBuf::from(root).join("System32").join("cmd.exe"))
            .unwrap_or_else(|_| PathBuf::from("cmd.exe"));
        match std::process::Command::new(cmd)
            .arg("/C")
            .arg(path)
            .arg("--version")
            .output()
        {
            Ok(output) => json!({
                "exit": output.status.code(),
                "stdout": truncate(&String::from_utf8_lossy(&output.stdout), 300),
                "stderr": truncate(&String::from_utf8_lossy(&output.stderr), 300),
            }),
            Err(err) => json!({"error": err.to_string(), "raw": err.raw_os_error()}),
        }
    }

    /// One `CreateProcessW` variant per parameter a caller can change. On the
    /// runner, raw creation with nothing but the application name succeeds
    /// inside the container while `std::process::Command` is denied, so the
    /// matrix isolates which parameter the denial follows: handle inheritance,
    /// the environment block, the unicode-environment flag, stdio handles, the
    /// handle list, breakaway-from-job, or a console.
    fn process_matrix(path: &Path) -> Value {
        let name = wide(&path.to_string_lossy());
        let base_line = format!("\"{}\" --version", path.display());
        let environment = environment_block();
        let plain = plain_startup();
        let mut variants = vec![
            matrix_entry("raw_appname_noinherit", unsafe {
                create_core(
                    name.as_ptr(),
                    &mut wide(&base_line),
                    0,
                    std::ptr::null(),
                    CREATE_NO_WINDOW,
                    &plain,
                )
            }),
            matrix_entry("raw_appname_inherit", unsafe {
                create_core(
                    name.as_ptr(),
                    &mut wide(&base_line),
                    1,
                    std::ptr::null(),
                    CREATE_NO_WINDOW,
                    &plain,
                )
            }),
            matrix_entry("raw_no_appname_noinherit", unsafe {
                create_core(
                    std::ptr::null(),
                    &mut wide(&base_line),
                    0,
                    std::ptr::null(),
                    CREATE_NO_WINDOW,
                    &plain,
                )
            }),
            matrix_entry("raw_appname_env_unicode", unsafe {
                create_core(
                    name.as_ptr(),
                    &mut wide(&base_line),
                    0,
                    environment.as_ptr() as *const c_void,
                    CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
                    &plain,
                )
            }),
            matrix_entry("raw_appname_inherit_env_unicode", unsafe {
                create_core(
                    name.as_ptr(),
                    &mut wide(&base_line),
                    1,
                    environment.as_ptr() as *const c_void,
                    CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
                    &plain,
                )
            }),
            matrix_entry("raw_appname_breakaway", unsafe {
                create_core(
                    name.as_ptr(),
                    &mut wide(&base_line),
                    0,
                    std::ptr::null(),
                    CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB,
                    &plain,
                )
            }),
        ];
        if let Some(handles) = unsafe { nul_handles() } {
            variants.push(matrix_entry("raw_appname_inherit_stdio_nul", unsafe {
                create_core(
                    name.as_ptr(),
                    &mut wide(&base_line),
                    1,
                    std::ptr::null(),
                    CREATE_NO_WINDOW,
                    &nul_startup(&handles),
                )
            }));
            variants.push(matrix_entry(
                "raw_appname_inherit_stdio_nul_handle_list",
                unsafe { create_with_handle_list(&name, &mut wide(&base_line), &handles) },
            ));
            for handle in handles {
                unsafe {
                    CloseHandle(handle);
                }
            }
        }
        variants.push(matrix_entry(
            "std_pipes",
            command_variant(path, false, false),
        ));
        variants.push(matrix_entry("std_null", command_variant(path, true, false)));
        variants.push(matrix_entry(
            "std_null_no_window",
            command_variant(path, true, true),
        ));
        json!(variants)
    }

    fn matrix_entry(variant: &str, result: Value) -> Value {
        json!({"variant": variant, "result": result})
    }

    /// The shape the launcher itself uses, plus the environment block: a
    /// Unicode environment is what `std::process::Command` always supplies.
    fn environment_block() -> Vec<u16> {
        let mut block = Vec::new();
        for (key, value) in std::env::vars() {
            block.extend(format!("{key}={value}").encode_utf16());
            block.push(0);
        }
        block.push(0);
        block
    }

    fn plain_startup() -> STARTUPINFOW {
        let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
        startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        startup
    }

    unsafe fn nul_handles() -> Option<[HANDLE; 3]> {
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: 1,
        };
        let nul = wide("NUL");
        let mut handles: Vec<HANDLE> = Vec::new();
        for _ in 0..3 {
            let handle = CreateFileW(
                nul.as_ptr(),
                FILE_GENERIC_READ | FILE_GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                &mut attributes,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            );
            if handle == INVALID_HANDLE_VALUE {
                for existing in handles {
                    CloseHandle(existing);
                }
                return None;
            }
            handles.push(handle);
        }
        Some([handles[0], handles[1], handles[2]])
    }

    fn nul_startup(handles: &[HANDLE; 3]) -> STARTUPINFOW {
        let mut startup = plain_startup();
        startup.dwFlags = STARTF_USESTDHANDLES;
        startup.hStdInput = handles[0];
        startup.hStdOutput = handles[1];
        startup.hStdError = handles[2];
        startup
    }

    unsafe fn create_core(
        application: *const u16,
        line: &mut [u16],
        inherit: i32,
        environment: *const c_void,
        flags: u32,
        startup: &STARTUPINFOW,
    ) -> Value {
        let mut info: PROCESS_INFORMATION = std::mem::zeroed();
        let created = CreateProcessW(
            application,
            line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            inherit,
            flags,
            environment,
            std::ptr::null(),
            startup,
            &mut info,
        );
        if created == 0 {
            return json!({"created": false, "error": code_text(GetLastError())});
        }
        let wait = WaitForSingleObject(info.hProcess, 30_000);
        let mut code: u32 = 0;
        let got = GetExitCodeProcess(info.hProcess, &mut code);
        CloseHandle(info.hThread);
        CloseHandle(info.hProcess);
        json!({
            "created": true,
            "wait_completed": wait == WAIT_OBJECT_0,
            "exit": if got != 0 { Some(code) } else { None },
        })
    }

    unsafe fn create_with_handle_list(
        name: &[u16],
        line: &mut [u16],
        handles: &[HANDLE; 3],
    ) -> Value {
        let mut size: usize = 0;
        InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size);
        if size == 0 {
            return json!({"created": false, "error": format!("attribute list size: {}", code_text(GetLastError()))});
        }
        let mut memory = vec![0u8; size];
        let list = memory.as_mut_ptr() as *mut c_void;
        if InitializeProcThreadAttributeList(list, 1, 0, &mut size) == 0 {
            return json!({"created": false, "error": format!("InitializeProcThreadAttributeList: {}", code_text(GetLastError()))});
        }
        let updated = UpdateProcThreadAttribute(
            list,
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            handles.as_ptr() as *const c_void,
            std::mem::size_of_val(handles),
            std::ptr::null_mut(),
            std::ptr::null(),
        );
        if updated == 0 {
            let error = code_text(GetLastError());
            DeleteProcThreadAttributeList(list);
            return json!({"created": false, "error": format!("UpdateProcThreadAttribute: {error}")});
        }
        let mut startup: STARTUPINFOEXW = std::mem::zeroed();
        startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = handles[0];
        startup.StartupInfo.hStdOutput = handles[1];
        startup.StartupInfo.hStdError = handles[2];
        startup.lpAttributeList = list;
        let result = create_core(
            name.as_ptr(),
            line,
            1,
            std::ptr::null(),
            EXTENDED_STARTUPINFO_PRESENT | CREATE_NO_WINDOW,
            &startup.StartupInfo,
        );
        DeleteProcThreadAttributeList(list);
        result
    }

    fn command_variant(path: &Path, null_stdio: bool, no_window: bool) -> Value {
        let mut command = std::process::Command::new(path);
        command.arg("--version");
        if null_stdio {
            command
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
        }
        if no_window {
            command.creation_flags(CREATE_NO_WINDOW);
        }
        match command.output() {
            Ok(output) => json!({"exit": output.status.code()}),
            Err(err) => json!({"error": err.to_string(), "raw": err.raw_os_error()}),
        }
    }

    /// The job the container child is in. A breakaway request that the job does
    /// not allow fails with exactly ERROR_ACCESS_DENIED, so the flags here
    /// decide whether that is a possible cause.
    fn job_report() -> Value {
        unsafe {
            let mut in_job: i32 = 0;
            let queried_presence =
                IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut in_job);
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            let mut returned: u32 = 0;
            let queried_limits = QueryInformationJobObject(
                std::ptr::null_mut(),
                JobObjectExtendedLimitInformation,
                &mut info as *mut _ as *mut c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                &mut returned,
            );
            let flags = if queried_limits != 0 {
                info.BasicLimitInformation.LimitFlags
            } else {
                0
            };
            json!({
                "presence_query_ok": queried_presence != 0,
                "in_job": in_job != 0,
                "limit_query_ok": queried_limits != 0,
                "limit_flags": flags,
                "breakaway_ok": flags & JOB_OBJECT_LIMIT_BREAKAWAY_OK != 0,
                "silent_breakaway_ok": flags & JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK != 0,
                "kill_on_job_close": flags & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE != 0,
                "active_process_limited": flags & JOB_OBJECT_LIMIT_ACTIVE_PROCESS != 0,
                "active_process_limit": info.BasicLimitInformation.ActiveProcessLimit,
            })
        }
    }

    fn hardlink_lines(path: &Path) -> Value {
        command_lines("fsutil", &["hardlink", "list", &path.display().to_string()])
    }

    fn chains_for(candidates: &[PathBuf]) -> Value {
        let mut chains: Vec<Value> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        for candidate in candidates {
            let Some(parent) = candidate.parent() else {
                continue;
            };
            let key = parent.to_string_lossy().to_lowercase();
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            let mut chain: Vec<Value> = Vec::new();
            let mut current = Some(parent.to_path_buf());
            while let Some(dir) = current {
                if chain.len() > 12 {
                    break;
                }
                chain.push(json!({
                    "path": dir.display().to_string(),
                    "sddl_dacl": sddl(&dir, DACL_SECURITY_INFORMATION),
                    "sddl_label": sddl(&dir, LABEL_SECURITY_INFORMATION),
                    "reparse_point": reparse_point(&dir),
                }));
                current = dir.parent().map(Path::to_path_buf);
            }
            chains.push(json!({
                "candidate": candidate.display().to_string(),
                "dir": parent.display().to_string(),
                "chain": chain,
            }));
        }
        json!(chains)
    }

    fn reparse_point(path: &Path) -> bool {
        let name = wide(&path.to_string_lossy());
        let attributes = unsafe { GetFileAttributesW(name.as_ptr()) };
        attributes != INVALID_FILE_ATTRIBUTES && (attributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0
    }

    // ------------------------------------------------------------------
    // Security descriptors and tokens
    // ------------------------------------------------------------------

    fn sddl(path: &Path, information: OBJECT_SECURITY_INFORMATION) -> Value {
        unsafe {
            let name = wide(&path.to_string_lossy());
            let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
            let status = GetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                information,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut descriptor,
            );
            if status != 0 {
                return json!({"error": code_text(status)});
            }
            let mut text: *mut u16 = std::ptr::null_mut();
            let mut length: u32 = 0;
            let converted = ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor,
                1,
                information,
                &mut text,
                &mut length,
            );
            if !descriptor.is_null() {
                LocalFree(descriptor as *mut c_void);
            }
            if converted == 0 {
                return json!({"error": code_text(GetLastError())});
            }
            let value = from_wide(text);
            if !text.is_null() {
                LocalFree(text as *mut c_void);
            }
            json!(value)
        }
    }

    fn token_report() -> Value {
        json!({
            "user": token_user(),
            "integrity": token_integrity(),
            "is_app_container": token_is_app_container(),
            "app_container_sid": token_app_container_sid(),
            "groups": token_sid_list(TokenGroups),
            "capabilities": token_sid_list(TokenCapabilities),
        })
    }

    fn capability_match(expected: &[String]) -> Value {
        let present = token_sid_list(TokenCapabilities);
        let mut values = serde_json::Map::new();
        for sid in expected {
            let found = present
                .iter()
                .any(|value| value.get("sid").and_then(Value::as_str) == Some(sid.as_str()));
            values.insert(sid.clone(), json!(found));
        }
        Value::Object(values)
    }

    fn token_bytes(class: TOKEN_INFORMATION_CLASS) -> Result<Vec<u8>, String> {
        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(format!("OpenProcessToken: {}", code_text(GetLastError())));
            }
            let mut size: u32 = 0;
            GetTokenInformation(token, class, std::ptr::null_mut(), 0, &mut size);
            let mut buffer = vec![0u8; size.max(16) as usize];
            let ok = GetTokenInformation(
                token,
                class,
                buffer.as_mut_ptr() as *mut c_void,
                size,
                &mut size,
            );
            CloseHandle(token);
            if ok == 0 {
                return Err(format!(
                    "GetTokenInformation({class}): {}",
                    code_text(GetLastError())
                ));
            }
            buffer.truncate(size as usize);
            Ok(buffer)
        }
    }

    fn read_struct<T: Copy>(buffer: &[u8]) -> T {
        unsafe { std::ptr::read_unaligned(buffer.as_ptr() as *const T) }
    }

    fn token_user() -> Value {
        match token_bytes(TokenUser) {
            Ok(buffer) if buffer.len() >= std::mem::size_of::<TOKEN_USER>() => {
                let user: TOKEN_USER = read_struct(&buffer);
                json!(sid_string(user.User.Sid))
            }
            Ok(_) => json!("buffer too small"),
            Err(err) => json!(err),
        }
    }

    fn token_is_app_container() -> Value {
        match token_bytes(TokenIsAppContainer) {
            Ok(buffer) if buffer.len() >= 4 => {
                let value: u32 = read_struct(&buffer);
                json!(value != 0)
            }
            Ok(_) => json!("buffer too small"),
            Err(err) => json!(err),
        }
    }

    fn token_app_container_sid() -> Value {
        match token_bytes(TokenAppContainerSid) {
            Ok(buffer) if buffer.len() >= std::mem::size_of::<TOKEN_APPCONTAINER_INFORMATION>() => {
                let info: TOKEN_APPCONTAINER_INFORMATION = read_struct(&buffer);
                json!(sid_string(info.TokenAppContainer))
            }
            Ok(_) => json!("buffer too small"),
            Err(err) => json!(err),
        }
    }

    fn token_integrity() -> Value {
        match token_bytes(TokenIntegrityLevel) {
            Ok(buffer) if buffer.len() >= std::mem::size_of::<TOKEN_MANDATORY_LABEL>() => {
                let label: TOKEN_MANDATORY_LABEL = read_struct(&buffer);
                let sid = label.Label.Sid;
                json!({
                    "sid": sid_string(sid),
                    "level": integrity_name(sid),
                })
            }
            Ok(_) => json!("buffer too small"),
            Err(err) => json!(err),
        }
    }

    fn token_sid_list(class: TOKEN_INFORMATION_CLASS) -> Vec<Value> {
        match token_bytes(class) {
            Ok(buffer) if buffer.len() >= std::mem::size_of::<TOKEN_GROUPS>() => {
                let header: TOKEN_GROUPS = read_struct(&buffer);
                let offset = std::mem::offset_of!(TOKEN_GROUPS, Groups);
                let entry = std::mem::size_of::<windows_sys::Win32::Security::SID_AND_ATTRIBUTES>();
                let mut values = Vec::new();
                for index in 0..header.GroupCount as usize {
                    let start = offset + index * entry;
                    if start + entry > buffer.len() {
                        break;
                    }
                    let item: windows_sys::Win32::Security::SID_AND_ATTRIBUTES =
                        read_struct(&buffer[start..start + entry]);
                    values.push(json!({
                        "sid": sid_string(item.Sid),
                        "attributes": item.Attributes,
                    }));
                }
                values
            }
            Ok(_) => vec![json!("buffer too small")],
            Err(err) => vec![json!(err)],
        }
    }

    fn sid_string(sid: PSID) -> String {
        if sid.is_null() {
            return "null".into();
        }
        unsafe {
            let mut text: *mut u16 = std::ptr::null_mut();
            if ConvertSidToStringSidW(sid, &mut text) == 0 {
                return format!("ConvertSidToStringSidW: {}", code_text(GetLastError()));
            }
            let value = from_wide(text);
            if !text.is_null() {
                LocalFree(text as *mut c_void);
            }
            value
        }
    }

    fn integrity_name(sid: PSID) -> &'static str {
        if sid.is_null() {
            return "unknown";
        }
        unsafe {
            let count = GetSidSubAuthorityCount(sid);
            if count.is_null() || *count == 0 {
                return "unknown";
            }
            let last = GetSidSubAuthority(sid, (*count - 1) as u32);
            if last.is_null() {
                return "unknown";
            }
            match *last {
                0x0000 => "untrusted",
                0x1000 => "low",
                0x2000 => "medium",
                0x2100 => "medium-plus",
                0x3000 => "high",
                0x4000 => "system",
                0x5000 => "protected",
                _ => "unknown",
            }
        }
    }

    // ------------------------------------------------------------------
    // Policy: the host-side gates that can deny an execution
    // ------------------------------------------------------------------

    fn policy_report() -> Value {
        json!({
            "defender": {
                "disable_realtime_monitoring": reg_value(
                    "SOFTWARE\\Microsoft\\Windows Defender\\Real-Time Protection",
                    Some("DisableRealtimeMonitoring"),
                ),
                "policy_disable_realtime_monitoring": reg_value(
                    "SOFTWARE\\Policies\\Microsoft\\Windows Defender\\Real-Time Protection",
                    Some("DisableRealtimeMonitoring"),
                ),
                "policy_disable_antispyware": reg_value(
                    "SOFTWARE\\Policies\\Microsoft\\Windows Defender",
                    Some("DisableAntiSpyware"),
                ),
                "tamper_protection": reg_value(
                    "SOFTWARE\\Microsoft\\Windows Defender\\Features",
                    Some("TamperProtection"),
                ),
                "asr_rules_machine": reg_rule_map(
                    "SOFTWARE\\Microsoft\\Windows Defender\\Windows Defender Exploit Guard\\ASR\\Rules",
                ),
                "asr_rules_policy": reg_rule_map(
                    "SOFTWARE\\Policies\\Microsoft\\Windows Defender\\Windows Defender Exploit Guard\\ASR\\Rules",
                ),
                "exclusions_paths": reg_subkeys(
                    "SOFTWARE\\Microsoft\\Windows Defender\\Exclusions\\Paths",
                ),
            },
            "wdac": {
                "ci_policy": reg_dump_values("SYSTEM\\CurrentControlSet\\Control\\CI\\Policy"),
                "ci_config": reg_dump_values("SYSTEM\\CurrentControlSet\\Control\\CI\\Config"),
            },
            "srp": reg_dump_values("SOFTWARE\\Policies\\Microsoft\\Windows\\Safer\\CodeIdentifiers"),
            "applocker": {
                "subkeys": reg_subkeys("SOFTWARE\\Policies\\Microsoft\\Windows\\AppLocker"),
                "exe": reg_dump_values("SOFTWARE\\Policies\\Microsoft\\Windows\\AppLocker\\Exe"),
            },
        })
    }

    fn code_integrity_report() -> Value {
        json!({
            "sipolicy_present": Path::new("C:\\Windows\\System32\\CodeIntegrity\\SIPolicy.p7b").is_file(),
            "active_policies": dir_list("C:\\Windows\\System32\\CodeIntegrity\\CiPolicies\\Active"),
        })
    }

    fn dir_list(path: &str) -> Value {
        match std::fs::read_dir(path) {
            Ok(entries) => json!(entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect::<Vec<_>>()),
            Err(err) => json!(format!("{err}")),
        }
    }

    fn reg_value(subkey: &str, name: Option<&str>) -> Value {
        unsafe {
            let mut key: HKEY = std::ptr::null_mut();
            let subkey_wide = wide(subkey);
            let status = RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
                subkey_wide.as_ptr(),
                0,
                KEY_READ | KEY_WOW64_64KEY,
                &mut key,
            );
            if status != 0 {
                return json!({"missing": true});
            }
            let name_wide = name.map(wide);
            let pointer = name_wide
                .as_ref()
                .map_or(std::ptr::null(), |value| value.as_ptr());
            let mut kind: REG_VALUE_TYPE = 0;
            let mut size: u32 = 0;
            let status = RegQueryValueExW(
                key,
                pointer,
                std::ptr::null(),
                &mut kind,
                std::ptr::null_mut(),
                &mut size,
            );
            if status != 0 {
                RegCloseKey(key);
                return json!({"missing": true});
            }
            let mut data = vec![0u8; size as usize + 2];
            let status = RegQueryValueExW(
                key,
                pointer,
                std::ptr::null(),
                &mut kind,
                data.as_mut_ptr(),
                &mut size,
            );
            RegCloseKey(key);
            if status != 0 {
                return json!({"error": code_text(status)});
            }
            data.truncate(size as usize);
            reg_data_value(kind, &data)
        }
    }

    fn reg_dump_values(subkey: &str) -> Value {
        unsafe {
            let mut key: HKEY = std::ptr::null_mut();
            let subkey_wide = wide(subkey);
            if RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
                subkey_wide.as_ptr(),
                0,
                KEY_READ | KEY_WOW64_64KEY,
                &mut key,
            ) != 0
            {
                return json!({"missing": true});
            }
            let mut values = serde_json::Map::new();
            let mut index = 0u32;
            loop {
                let mut name_buffer = vec![0u16; 512];
                let mut name_length = name_buffer.len() as u32;
                let mut kind: REG_VALUE_TYPE = 0;
                let mut size: u32 = 0;
                let status = RegEnumValueW(
                    key,
                    index,
                    name_buffer.as_mut_ptr(),
                    &mut name_length,
                    std::ptr::null(),
                    &mut kind,
                    std::ptr::null_mut(),
                    &mut size,
                );
                if status != 0 {
                    break;
                }
                let name = String::from_utf16_lossy(&name_buffer[..name_length as usize]);
                let mut data = vec![0u8; size as usize + 2];
                let mut size2 = size;
                let status = RegEnumValueW(
                    key,
                    index,
                    name_buffer.as_mut_ptr(),
                    &mut name_length,
                    std::ptr::null(),
                    &mut kind,
                    data.as_mut_ptr(),
                    &mut size2,
                );
                if status == 0 {
                    data.truncate(size2 as usize);
                    values.insert(name, reg_data_value(kind, &data));
                }
                index += 1;
            }
            RegCloseKey(key);
            Value::Object(values)
        }
    }

    fn reg_subkeys(subkey: &str) -> Value {
        unsafe {
            let mut key: HKEY = std::ptr::null_mut();
            let subkey_wide = wide(subkey);
            if RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
                subkey_wide.as_ptr(),
                0,
                KEY_READ | KEY_WOW64_64KEY,
                &mut key,
            ) != 0
            {
                return json!({"missing": true});
            }
            let mut names = Vec::new();
            let mut index = 0u32;
            loop {
                let mut name_buffer = vec![0u16; 512];
                let mut name_length = name_buffer.len() as u32;
                let status = RegEnumKeyExW(
                    key,
                    index,
                    name_buffer.as_mut_ptr(),
                    &mut name_length,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut::<FILETIME>(),
                );
                if status != 0 {
                    break;
                }
                names.push(String::from_utf16_lossy(
                    &name_buffer[..name_length as usize],
                ));
                index += 1;
            }
            RegCloseKey(key);
            json!(names)
        }
    }

    fn reg_rule_map(subkey: &str) -> Value {
        unsafe {
            let mut key: HKEY = std::ptr::null_mut();
            let subkey_wide = wide(subkey);
            if RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
                subkey_wide.as_ptr(),
                0,
                KEY_READ | KEY_WOW64_64KEY,
                &mut key,
            ) != 0
            {
                return json!({"missing": true});
            }
            let mut rules = serde_json::Map::new();
            let mut index = 0u32;
            loop {
                let mut name_buffer = vec![0u16; 512];
                let mut name_length = name_buffer.len() as u32;
                let status = RegEnumKeyExW(
                    key,
                    index,
                    name_buffer.as_mut_ptr(),
                    &mut name_length,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut::<FILETIME>(),
                );
                if status != 0 {
                    break;
                }
                let rule = String::from_utf16_lossy(&name_buffer[..name_length as usize]);
                let mut rule_key: HKEY = std::ptr::null_mut();
                if RegOpenKeyExW(key, name_buffer.as_ptr(), 0, KEY_READ, &mut rule_key) == 0 {
                    let star = wide("*");
                    let mut kind: REG_VALUE_TYPE = 0;
                    let mut size: u32 = 0;
                    if RegQueryValueExW(
                        rule_key,
                        star.as_ptr(),
                        std::ptr::null(),
                        &mut kind,
                        std::ptr::null_mut(),
                        &mut size,
                    ) == 0
                    {
                        let mut data = vec![0u8; size as usize + 2];
                        let mut size2 = size;
                        if RegQueryValueExW(
                            rule_key,
                            star.as_ptr(),
                            std::ptr::null(),
                            &mut kind,
                            data.as_mut_ptr(),
                            &mut size2,
                        ) == 0
                        {
                            data.truncate(size2 as usize);
                            rules.insert(rule, reg_data_value(kind, &data));
                        }
                    }
                    RegCloseKey(rule_key);
                }
                index += 1;
            }
            RegCloseKey(key);
            Value::Object(rules)
        }
    }

    fn reg_data_value(kind: REG_VALUE_TYPE, data: &[u8]) -> Value {
        match kind {
            REG_SZ | REG_EXPAND_SZ => json!(utf16_string(data)),
            REG_DWORD if data.len() >= 4 => {
                json!(u32::from_le_bytes([data[0], data[1], data[2], data[3]]))
            }
            REG_QWORD if data.len() >= 8 => json!(u64::from_le_bytes([
                data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
            ])),
            REG_MULTI_SZ => {
                let mut items = Vec::new();
                for chunk in utf16_string(data).split('\0') {
                    let item = chunk.trim().to_string();
                    if !item.is_empty() {
                        items.push(item);
                    }
                }
                json!(items)
            }
            other => json!({"type": other, "bytes": data.len()}),
        }
    }

    fn utf16_string(data: &[u8]) -> String {
        let mut units: Vec<u16> = data
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .take_while(|unit| *unit != 0)
            .collect();
        while units.last() == Some(&0) {
            units.pop();
        }
        String::from_utf16_lossy(&units)
    }

    // ------------------------------------------------------------------
    // Small utilities
    // ------------------------------------------------------------------

    fn code_text(code: u32) -> String {
        unsafe {
            let mut buffer = vec![0u16; 1024];
            let length = FormatMessageW(
                FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS,
                std::ptr::null(),
                code,
                0,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                std::ptr::null(),
            );
            if length == 0 {
                return format!("error {code}");
            }
            let text = String::from_utf16_lossy(&buffer[..length as usize]);
            format!("{} ({code})", text.trim())
        }
    }

    fn wide(text: &str) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        let mut units: Vec<u16> = std::ffi::OsStr::new(text).encode_wide().collect();
        units.push(0);
        units
    }

    fn from_wide(pointer: *const u16) -> String {
        if pointer.is_null() {
            return String::new();
        }
        let mut length = 0usize;
        unsafe {
            while *pointer.add(length) != 0 {
                length += 1;
                if length > 32_768 {
                    break;
                }
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(pointer, length))
        }
    }

    fn pretty(value: &Value) -> String {
        serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
    }

    fn truncate(text: &str, limit: usize) -> String {
        if text.chars().count() <= limit {
            return text.to_string();
        }
        let mut clipped: String = text.chars().take(limit).collect();
        clipped.push_str("…(truncated)");
        clipped
    }
}

#[cfg(windows)]
fn main() {
    diag::run();
}

#[cfg(not(windows))]
fn main() {
    println!("container diagnostics are Windows-only");
}
