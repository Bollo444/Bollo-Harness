//! Golden scenario and loop-level negatives.
//!
//! The scenario is a real dirty repository: the fixture's `add` is wrong, the
//! loop patches it behind an approval, runs `cargo test` through the exec
//! broker, labels verification separately from completion, and the recorded
//! checkpoint restores the preimage afterwards.

use bollo_core::{NoChannel, ScriptedChannel};
use bollo_protocol::vocab::{TerminalState, VerificationStatus};
use bollo_store::OperationState;
use bollo_tests::{tool_intent, tool_response, Harness};
use bollo_workspace::RestorePlan;
use serde_json::json;

const FIX: &str = "a + b";
const BROKEN: &str = "a - b";

#[test]
fn golden_edit_verify_restore() {
    let mut harness = Harness::new(bollo_protocol::vocab::Profile::Balanced, 10, Vec::new());
    harness.seed_broken_cargo_project();
    let original_hash = harness.fs.current_sha256("src/lib.rs").unwrap();
    harness.set_script(vec![
        tool_response(vec![tool_intent(
            "patch_1",
            "apply_patch",
            json!({
                "path": "src/lib.rs",
                "old_text": BROKEN,
                "new_text": FIX,
                "expected_sha256": original_hash
            }),
        )]),
        tool_response(vec![tool_intent(
            "test_1",
            "exec",
            json!({"argv": ["cargo", "test"], "cwd": ".", "timeout_seconds": 300}),
        )]),
        bollo_providers::fake::ScriptedResponse::text("fixed and verified"),
    ]);

    // One approval for the mutation, one for the execution.
    let (outcome, events) = harness.run("fix the add function and run the tests", vec![true, true]);

    assert_eq!(outcome.state, TerminalState::Completed);
    assert_eq!(outcome.exit_code, 0);
    assert_eq!(
        outcome.verification,
        VerificationStatus::Passed,
        "the fixture test must be observed as passed"
    );
    assert!(harness.read("src/lib.rs").contains(FIX));

    // Verification is reported as its own event, not folded into completion.
    let verification = events
        .iter()
        .find(|event| event.event_type == bollo_protocol::EventType::VerificationResult)
        .expect("verification_result event");
    assert_eq!(verification.data["status"], "passed");
    assert_eq!(verification.data["command"][0], "cargo");
    let finished = events
        .iter()
        .find(|event| event.event_type == bollo_protocol::EventType::RunFinished)
        .expect("run_finished event");
    assert_eq!(finished.data["state"], "completed");
    assert_eq!(finished.data["verification"], "passed");

    // Nothing is left unknown after a clean run.
    let operations = harness
        .store
        .operations_for_session(&harness.session)
        .unwrap();
    assert!(operations
        .iter()
        .all(|operation| operation.state == OperationState::Succeeded));
    assert_eq!(operations.len(), 2, "one mutation and one verification");

    // The checkpoint is persisted and restores only while the postimage matches.
    let stored = harness
        .store
        .checkpoints_for_session(&harness.session)
        .unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].path, "src/lib.rs");
    let post = stored[0].post_sha256.clone().expect("postimage recorded");
    assert_eq!(harness.fs.current_sha256("src/lib.rs").unwrap(), post);

    let checkpoint = harness.checkpoints.all().next().unwrap().clone();
    let plan = harness
        .checkpoints
        .preview(&checkpoint.id, Some(&post))
        .expect("restore plan");
    match plan {
        RestorePlan::WritePreimage { path, text } => {
            assert_eq!(path, "src/lib.rs");
            assert!(text.contains(BROKEN), "preimage is the original broken code");
            harness
                .fs
                .write_text(&path, &text, Some(&post))
                .expect("conditional restore applies");
        }
        other => panic!("unexpected plan {other:?}"),
    }
    assert!(harness.read("src/lib.rs").contains(BROKEN));

    // After our own restore the file no longer matches the recorded postimage,
    // so a second restore must refuse rather than clobber.
    assert!(harness
        .checkpoints
        .preview(&checkpoint.id, harness.fs.current_sha256("src/lib.rs").as_deref())
        .is_err());
}

#[test]
fn failing_verification_is_completion_not_success() {
    let mut harness = Harness::new(bollo_protocol::vocab::Profile::Balanced, 10, Vec::new());
    harness.seed_broken_cargo_project();
    harness.set_script(vec![
        tool_response(vec![tool_intent(
            "test_1",
            "exec",
            json!({"argv": ["cargo", "test"], "cwd": ".", "timeout_seconds": 300}),
        )]),
        bollo_providers::fake::ScriptedResponse::text("ran the tests"),
    ]);

    let (outcome, _events) = harness.run("run the tests", vec![true]);
    assert_eq!(outcome.state, TerminalState::Completed);
    assert_eq!(outcome.exit_code, 0, "exit 0 only means the run completed");
    assert_eq!(outcome.verification, VerificationStatus::Failed);
}

#[test]
fn headless_ask_blocks_without_defaulting_to_yes() {
    let mut harness = Harness::new(bollo_protocol::vocab::Profile::Balanced, 10, Vec::new());
    harness.set_script(vec![tool_response(vec![tool_intent(
        "write_1",
        "write_file",
        json!({"path": "new.txt", "content": "nope\n", "expected_sha256": null}),
    )])]);

    let mut channel = NoChannel;
    let (outcome, events) = harness.run_with_channel("write a file", &mut channel);
    assert_eq!(outcome.state, TerminalState::Blocked);
    assert_eq!(outcome.exit_code, 3);
    assert_eq!(outcome.reason.as_deref(), Some("approval_required"));
    assert!(!harness.exists("new.txt"), "an ask is not an allow");
    assert!(events
        .iter()
        .any(|event| event.event_type == bollo_protocol::EventType::ApprovalRequested));
}

#[test]
fn read_only_denies_mutation_and_records_no_intent() {
    let mut harness = Harness::new(bollo_protocol::vocab::Profile::ReadOnly, 10, Vec::new());
    harness.set_script(vec![
        tool_response(vec![tool_intent(
            "write_1",
            "write_file",
            json!({"path": "new.txt", "content": "nope\n", "expected_sha256": null}),
        )]),
        bollo_providers::fake::ScriptedResponse::text("could not write"),
    ]);

    let (outcome, events) = harness.run("write a file", Vec::new());
    assert_eq!(outcome.state, TerminalState::Completed);
    assert!(!harness.exists("new.txt"));
    assert!(events.iter().any(|event| {
        event.event_type == bollo_protocol::EventType::ToolResult
            && event.data["status"] == "denied"
    }));
    assert!(
        harness
            .store
            .operations_for_session(&harness.session)
            .unwrap()
            .is_empty(),
        "denied proposals must not receive a journalled intent"
    );
    assert!(!events
        .iter()
        .any(|event| event.event_type == bollo_protocol::EventType::ApprovalRequested));
}

#[test]
fn run_limit_stops_the_loop_with_exit_4() {
    let mut harness = Harness::new(bollo_protocol::vocab::Profile::Balanced, 1, Vec::new());
    harness.seed("a.txt", "hello\n");
    harness.set_script(vec![
        tool_response(vec![tool_intent(
            "read_1",
            "read_file",
            json!({"path": "a.txt"}),
        )]),
        tool_response(vec![tool_intent(
            "read_2",
            "read_file",
            json!({"path": "a.txt"}),
        )]),
        bollo_providers::fake::ScriptedResponse::text("done"),
    ]);

    let (outcome, _events) = harness.run("read twice", Vec::new());
    assert_eq!(outcome.state, TerminalState::Failed);
    assert_eq!(outcome.reason.as_deref(), Some("run_limit_exceeded"));
    assert_eq!(outcome.exit_code, 4);
}

#[test]
fn cancellation_maps_to_130_and_runs_nothing() {
    let mut harness = Harness::new(bollo_protocol::vocab::Profile::Balanced, 10, Vec::new());
    harness.seed("a.txt", "hello\n");
    harness.set_script(vec![tool_response(vec![tool_intent(
        "read_1",
        "read_file",
        json!({"path": "a.txt"}),
    )])]);
    harness.cancel.cancel();

    let (outcome, events) = harness.run("read a file", Vec::new());
    assert_eq!(outcome.state, TerminalState::Cancelled);
    assert_eq!(outcome.exit_code, 130);
    assert!(events.iter().all(|event| {
        event.event_type != bollo_protocol::EventType::ToolResult
            && event.event_type != bollo_protocol::EventType::ToolProposed
    }));
}

#[test]
fn each_ask_needs_its_own_decision() {
    let mut harness = Harness::new(bollo_protocol::vocab::Profile::Balanced, 10, Vec::new());
    harness.set_script(vec![
        tool_response(vec![tool_intent(
            "write_1",
            "write_file",
            json!({"path": "one.txt", "content": "one\n", "expected_sha256": null}),
        )]),
        tool_response(vec![tool_intent(
            "write_2",
            "write_file",
            json!({"path": "two.txt", "content": "two\n", "expected_sha256": null}),
        )]),
        bollo_providers::fake::ScriptedResponse::text("one of two written"),
    ]);

    let mut channel = ScriptedChannel::new(vec![true, false]);
    let (outcome, _events) = harness.run_with_channel("write two files", &mut channel);
    assert_eq!(outcome.state, TerminalState::Completed);
    assert!(harness.exists("one.txt"));
    assert!(!harness.exists("two.txt"), "the second ask was denied");

    // The denied proposal never received a journalled intent, so only the
    // approved write exists in the operations ledger.
    let operations = harness
        .store
        .operations_for_session(&harness.session)
        .unwrap();
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].tool_call_id, "write_1");
    assert_eq!(operations[0].state, OperationState::Succeeded);
}
