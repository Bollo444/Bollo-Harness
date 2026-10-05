//! Trusted command hooks.
//!
//! Contract (docs/reference/hooks-and-plugins.md): a before-tool hook must exit
//! 0 with `{"decision":"continue"|"block","reason":...}`; block, timeout,
//! nonzero exit or invalid output denies the action. After-tool hooks are
//! recorded but can never rewrite the observed result. Hooks receive bounded
//! metadata JSON — never credentials or raw transcripts — and cannot enlarge
//! or authorize the action they surround.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use bollo_protocol::cancel::CancellationToken;
use bollo_workspace::process::{run_with_stdin, ChildSandbox, ExecRequest, ExecStatus};

use crate::trust::TrustStore;

pub const HOOK_STDIN_MAX_BYTES: usize = 64 * 1024;
pub const HOOK_STDOUT_MAX_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    BeforeTool,
    AfterTool,
}

#[derive(Debug, Clone)]
pub struct HookSpec {
    pub id: String,
    pub event: HookEvent,
    /// Enabled is not trusted; expiry requires a matching trust record.
    pub enabled: bool,
    pub argv: Vec<String>,
    pub timeout_seconds: u32,
    pub env_names: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HookPayload {
    pub schema_version: String,
    pub event: HookEvent,
    pub session_id: String,
    pub run_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub intent_hash: String,
    pub summary: String,
    pub result_status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookOutcome {
    /// Before: proceed to the policy gate. After: recorded, result unchanged.
    Continue,
    /// The action is denied (before hooks only).
    Denied { reason: String },
    /// After-hook bookkeeping: failure is recorded, never fatal to the result.
    Recorded { ok: bool, note: String },
}

pub fn run_before_hook(
    spec: &HookSpec,
    payload: &HookPayload,
    cwd: &Path,
    trust: &TrustStore,
    cancel: &CancellationToken,
    sandbox: Option<&dyn ChildSandbox>,
) -> HookOutcome {
    if spec.event != HookEvent::BeforeTool {
        return HookOutcome::Denied {
            reason: "misconfigured hook event".into(),
        };
    }
    if !spec.enabled {
        return HookOutcome::Continue;
    }
    if cancel.is_cancelled() {
        return HookOutcome::Denied {
            reason: "run cancelled before hook execution".into(),
        };
    }
    if !trust.verify(&spec.argv, cwd, &spec.env_names) {
        return HookOutcome::Denied {
            reason: format!(
                "hook {} has no matching trust record for this exact identity",
                spec.id
            ),
        };
    }
    match execute(spec, payload, cwd, sandbox) {
        Ok(stdout) => match serde_json::from_str::<serde_json::Value>(&stdout) {
            Ok(value) => match value["decision"].as_str() {
                Some("continue") => HookOutcome::Continue,
                Some("block") => HookOutcome::Denied {
                    reason: value["reason"]
                        .as_str()
                        .unwrap_or("blocked by required pre-hook")
                        .to_string(),
                },
                _ => HookOutcome::Denied {
                    reason: format!("hook {} returned an invalid decision", spec.id),
                },
            },
            Err(_) => HookOutcome::Denied {
                reason: format!("hook {} returned invalid JSON on stdout", spec.id),
            },
        },
        Err(reason) => HookOutcome::Denied { reason },
    }
}

/// Run an after-tool hook. Its failure is recorded and cannot erase or modify
/// the tool's already-observed outcome.
pub fn run_after_hook(
    spec: &HookSpec,
    payload: &HookPayload,
    cwd: &Path,
    trust: &TrustStore,
    sandbox: Option<&dyn ChildSandbox>,
) -> HookOutcome {
    if spec.event != HookEvent::AfterTool || !spec.enabled {
        return HookOutcome::Recorded {
            ok: true,
            note: "after-hook disabled".into(),
        };
    }
    if !trust.verify(&spec.argv, cwd, &spec.env_names) {
        return HookOutcome::Recorded {
            ok: false,
            note: format!("after-hook {} is not trusted", spec.id),
        };
    }
    match execute(spec, payload, cwd, sandbox) {
        Ok(stdout) => HookOutcome::Recorded {
            ok: true,
            note: format!("after-hook {} completed: {}", spec.id, truncate(&stdout, 256)),
        },
        Err(reason) => HookOutcome::Recorded {
            ok: false,
            note: reason,
        },
    }
}

fn execute(
    spec: &HookSpec,
    payload: &HookPayload,
    cwd: &Path,
    sandbox: Option<&dyn ChildSandbox>,
) -> Result<String, String> {
    let request = ExecRequest {
        argv: spec.argv.clone(),
        cwd: cwd.to_path_buf(),
        timeout: Duration::from_secs(spec.timeout_seconds.clamp(1, 30) as u64),
        env: BTreeMap::new(),
        max_output_bytes: HOOK_STDOUT_MAX_BYTES,
    };
    let mut payload_bytes =
        serde_json::to_vec(payload).map_err(|err| format!("payload serialization: {err}"))?;
    if payload_bytes.len() > HOOK_STDIN_MAX_BYTES {
        payload_bytes.truncate(HOOK_STDIN_MAX_BYTES);
    }
    let outcome = match sandbox {
        Some(sandbox) => sandbox.run_with_stdin(&request, Some(&payload_bytes)),
        None => run_with_stdin(&request, Some(&payload_bytes)),
    };
    match outcome.status {
        ExecStatus::Exited if outcome.exit_code == Some(0) => Ok(outcome.stdout),
        ExecStatus::Exited => Err(format!(
            "hook {} exited with {:?}",
            spec.id, outcome.exit_code
        )),
        ExecStatus::TimedOut => Err(format!("hook {} timed out", spec.id)),
        ExecStatus::FailedToStart => Err(format!(
            "hook {} failed to start: {}",
            spec.id, outcome.stderr
        )),
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        text.to_string()
    } else {
        format!("{}…", &text[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload() -> HookPayload {
        HookPayload {
            schema_version: "0.1".into(),
            event: HookEvent::BeforeTool,
            session_id: "sess_test".into(),
            run_id: "run_test".into(),
            tool_call_id: "call_test".into(),
            tool_name: "write_file".into(),
            intent_hash: "a".repeat(64),
            summary: "write a file".into(),
            result_status: None,
        }
    }

    fn hook_spec(argv: Vec<String>) -> HookSpec {
        HookSpec {
            id: "test-hook".into(),
            event: HookEvent::BeforeTool,
            enabled: true,
            argv,
            timeout_seconds: 10,
            env_names: Vec::new(),
        }
    }

    #[cfg(windows)]
    fn echo_script(text: &str) -> Vec<String> {
        vec!["cmd".into(), "/C".into(), format!("echo {text}")]
    }
    #[cfg(unix)]
    fn echo_script(text: &str) -> Vec<String> {
        vec!["sh".into(), "-c".into(), format!("echo '{text}'")]
    }

    // Spawning behavior (continue/block/invalid/exit/timeout/stdin) is covered
    // by tests/hooks_stdio.rs against the bollo-hook-fixture binary, so these
    // unit tests only exercise the pre-spawn trust rules.

    #[test]
    fn untrusted_hook_never_spawns() {
        let cwd = std::env::current_dir().unwrap();
        let spec = hook_spec(echo_script("{\"decision\":\"continue\"}"));
        assert!(matches!(
            run_before_hook(
                &spec,
                &payload(),
                &cwd,
                &TrustStore::new(),
                &CancellationToken::new(),
                None
            ),
            HookOutcome::Denied { .. }
        ));
    }

    #[test]
    fn disabled_hooks_do_nothing() {
        let cwd = std::env::current_dir().unwrap();
        let mut spec = hook_spec(echo_script("{\"decision\":\"block\"}"));
        spec.enabled = false;
        assert_eq!(
            run_before_hook(
                &spec,
                &payload(),
                &cwd,
                &TrustStore::new(),
                &CancellationToken::new(),
                None
            ),
            HookOutcome::Continue
        );
    }
}
