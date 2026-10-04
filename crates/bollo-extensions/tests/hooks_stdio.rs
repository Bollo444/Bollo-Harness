//! Hook behavior against the native fixture binary: veto, failure, timeout,
//! stdin delivery, and the rule that after-hook failure cannot falsify results.

use bollo_extensions::{HookEvent, HookOutcome, HookPayload, HookSpec, TrustStore};
use bollo_extensions::{run_after_hook, run_before_hook};
use bollo_protocol::cancel::CancellationToken;

fn payload(event: HookEvent, result_status: Option<&str>) -> HookPayload {
    HookPayload {
        schema_version: "0.1".into(),
        event,
        session_id: "sess_test".into(),
        run_id: "run_test".into(),
        tool_call_id: "call_test".into(),
        tool_name: "write_file".into(),
        intent_hash: "a".repeat(64),
        summary: "write a file".into(),
        result_status: result_status.map(str::to_string),
    }
}

fn spec(scenario: &str, event: HookEvent, timeout_seconds: u32) -> HookSpec {
    HookSpec {
        id: format!("fixture-{scenario}"),
        event,
        enabled: true,
        argv: vec![
            env!("CARGO_BIN_EXE_bollo-hook-fixture").to_string(),
            scenario.to_string(),
        ],
        timeout_seconds,
        env_names: Vec::new(),
    }
}

fn trusted(spec: &HookSpec, cwd: &std::path::Path) -> TrustStore {
    let mut trust = TrustStore::new();
    trust.grant(&spec.argv, cwd, &spec.env_names).unwrap();
    trust
}

fn before(scenario: &str, timeout_seconds: u32) -> HookOutcome {
    let cwd = std::env::current_dir().unwrap();
    let spec = spec(scenario, HookEvent::BeforeTool, timeout_seconds);
    let trust = trusted(&spec, &cwd);
    run_before_hook(
        &spec,
        &payload(HookEvent::BeforeTool, None),
        &cwd,
        &trust,
        &CancellationToken::new(),
    )
}

#[test]
fn continue_block_invalid_and_nonzero() {
    assert_eq!(before("continue", 10), HookOutcome::Continue);

    match before("block", 10) {
        HookOutcome::Denied { reason } => assert!(reason.contains("fixture block")),
        other => panic!("expected block denial, got {other:?}"),
    }

    assert!(matches!(before("invalid", 10), HookOutcome::Denied { .. }));
    assert!(matches!(before("exit3", 10), HookOutcome::Denied { .. }));
    assert!(matches!(before("empty", 10), HookOutcome::Denied { .. }));
}

#[test]
fn timeout_denies_the_action() {
    match before("sleep", 1) {
        HookOutcome::Denied { reason } => assert!(reason.contains("timed out")),
        other => panic!("expected timeout denial, got {other:?}"),
    }
}

#[test]
fn payload_is_delivered_on_stdin() {
    let cwd = std::env::current_dir().unwrap();
    let spec = spec("echo-stdin", HookEvent::AfterTool, 10);
    let trust = trusted(&spec, &cwd);
    match run_after_hook(
        &spec,
        &payload(HookEvent::AfterTool, Some("succeeded")),
        &cwd,
        &trust,
    ) {
        HookOutcome::Recorded { ok, note } => {
            assert!(ok);
            assert!(note.contains("stdin-len:"), "note was {note:?}");
            assert!(!note.contains("stdin-len:0"), "payload must not be empty");
        }
        other => panic!("unexpected outcome {other:?}"),
    }
}

#[test]
fn after_hook_failure_cannot_rewrite_the_result() {
    let cwd = std::env::current_dir().unwrap();
    let spec = spec("exit3", HookEvent::AfterTool, 10);
    let trust = trusted(&spec, &cwd);
    match run_after_hook(
        &spec,
        &payload(HookEvent::AfterTool, Some("succeeded")),
        &cwd,
        &trust,
    ) {
        HookOutcome::Recorded { ok, note } => {
            assert!(!ok);
            assert!(note.contains("exited"));
        }
        other => panic!("unexpected outcome {other:?}"),
    }
}
