//! Child process broker: argv execution with no implicit shell parsing,
//! filtered environment, bounded output capture and deadline enforcement.
//!
//! Provider credentials are never inherited: the environment is built from an
//! allowlist plus explicit overrides only.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecRequest {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub timeout: Duration,
    pub env: BTreeMap<String, String>,
    pub max_output_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecStatus {
    Exited,
    TimedOut,
    FailedToStart,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutcome {
    pub status: ExecStatus,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    pub duration_ms: u64,
    /// Whether the child was successfully reaped. `false` means the exit is
    /// not fully observed and the effect state may be unknown.
    pub reaped: bool,
}

impl ExecOutcome {
    pub fn succeeded(&self) -> bool {
        self.status == ExecStatus::Exited && self.exit_code == Some(0)
    }

    pub(crate) fn failed_to_start(message: String) -> Self {
        Self {
            status: ExecStatus::FailedToStart,
            exit_code: None,
            stdout: String::new(),
            stderr: message,
            truncated: false,
            duration_ms: 0,
            reaped: false,
        }
    }
}

/// The allowlisted base environment. Additions must be explicit.
///
/// Only OS/toolchain *locations* are inherited. Credential-bearing variables
/// (API keys, tokens, cloud profiles) are never passed: a child that needs a
/// scoped secret gets it through an explicit, trusted extension record.
pub fn base_environment() -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for key in [
        "PATH",
        "SystemRoot",
        "SYSTEMROOT",
        "TEMP",
        "TMP",
        "HOME",
        "LANG",
        "TERM",
        "COMSPEC",
        // Windows build toolchains resolve their own compiler and SDK from
        // these locations; without them a brokered `cargo test` cannot find a
        // real linker. They are directory paths, never secrets.
        "USERPROFILE",
        "ProgramData",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "LOCALAPPDATA",
        "APPDATA",
    ] {
        if let Ok(value) = std::env::var(key) {
            env.insert(key.to_string(), value);
        }
    }
    env
}

/// A containment backend that runs brokered children under enforced
/// sandboxing (see `sandbox_win`). The host broker ([`run`]) remains the
/// default; a sandbox backend must preserve the same outcome semantics.
pub trait ChildSandbox {
    /// Run a brokered child under enforced containment. Semantics must match
    /// [`run_with_stdin`]; only the isolation differs.
    fn run_with_stdin(&self, request: &ExecRequest, stdin_bytes: Option<&[u8]>) -> ExecOutcome;

    fn run(&self, request: &ExecRequest) -> ExecOutcome {
        self.run_with_stdin(request, None)
    }
}

pub fn run(request: &ExecRequest) -> ExecOutcome {
    run_with_stdin(request, None)
}

/// Convert a resolved path to the form a child process can actually use as a
/// current directory. `std::fs::canonicalize` returns verbatim paths
/// (`\\?\C:\...`) on Windows; most programs do not understand that prefix, and
/// cmd treats it as an unsupported UNC path, silently falling back to the
/// Windows directory. The path is unchanged on other platforms.
pub fn filesystem_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let text = path.to_string_lossy();
        if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = text.strip_prefix(r"\\?\") {
            return PathBuf::from(rest);
        }
    }
    path.to_path_buf()
}

/// Like [`run`] but writes bounded bytes to the child's stdin first (used by
/// trusted command hooks; the write happens on a separate thread so a child
/// that ignores stdin cannot deadlock the broker).
pub fn run_with_stdin(request: &ExecRequest, stdin_bytes: Option<&[u8]>) -> ExecOutcome {
    if request.argv.is_empty() {
        return ExecOutcome::failed_to_start("argv must not be empty".into());
    }
    let started = Instant::now();
    let mut command = Command::new(&request.argv[0]);
    command
        .args(&request.argv[1..])
        .current_dir(filesystem_path(&request.cwd))
        .stdin(if stdin_bytes.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.env_clear();
    for (key, value) in base_environment()
        .iter()
        .chain(request.env.iter())
    {
        command.env(key, value);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console flash for tool children.
        command.creation_flags(0x0800_0000);
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => return ExecOutcome::failed_to_start(err.to_string()),
    };
    if let Some(bytes) = stdin_bytes {
        if let Some(mut stdin) = child.stdin.take() {
            let owned = bytes.to_vec();
            std::thread::spawn(move || {
                use std::io::Write;
                let _ = stdin.write_all(&owned);
                let _ = stdin.flush();
                // Dropping stdin closes the pipe.
            });
        }
    }
    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");
    let cap = request.max_output_bytes;
    let out_thread = std::thread::spawn(move || drain(stdout, cap));
    let err_thread = std::thread::spawn(move || drain(stderr, cap));

    let deadline = started + request.timeout;
    let mut timed_out = false;
    let mut exit_code = None;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                exit_code = status.code();
                break;
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    timed_out = true;
                    let _ = child.kill();
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(err) => return ExecOutcome::failed_to_start(err.to_string()),
        }
    }
    // Reap with a bounded grace period; a failure to reap is reported, never
    // guessed.
    let reap_deadline = Instant::now() + Duration::from_secs(2);
    let mut reaped = false;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                reaped = true;
                break;
            }
            Ok(None) => {
                if Instant::now() >= reap_deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => break,
        }
    }
    let (stdout, out_truncated) = out_thread.join().unwrap_or_default();
    let (stderr, err_truncated) = err_thread.join().unwrap_or_default();
    let duration_ms = started.elapsed().as_millis() as u64;
    if !reaped && timed_out {
        reaped = false;
    }
    ExecOutcome {
        status: if timed_out {
            ExecStatus::TimedOut
        } else {
            ExecStatus::Exited
        },
        exit_code: if timed_out { None } else { exit_code },
        stdout,
        stderr,
        truncated: out_truncated || err_truncated,
        duration_ms,
        reaped,
    }
}

pub(crate) fn drain(mut reader: impl Read, cap: u64) -> (String, bool) {
    let mut buffer: Vec<u8> = Vec::new();
    let mut truncated = false;
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                let remaining = cap.saturating_sub(buffer.len() as u64);
                if remaining == 0 {
                    truncated = true;
                    continue;
                }
                let take = (remaining as usize).min(read);
                buffer.extend_from_slice(&chunk[..take]);
                if take < read {
                    truncated = true;
                }
            }
            Err(_) => break,
        }
    }
    (String::from_utf8_lossy(&buffer).into_owned(), truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(argv: Vec<&str>, timeout_secs: u64) -> ExecRequest {
        ExecRequest {
            argv: argv.into_iter().map(str::to_string).collect(),
            cwd: std::env::current_dir().unwrap(),
            timeout: Duration::from_secs(timeout_secs),
            env: BTreeMap::new(),
            max_output_bytes: 4096,
        }
    }

    #[test]
    fn captures_stdout_and_exit_code() {
        #[cfg(windows)]
        let req = request(vec!["cmd", "/C", "echo hello"], 10);
        #[cfg(unix)]
        let req = request(vec!["sh", "-c", "echo hello"], 10);
        let outcome = run(&req);
        assert_eq!(outcome.status, ExecStatus::Exited);
        assert_eq!(outcome.exit_code, Some(0));
        assert!(outcome.stdout.contains("hello"));
        assert!(outcome.reaped);
    }

    #[test]
    fn missing_program_fails_to_start() {
        let outcome = run(&request(vec!["definitely-not-a-program-xyz"], 5));
        assert_eq!(outcome.status, ExecStatus::FailedToStart);
        assert!(outcome.exit_code.is_none());
    }

    #[test]
    fn timeout_is_reported_with_unknown_exit() {
        #[cfg(windows)]
        let req = request(vec!["cmd", "/C", "ping 127.0.0.1 -n 6 >NUL"], 1);
        #[cfg(unix)]
        let req = request(vec!["sleep", "5"], 1);
        let outcome = run(&req);
        assert_eq!(outcome.status, ExecStatus::TimedOut);
        assert!(outcome.exit_code.is_none());
    }

    #[test]
    fn output_capture_is_bounded() {
        #[cfg(windows)]
        let req = request(vec!["cmd", "/C", "for /L %i in (1,1,2000) do @echo 0123456789"], 10);
        #[cfg(unix)]
        let req = request(vec!["sh", "-c", "yes 0123456789 | head -n 2000"], 10);
        let mut req = req;
        req.max_output_bytes = 256;
        let outcome = run(&req);
        assert!(outcome.truncated);
        assert!(outcome.stdout.len() <= 256);
    }

    #[test]
    fn filesystem_path_strips_the_windows_verbatim_prefix() {
        #[cfg(windows)]
        {
            let verbatim = Path::new(r"\\?\C:\work\project");
            assert_eq!(filesystem_path(verbatim), PathBuf::from(r"C:\work\project"));
            let unc = Path::new(r"\\?\UNC\server\share\project");
            assert_eq!(filesystem_path(unc), PathBuf::from(r"\\server\share\project"));
        }
        let plain = Path::new("/work/project");
        assert_eq!(filesystem_path(plain), PathBuf::from("/work/project"));
    }

    #[test]
    fn environment_is_filtered() {
        std::env::set_var("BOLLO_TEST_CANARY_SECRET", "do-not-inherit");
        std::env::set_var("ANTHROPIC_API_KEY", "sk-do-not-inherit");
        #[cfg(windows)]
        let req = request(vec!["cmd", "/C", "set"], 10);
        #[cfg(unix)]
        let req = request(vec!["sh", "-c", "env"], 10);
        let outcome = run(&req);
        assert!(
            !outcome.stdout.contains("BOLLO_TEST_CANARY_SECRET"),
            "child inherited a credential-looking variable: {}",
            outcome.stdout
        );
        assert!(
            !outcome.stdout.contains("sk-do-not-inherit"),
            "child inherited a provider credential"
        );
        std::env::remove_var("BOLLO_TEST_CANARY_SECRET");
        std::env::remove_var("ANTHROPIC_API_KEY");
        // Locations are inherited; secrets are not.
        let allowed = base_environment();
        assert!(allowed.contains_key("PATH"));
        assert!(!allowed.contains_key("ANTHROPIC_API_KEY"));
    }
}
