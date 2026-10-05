//! Execute: run a prepared action. Every entry point requires a
//! [`bollo_policy::Authorization`] whose intent hash matches the prepared
//! intent, so execution without the gate is not reachable from this crate.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::Value;

use bollo_policy::gate::Authorization;
use bollo_policy::normalize::intent_hash;
use bollo_protocol::errors::ErrorCode;
use bollo_protocol::vocab::ToolStatus;
use bollo_workspace::process::{run, ChildSandbox, ExecOutcome, ExecRequest, ExecStatus};
use bollo_workspace::{CheckpointLog, WorkspaceFs};

use crate::prepare::{PreparedAction, PreparedTool};
use crate::search::search;
use crate::{
    ApplyPatchData, ExecData, GitData, McpToolSpec, ReadFileData, SearchData, ToolError,
    WriteFileData,
};

pub struct ExecuteContext<'a> {
    pub fs: &'a WorkspaceFs,
    pub checkpoints: &'a mut CheckpointLog,
    pub max_output_bytes: u64,
    pub git_timeout: Duration,
    /// Enforcement sandbox for brokered children (workspace mode on a host with
    /// verified containment). `None` means the host broker.
    pub sandbox: Option<&'a dyn ChildSandbox>,
}

impl<'a> ExecuteContext<'a> {
    pub fn new(fs: &'a WorkspaceFs, checkpoints: &'a mut CheckpointLog, max_output_bytes: u64) -> Self {
        Self {
            fs,
            checkpoints,
            max_output_bytes,
            git_timeout: Duration::from_secs(30),
            sandbox: None,
        }
    }
}

/// Route a brokered child through the active sandbox when one is attached,
/// otherwise through the host broker. Semantics are identical either way.
fn run_child(ctx: &ExecuteContext<'_>, request: &ExecRequest) -> ExecOutcome {
    match ctx.sandbox {
        Some(sandbox) => sandbox.run(request),
        None => run(request),
    }
}

/// The host side of MCP dispatch: the composition root owns live server
/// connections; `bollo-tools` only defines the port. A call reaches this port
/// only after prepare → policy → mode → approval → journal intent.
pub trait McpDispatch {
    /// Call a discovered tool identified by its canonical id. Implementations
    /// must never replay an uncertain call: a transport failure is reported as
    /// `Unknown` so the journal records an unknown effect.
    fn dispatch(&mut self, canonical_id: &str, arguments: &Value) -> McpDispatchOutcome;

    /// Give the port a chance to apply an announced catalog change (an MCP
    /// server's `notifications/tools/list_changed`, for example). Returning
    /// `Some` replaces the MCP catalog the runtime advertises to the model and
    /// resolves proposals against, for the rest of the run; `None` means
    /// nothing changed. The default is a no-op, so non-MCP ports are unaffected.
    fn refresh_catalog(&mut self) -> Option<Vec<McpToolSpec>> {
        None
    }
}

/// The result of one MCP dispatch, already mapped onto the runtime's status
/// vocabulary. `error` is reserved for transport/protocol faults; a
/// server-reported `is_error` carries its content as the model-facing message.
#[derive(Debug, Clone)]
pub struct McpDispatchOutcome {
    pub status: ToolStatus,
    pub summary: String,
    pub content: Vec<Value>,
    pub is_error: bool,
    pub truncated: bool,
    pub error: Option<ToolError>,
}

impl McpDispatchOutcome {
    /// A completed round trip. `isError` is the server's own report and maps
    /// to `Failed`, with content preserved for the model as untrusted data.
    pub fn from_result(content: Vec<Value>, is_error: bool, truncated: bool) -> Self {
        Self {
            status: if is_error {
                ToolStatus::Failed
            } else {
                ToolStatus::Succeeded
            },
            summary: String::new(),
            content,
            is_error,
            truncated,
            error: None,
        }
    }

    /// A known failure before or without a server round trip (for example a
    /// quarantined server refusing the call).
    pub fn failed(code: ErrorCode, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            status: ToolStatus::Failed,
            summary: message.clone(),
            content: Vec::new(),
            is_error: true,
            truncated: false,
            error: Some(ToolError::new(code, message)),
        }
    }

    /// An unconfirmable outcome. Never retried automatically.
    pub fn unknown(message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            status: ToolStatus::Unknown,
            summary: message.clone(),
            content: Vec::new(),
            is_error: true,
            truncated: false,
            error: Some(ToolError::new(ErrorCode::OperationUnknown, message)),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToolOutcome {
    pub status: ToolStatus,
    pub summary: String,
    pub data: serde_json::Value,
    pub truncated: bool,
    pub duration_ms: u64,
    pub error: Option<ToolError>,
    pub preimage: Option<String>,
    pub postimage: Option<String>,
}

impl ToolOutcome {
    fn done(
        status: ToolStatus,
        summary: String,
        data: serde_json::Value,
        truncated: bool,
        started: Instant,
        error: Option<ToolError>,
    ) -> Self {
        Self {
            status,
            summary,
            data,
            truncated,
            duration_ms: started.elapsed().as_millis() as u64,
            error,
            preimage: None,
            postimage: None,
        }
    }

    fn failed(summary: String, error: ToolError, started: Instant) -> Self {
        let code = error.code;
        let payload = serde_json::json!({
            "error": {"code": code.as_str(), "message": error.message.clone()}
        });
        Self::done(
            match code {
                ErrorCode::OperationUnknown => ToolStatus::Unknown,
                _ => ToolStatus::Failed,
            },
            summary,
            payload,
            false,
            started,
            Some(error),
        )
    }

    pub fn succeeded(&self) -> bool {
        self.status == ToolStatus::Succeeded
    }
}

pub fn execute(
    prepared: &PreparedTool,
    authorization: &Authorization,
    ctx: &mut ExecuteContext,
) -> ToolOutcome {
    execute_with_mcp(prepared, authorization, ctx, None::<&mut dyn McpDispatch>)
}

/// Execute with the host's MCP dispatch port attached. An MCP call still
/// requires an [`Authorization`] whose intent hash matches the prepared intent,
/// exactly like a built-in; an absent port fails closed.
pub fn execute_with_mcp<D: McpDispatch + ?Sized>(
    prepared: &PreparedTool,
    authorization: &Authorization,
    ctx: &mut ExecuteContext,
    mcp: Option<&mut D>,
) -> ToolOutcome {
    let started = Instant::now();
    if authorization.intent_hash() != intent_hash(&prepared.intent) {
        return ToolOutcome::failed(
            format!("{}: authorization mismatch", prepared.tool_name),
            ToolError::new(
                ErrorCode::ApprovalStale,
                "authorization does not match the prepared intent",
            ),
            started,
        );
    }
    match &prepared.action {
        PreparedAction::ReadFile {
            path,
            offset,
            limit,
        } => match ctx.fs.read_text(path, *offset, *limit) {
            Ok(read) => {
                let data = ReadFileData {
                    text: read.text,
                    start_line: read.start_line,
                    end_line: read.end_line,
                    total_lines: read.total_lines,
                    sha256: read.sha256,
                    truncated: read.truncated,
                };
                ToolOutcome::done(
                    ToolStatus::Succeeded,
                    format!(
                        "read {} (lines {}-{} of {})",
                        path, data.start_line, data.end_line, data.total_lines
                    ),
                    serde_json::to_value(&data).expect("serializable"),
                    data.truncated,
                    started,
                    None,
                )
            }
            Err(err) => ToolOutcome::failed(
                format!("read {path} failed"),
                err.into(),
                started,
            ),
        },
        PreparedAction::Search {
            pattern,
            path,
            max_results,
        } => match ctx.fs.resolve(path) {
            Ok(real) => match search(&real, path, pattern, *max_results) {
                Ok(result) => {
                    let data = SearchData {
                        count: result.matches.len(),
                        truncated: result.truncated,
                        matches: result.matches,
                    };
                    ToolOutcome::done(
                        ToolStatus::Succeeded,
                        format!(
                            "search {:?}: {} match(es) in {} file(s){}",
                            pattern,
                            data.count,
                            result.files_scanned,
                            if data.truncated { " (truncated)" } else { "" }
                        ),
                        serde_json::to_value(&data).expect("serializable"),
                        data.truncated,
                        started,
                        None,
                    )
                }
                Err(err) => ToolOutcome::failed(
                    format!("search {pattern:?} failed"),
                    err,
                    started,
                ),
            },
            Err(err) => ToolOutcome::failed(
                format!("search path {path} rejected"),
                err.into(),
                started,
            ),
        },
        PreparedAction::WriteFile {
            path,
            content,
            expected_sha256,
        } => {
            let real = match ctx.fs.resolve(path) {
                Ok(real) => real,
                Err(err) => {
                    return ToolOutcome::failed(
                        format!("write {path} rejected"),
                        err.into(),
                        started,
                    )
                }
            };
            let before = std::fs::read(&real).ok();
            let checkpoint = ctx.checkpoints.capture(path, before.as_deref());
            match ctx.fs.write_text(path, content, expected_sha256.as_deref()) {
                Ok(write) => {
                    if let Ok(after) = std::fs::read(&real) {
                        let _ = ctx.checkpoints.mark_post(&checkpoint.id, &after);
                    }
                    let data = WriteFileData {
                        pre_sha256: write.pre_sha256.clone(),
                        post_sha256: write.post_sha256.clone(),
                        bytes: write.bytes,
                        checkpoint_id: checkpoint.id.to_string(),
                    };
                    let mut outcome = ToolOutcome::done(
                        ToolStatus::Succeeded,
                        format!("wrote {} ({} bytes)", path, write.bytes),
                        serde_json::to_value(&data).expect("serializable"),
                        false,
                        started,
                        None,
                    );
                    outcome.preimage = write.pre_sha256;
                    outcome.postimage = Some(write.post_sha256);
                    outcome
                }
                Err(err) => ToolOutcome::failed(
                    format!("write {path} failed"),
                    err.into(),
                    started,
                ),
            }
        }
        PreparedAction::ApplyPatch {
            path,
            old_text,
            new_text,
            expected_sha256,
        } => {
            let real = match ctx.fs.resolve(path) {
                Ok(real) => real,
                Err(err) => {
                    return ToolOutcome::failed(
                        format!("patch {path} rejected"),
                        err.into(),
                        started,
                    )
                }
            };
            let before = std::fs::read(&real).ok();
            let checkpoint = ctx.checkpoints.capture(path, before.as_deref());
            match ctx
                .fs
                .patch_text(path, old_text, new_text, expected_sha256)
            {
                Ok(patch) => {
                    if let Ok(after) = std::fs::read(&real) {
                        let _ = ctx.checkpoints.mark_post(&checkpoint.id, &after);
                    }
                    let data = ApplyPatchData {
                        pre_sha256: patch.pre_sha256.clone(),
                        post_sha256: patch.post_sha256.clone(),
                        replaced_bytes: patch.replaced_bytes,
                        checkpoint_id: checkpoint.id.to_string(),
                    };
                    let mut outcome = ToolOutcome::done(
                        ToolStatus::Succeeded,
                        format!("patched {} ({} bytes replaced)", path, patch.replaced_bytes),
                        serde_json::to_value(&data).expect("serializable"),
                        false,
                        started,
                        None,
                    );
                    outcome.preimage = Some(patch.pre_sha256);
                    outcome.postimage = Some(patch.post_sha256);
                    outcome
                }
                Err(err) => ToolOutcome::failed(
                    format!("patch {path} failed"),
                    err.into(),
                    started,
                ),
            }
        }
        PreparedAction::Exec {
            argv,
            cwd,
            timeout,
        } => {
            let resolved_cwd = match ctx.fs.resolve(cwd) {
                Ok(path) => path,
                Err(err) => {
                    return ToolOutcome::failed(
                        format!("exec cwd {cwd} rejected"),
                        err.into(),
                        started,
                    )
                }
            };
            let request = ExecRequest {
                argv: argv.clone(),
                cwd: resolved_cwd,
                timeout: *timeout,
                env: BTreeMap::new(),
                max_output_bytes: ctx.max_output_bytes,
            };
            let outcome = run_child(ctx, &request);
            let status = match (outcome.status, outcome.reaped, outcome.exit_code) {
                (ExecStatus::Exited, _, Some(0)) => ToolStatus::Succeeded,
                (ExecStatus::Exited, _, _) => ToolStatus::Failed,
                (ExecStatus::TimedOut, true, _) => ToolStatus::Failed,
                (ExecStatus::TimedOut, false, _) => ToolStatus::Unknown,
                (ExecStatus::FailedToStart, _, _) => ToolStatus::Failed,
            };
            let error = match outcome.status {
                ExecStatus::TimedOut if outcome.reaped => Some(ToolError::new(
                    ErrorCode::ToolTimeout,
                    "command timed out and its process group was terminated",
                )),
                ExecStatus::TimedOut => Some(ToolError::new(
                    ErrorCode::OperationUnknown,
                    "command timed out and could not be confirmed reaped; effects may be unknown",
                )),
                ExecStatus::FailedToStart => Some(ToolError::new(
                    ErrorCode::Internal,
                    outcome.stderr.clone(),
                )),
                ExecStatus::Exited => None,
            };
            let data = ExecData {
                exit_code: outcome.exit_code,
                stdout: outcome.stdout,
                stderr: outcome.stderr,
                truncated: outcome.truncated,
                duration_ms: outcome.duration_ms,
                reaped: outcome.reaped,
                status: format!("{:?}", outcome.status).to_ascii_lowercase(),
            };
            let summary = match outcome.exit_code {
                Some(code) => format!("exec {} (exit {code})", argv_summary(argv)),
                None => format!("exec {} (no observed exit)", argv_summary(argv)),
            };
            ToolOutcome::done(
                status,
                summary,
                serde_json::to_value(&data).expect("serializable"),
                data.truncated,
                started,
                error,
            )
        }
        PreparedAction::GitStatus => git_outcome(ctx, started, false, None),
        PreparedAction::GitDiff { path } => git_outcome(ctx, started, true, path.as_deref()),
        PreparedAction::Mcp {
            canonical_id,
            server_id,
            tool_name,
            arguments,
        } => {
            let Some(dispatch) = mcp else {
                return ToolOutcome::failed(
                    format!("{canonical_id}: no MCP dispatch attached"),
                    ToolError::new(ErrorCode::McpError, "this run has no MCP dispatch attached"),
                    started,
                );
            };
            let dispatched = dispatch.dispatch(canonical_id, arguments);
            let data = serde_json::json!({
                "server": server_id,
                "tool": tool_name,
                "content": dispatched.content,
                "is_error": dispatched.is_error,
                "truncated": dispatched.truncated,
            });
            let summary = if dispatched.summary.trim().is_empty() {
                format!("mcp {server_id}/{tool_name}")
            } else {
                dispatched.summary.clone()
            };
            ToolOutcome::done(
                dispatched.status,
                summary,
                data,
                dispatched.truncated,
                started,
                dispatched.error,
            )
        }
    }
}

fn git_outcome(
    ctx: &mut ExecuteContext,
    started: Instant,
    diff: bool,
    path: Option<&str>,
) -> ToolOutcome {
    let root: PathBuf = match ctx.fs.resolve(".") {
        Ok(root) => root,
        Err(err) => return ToolOutcome::failed("git root rejected".into(), err.into(), started),
    };
    let mut argv: Vec<String> = if diff {
        vec![
            "git".into(),
            "-c".into(),
            "core.fsmonitor=false".into(),
            "diff".into(),
            "--no-ext-diff".into(),
            "--no-textconv".into(),
            "--no-color".into(),
        ]
    } else {
        vec![
            "git".into(),
            "-c".into(),
            "core.fsmonitor=false".into(),
            "status".into(),
            "--porcelain=v1".into(),
            "--branch".into(),
        ]
    };
    if let Some(raw) = path {
        let real = match ctx.fs.resolve(raw) {
            Ok(real) => real,
            Err(err) => {
                return ToolOutcome::failed(format!("git path {raw} rejected"), err.into(), started)
            }
        };
        let relative = real
            .strip_prefix(&root)
            .unwrap_or(&real)
            .to_string_lossy()
            .replace('\\', "/");
        if relative != "." && relative != "" {
            argv.push("--".into());
            argv.push(relative);
        }
    }
    let mut env = BTreeMap::new();
    env.insert("GIT_TERMINAL_PROMPT".to_string(), "0".to_string());
    env.insert("GIT_OPTIONAL_LOCKS".to_string(), "0".to_string());
    env.insert("GIT_PAGER".to_string(), "cat".to_string());
    let request = ExecRequest {
        argv: argv.clone(),
        cwd: root.clone(),
        timeout: ctx.git_timeout,
        env,
        max_output_bytes: ctx.max_output_bytes,
    };
    let outcome = run_child(ctx, &request);
    let data = GitData {
        stdout: if outcome.stdout.is_empty() {
            outcome.stderr.clone()
        } else {
            outcome.stdout.clone()
        },
        truncated: outcome.truncated,
    };
    let summary = if diff { "git diff" } else { "git status" };
    match outcome.status {
        ExecStatus::Exited => ToolOutcome::done(
            if outcome.exit_code == Some(0) {
                ToolStatus::Succeeded
            } else {
                ToolStatus::Failed
            },
            format!("{summary} (exit {:?})", outcome.exit_code),
            serde_json::to_value(&data).expect("serializable"),
            data.truncated,
            started,
            None,
        ),
        ExecStatus::TimedOut => ToolOutcome::failed(
            format!("{summary} timed out"),
            ToolError::new(ErrorCode::ToolTimeout, "git command timed out"),
            started,
        ),
        ExecStatus::FailedToStart => ToolOutcome::failed(
            format!("{summary} failed to start"),
            ToolError::new(
                ErrorCode::Internal,
                "git executable not found or not startable",
            ),
            started,
        ),
    }
}

fn argv_summary(argv: &[String]) -> String {
    let joined = argv
        .iter()
        .take(4)
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    if argv.len() > 4 {
        format!("{joined} …")
    } else {
        joined
    }
}

/// Heuristic used only to decide whether to *label* a completed exec as
/// verification evidence. It never changes permissions or execution.
pub fn looks_like_verification(argv: &[String]) -> bool {
    let joined = argv
        .iter()
        .map(|arg| arg.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    [
        "cargo test",
        "cargo nextest",
        "npm test",
        "npm run test",
        "pnpm test",
        "yarn test",
        "pytest",
        "go test",
        "dotnet test",
        "mvn test",
        "gradle test",
    ]
    .iter()
    .any(|needle| joined.contains(needle))
}

/// Root accessor helper for callers that already resolved the workspace.
pub fn workspace_root_of(fs: &WorkspaceFs) -> Result<PathBuf, ToolError> {
    fs.resolve(".").map_err(ToolError::from)
}


#[cfg(test)]
mod tests {
    use super::*;
    use bollo_policy::approval::MemoryApprovalStore;
    use bollo_policy::evaluate::PolicyDecision;
    use bollo_policy::gate::{authorize, AuthMode};
    use bollo_policy::normalize::effect_class_for;
    use bollo_policy::ApprovalExpectation;
    use bollo_protocol::vocab::Effect;
    use bollo_workspace::root::WorkspaceRoot;
    use serde_json::json;
    use time::OffsetDateTime;

    fn setup() -> (tempfile::TempDir, WorkspaceFs, CheckpointLog) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        let root = WorkspaceRoot::discover(dir.path()).unwrap();
        let fs = WorkspaceFs::new(&root);
        (dir, fs, CheckpointLog::new())
    }

    fn authorize_allow(prepared: &PreparedTool) -> Authorization {
        let decision = PolicyDecision {
            effect: Effect::Allow,
            rule_id: None,
            provenance: Vec::new(),
            reason: "test allow".into(),
        };
        let expect = ApprovalExpectation {
            intent_hash: intent_hash(&prepared.intent),
            policy_revision: 1,
            workspace_identity: "ws".into(),
            target_preimage: prepared.target_preimage.clone(),
        };
        let mut store = MemoryApprovalStore::new();
        authorize(
            &decision,
            &prepared.intent,
            None,
            &expect,
            &mut store,
            OffsetDateTime::now_utc(),
        )
        .unwrap()
    }

    fn prepare_tool(tool: &str, args: serde_json::Value, fs: &WorkspaceFs) -> PreparedTool {
        crate::prepare(tool, &args, fs, &fs.policy_root()).unwrap()
    }

    #[test]
    fn read_file_executes_with_authorization() {
        let (_dir, fs, mut checkpoints) = setup();
        let prepared = prepare_tool("read_file", json!({"path": "main.rs"}), &fs);
        let auth = authorize_allow(&prepared);
        let mut ctx = ExecuteContext::new(&fs, &mut checkpoints, 4096);
        let outcome = execute(&prepared, &auth, &mut ctx);
        assert_eq!(outcome.status, ToolStatus::Succeeded);
        assert!(outcome.data["text"].as_str().unwrap().contains("fn main"));
    }

    #[test]
    fn mismatched_authorization_is_refused() {
        let (_dir, fs, mut checkpoints) = setup();
        let first = prepare_tool("read_file", json!({"path": "main.rs"}), &fs);
        let second = prepare_tool("read_file", json!({"path": "main.rs", "limit": 5}), &fs);
        let auth = authorize_allow(&first);
        let mut ctx = ExecuteContext::new(&fs, &mut checkpoints, 4096);
        let outcome = execute(&second, &auth, &mut ctx);
        assert_eq!(outcome.status, ToolStatus::Failed);
        assert_eq!(outcome.error.unwrap().code, ErrorCode::ApprovalStale);
    }

    #[test]
    fn write_and_patch_create_checkpoints() {
        let (_dir, fs, mut checkpoints) = setup();
        let prepared = prepare_tool(
            "write_file",
            json!({"path": "new.txt", "content": "hello", "expected_sha256": null}),
            &fs,
        );
        let auth = authorize_allow(&prepared);
        {
            let mut ctx = ExecuteContext::new(&fs, &mut checkpoints, 4096);
            let outcome = execute(&prepared, &auth, &mut ctx);
            assert_eq!(outcome.status, ToolStatus::Succeeded);
            assert!(outcome.data["checkpoint_id"].as_str().is_some());
        }
        assert_eq!(checkpoints.len(), 1);

        let hash = fs.current_sha256("new.txt").unwrap();
        let patch = prepare_tool(
            "apply_patch",
            json!({"path": "new.txt", "old_text": "hello", "new_text": "world", "expected_sha256": hash}),
            &fs,
        );
        let auth = authorize_allow(&patch);
        let mut ctx = ExecuteContext::new(&fs, &mut checkpoints, 4096);
        let outcome = execute(&patch, &auth, &mut ctx);
        assert_eq!(outcome.status, ToolStatus::Succeeded);
        assert_eq!(fs.read_text("new.txt", None, None).unwrap().text, "world");
    }

    #[test]
    fn stale_write_is_refused_at_execution() {
        let (_dir, fs, mut checkpoints) = setup();
        let prepared = prepare_tool(
            "write_file",
            json!({"path": "main.rs", "content": "new", "expected_sha256": "0".repeat(64)}),
            &fs,
        );
        let auth = authorize_allow(&prepared);
        let mut ctx = ExecuteContext::new(&fs, &mut checkpoints, 4096);
        let outcome = execute(&prepared, &auth, &mut ctx);
        assert_eq!(outcome.status, ToolStatus::Failed);
        assert_eq!(outcome.error.unwrap().code, ErrorCode::PreimageConflict);
        assert_eq!(fs.read_text("main.rs", None, None).unwrap().text, "fn main() {}");
    }

    #[test]
    fn exec_runs_with_filtered_environment() {
        let (_dir, fs, mut checkpoints) = setup();
        #[cfg(windows)]
        let argv = json!(["cmd", "/C", "echo tool-ok"]);
        #[cfg(unix)]
        let argv = json!(["sh", "-c", "echo tool-ok"]);
        let prepared = prepare_tool(
            "exec",
            json!({"argv": argv, "cwd": ".", "timeout_seconds": 20}),
            &fs,
        );
        let auth = authorize_allow(&prepared);
        let mut ctx = ExecuteContext::new(&fs, &mut checkpoints, 4096);
        let outcome = execute(&prepared, &auth, &mut ctx);
        assert_eq!(outcome.status, ToolStatus::Succeeded);
        assert!(outcome.data["stdout"].as_str().unwrap().contains("tool-ok"));
    }

    /// Records broker routing: every child offered to this backend is counted
    /// and answered with canned output, so a test can prove a child never ran
    /// on the host.
    #[derive(Default)]
    struct RecordingSandbox {
        calls: std::cell::Cell<usize>,
    }

    impl ChildSandbox for RecordingSandbox {
        fn run_with_stdin(
            &self,
            request: &ExecRequest,
            _stdin_bytes: Option<&[u8]>,
        ) -> ExecOutcome {
            self.calls.set(self.calls.get() + 1);
            ExecOutcome {
                status: ExecStatus::Exited,
                exit_code: Some(0),
                stdout: format!("sandboxed:{}", request.argv.join(" ")),
                stderr: String::new(),
                truncated: false,
                duration_ms: 0,
                reaped: true,
            }
        }
    }

    #[test]
    fn exec_is_routed_through_the_attached_sandbox() {
        let (_dir, fs, mut checkpoints) = setup();
        #[cfg(windows)]
        let argv = json!(["cmd", "/C", "echo host-only"]);
        #[cfg(unix)]
        let argv = json!(["sh", "-c", "echo host-only"]);
        let prepared = prepare_tool(
            "exec",
            json!({"argv": argv, "cwd": ".", "timeout_seconds": 20}),
            &fs,
        );
        let auth = authorize_allow(&prepared);
        let sandbox = RecordingSandbox::default();
        let mut ctx = ExecuteContext::new(&fs, &mut checkpoints, 4096);
        ctx.sandbox = Some(&sandbox);
        let outcome = execute(&prepared, &auth, &mut ctx);
        assert_eq!(outcome.status, ToolStatus::Succeeded);
        assert_eq!(sandbox.calls.get(), 1);
        // The host command would print exactly `host-only`; this output can
        // only come from the sandbox backend.
        let stdout = outcome.data["stdout"].as_str().unwrap();
        assert!(stdout.contains("sandboxed:"), "stdout: {stdout}");
        assert_ne!(stdout.trim(), "host-only");
    }

    #[test]
    fn git_children_take_the_same_route_as_exec() {
        let (dir, fs, mut checkpoints) = setup();
        let sandbox = RecordingSandbox::default();
        let mut ctx = ExecuteContext::new(&fs, &mut checkpoints, 4096);
        ctx.sandbox = Some(&sandbox);
        let request = ExecRequest {
            argv: vec!["git".into(), "status".into()],
            cwd: dir.path().to_path_buf(),
            timeout: Duration::from_secs(5),
            env: std::collections::BTreeMap::new(),
            max_output_bytes: 4096,
        };
        let outcome = run_child(&ctx, &request);
        assert_eq!(sandbox.calls.get(), 1);
        assert!(outcome.stdout.contains("sandboxed:git status"));
    }

    #[test]
    fn verification_heuristic_labels_test_commands() {
        assert!(looks_like_verification(&["cargo".into(), "test".into()]));
        assert!(looks_like_verification(&["npm".into(), "test".into()]));
        assert!(!looks_like_verification(&["cargo".into(), "build".into(), "--release".into()]));
    }

    #[test]
    fn execute_returns_allow_mode_token_scope() {
        let (_dir, fs, _checkpoints) = setup();
        let prepared = prepare_tool("read_file", json!({"path": "main.rs"}), &fs);
        let auth = authorize_allow(&prepared);
        assert_eq!(auth.mode(), AuthMode::PolicyAllow);
    }

    #[test]
    fn effect_helper_compiles_for_exec() {
        assert_eq!(
            effect_class_for(bollo_protocol::vocab::ToolClass::Exec, false),
            bollo_protocol::vocab::EffectClass::Execution
        );
    }

    fn prepare_mcp_echo(fs: &WorkspaceFs) -> PreparedTool {
        let spec = crate::McpToolSpec {
            canonical_id: "mcp:fake:echo".into(),
            provider_name: crate::provider_safe_tool_name("mcp:fake:echo"),
            server_id: "fake".into(),
            tool_name: "echo".into(),
            description: "echo text".into(),
            input_schema: json!({"type": "object"}),
        };
        crate::prepare_with_catalog(
            "mcp_fake_echo",
            &json!({"text": "hello"}),
            fs,
            &fs.policy_root(),
            &[spec],
        )
        .unwrap()
    }

    #[derive(Default)]
    struct EchoDispatch {
        calls: usize,
        unknown: bool,
    }

    impl McpDispatch for EchoDispatch {
        fn dispatch(&mut self, canonical_id: &str, arguments: &Value) -> McpDispatchOutcome {
            self.calls += 1;
            assert_eq!(canonical_id, "mcp:fake:echo");
            assert_eq!(arguments["text"], "hello");
            if self.unknown {
                return McpDispatchOutcome::unknown("mcp server fake disconnected mid-call");
            }
            let mut outcome = McpDispatchOutcome::from_result(
                vec![json!({"type": "text", "text": "echo:hello"})],
                false,
                false,
            );
            outcome.summary = "mcp fake / echo: echo:hello".into();
            outcome
        }
    }

    #[test]
    fn mcp_calls_dispatch_once_after_authorization() {
        let (_dir, fs, mut checkpoints) = setup();
        let prepared = prepare_mcp_echo(&fs);
        let auth = authorize_allow(&prepared);
        let mut dispatch = EchoDispatch::default();
        {
            let mut ctx = ExecuteContext::new(&fs, &mut checkpoints, 4096);
            let outcome = execute_with_mcp(&prepared, &auth, &mut ctx, Some(&mut dispatch));
            assert_eq!(outcome.status, ToolStatus::Succeeded);
            assert_eq!(outcome.data["server"], "fake");
            assert_eq!(outcome.data["content"][0]["text"], "echo:hello");
            assert!(outcome.summary.contains("echo:hello"));
        }
        assert_eq!(dispatch.calls, 1);
    }

    #[test]
    fn mcp_transport_failure_is_unknown_and_not_retried() {
        let (_dir, fs, mut checkpoints) = setup();
        let prepared = prepare_mcp_echo(&fs);
        let auth = authorize_allow(&prepared);
        let mut dispatch = EchoDispatch {
            calls: 0,
            unknown: true,
        };
        {
            let mut ctx = ExecuteContext::new(&fs, &mut checkpoints, 4096);
            let outcome = execute_with_mcp(&prepared, &auth, &mut ctx, Some(&mut dispatch));
            assert_eq!(outcome.status, ToolStatus::Unknown);
            assert_eq!(outcome.error.unwrap().code, ErrorCode::OperationUnknown);
        }
        assert_eq!(dispatch.calls, 1, "an uncertain call is never replayed");
    }

    #[test]
    fn mcp_call_without_a_dispatch_port_fails_closed() {
        let (_dir, fs, mut checkpoints) = setup();
        let prepared = prepare_mcp_echo(&fs);
        let auth = authorize_allow(&prepared);
        let mut ctx = ExecuteContext::new(&fs, &mut checkpoints, 4096);
        let outcome = execute(&prepared, &auth, &mut ctx);
        assert_eq!(outcome.status, ToolStatus::Failed);
        assert_eq!(outcome.error.unwrap().code, ErrorCode::McpError);
    }
}
