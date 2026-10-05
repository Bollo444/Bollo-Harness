//! BH-021 integration tests: the advisory classifier hook through the real
//! loop. The classifier is a deterministic `ScriptedClassifier`; every
//! escalation, zero-call and fail-neutral property is asserted end-to-end.
//!
//! The harness runs `sandbox=off` on the host and `workspace_auto` requires
//! verified containment, so allow-path tests use `unrestricted` with an
//! explicit profiles opt-in and ask-path tests use `balanced`.

use std::sync::Arc;

use bollo_core::runtime::NoChannel;
use bollo_core::ClassifierAudit;
use bollo_policy::classifier::{Availability, ClassifierVerdict, ScriptedClassifier};
use bollo_protocol::vocab::{Profile, TerminalState};
use bollo_protocol::EventType;
use bollo_providers::fake::ScriptedResponse;
use bollo_tests::{tool_intent, tool_response, Harness};
use serde_json::json;

fn write_intent(call_id: &str) -> bollo_providers::ToolIntent {
    tool_intent(
        call_id,
        "write_file",
        json!({"path": "notes.txt", "content": "hello\n", "expected_sha256": null}),
    )
}

fn read_intent(call_id: &str, path: &str) -> bollo_providers::ToolIntent {
    tool_intent(call_id, "read_file", json!({"path": path}))
}

fn escalating() -> ClassifierVerdict {
    ClassifierVerdict::available("jev-test", 1.2, 0.9, 0.0)
}

fn neutral() -> ClassifierVerdict {
    ClassifierVerdict::available("jev-test", 0.1, 0.9, 0.0)
}

fn scripted(verdicts: Vec<ClassifierVerdict>) -> Arc<ScriptedClassifier> {
    Arc::new(ScriptedClassifier::new("jev-test", verdicts))
}

/// Read back the audit persisted on the run record and check it round-trips.
fn stored_audit(harness: &Harness, outcome: &bollo_core::RunOutcome) -> ClassifierAudit {
    let detail = harness
        .store
        .run(&outcome.run_id)
        .expect("run row is readable");
    serde_json::from_str(
        detail
            .classifier_json
            .as_deref()
            .expect("an attached gate persists an audit"),
    )
    .expect("audit JSON round-trips")
}

#[test]
fn escalating_hint_turns_allow_into_ask_and_is_audited() {
    let mut harness = Harness::new(Profile::Unrestricted, 10, Vec::new());
    harness.set_script(vec![
        tool_response(vec![write_intent("w1")]),
        ScriptedResponse::text("stopped"),
    ]);
    let classifier = scripted(vec![escalating()]);
    harness.classifier = Some(classifier.clone());
    harness.classifier_profiles = vec![Profile::Unrestricted];

    let mut channel = NoChannel;
    let (outcome, events) = harness.run_with_channel("write notes", &mut channel);

    assert_eq!(outcome.state, TerminalState::Blocked);
    assert_eq!(outcome.exit_code, 3);
    assert!(!harness.exists("notes.txt"), "escalation is not an allow");
    assert_eq!(classifier.calls(), 1, "one eligible allow, one call");

    let proposed = events
        .iter()
        .find(|event| event.event_type == EventType::ToolProposed)
        .expect("tool.proposed");
    assert_eq!(proposed.data["decision"], "ask");
    assert!(
        proposed.data["reason"]
            .as_str()
            .unwrap()
            .contains("advisory classifier"),
        "hint must be recorded as advisory provenance: {}",
        proposed.data["reason"]
    );

    let requested = events
        .iter()
        .find(|event| event.event_type == EventType::ApprovalRequested)
        .expect("approval.requested");
    assert!(
        requested.data["summary"]
            .as_str()
            .unwrap()
            .contains("advisory classifier"),
        "the approver must see why the ask happened"
    );

    let states = classifier.states();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].tool, "write_file");
    assert_eq!(states[0].paths, vec!["notes.txt"]);
    assert_eq!(states[0].effect, "workspace_mutation");

    // Usage/audit reporting carries the activity: one call, available, one
    // escalation, unknown cost — never a new event type.
    assert!(outcome.classifier.attached);
    assert_eq!(outcome.classifier.calls, 1);
    assert_eq!(outcome.classifier.escalations, 1);
    assert_eq!(
        outcome
            .classifier
            .availability
            .get(&Availability::Available),
        Some(&1)
    );
    assert_eq!(outcome.classifier.cost_microusd, None);
    assert!(!outcome.classifier.cost_known);
    assert_eq!(
        outcome.classifier.summary_line(),
        "1 call · available 1 · 1 escalated · cost unknown"
    );
    let persisted = stored_audit(&harness, &outcome);
    assert_eq!(
        persisted, outcome.classifier,
        "run record mirrors the outcome"
    );
    assert!(
        events
            .iter()
            .all(|event| event.data.get("classifier").is_none()),
        "classifier activity never becomes an event payload"
    );
}

#[test]
fn neutral_hint_leaves_allow_unchanged_and_executes() {
    let mut harness = Harness::new(Profile::Unrestricted, 10, Vec::new());
    harness.set_script(vec![
        tool_response(vec![write_intent("w1")]),
        ScriptedResponse::text("done"),
    ]);
    let classifier = scripted(vec![neutral()]);
    harness.classifier = Some(classifier.clone());
    harness.classifier_profiles = vec![Profile::Unrestricted];

    let (outcome, events) = harness.run("write notes", Vec::new());

    assert_eq!(outcome.state, TerminalState::Completed);
    assert!(harness.exists("notes.txt"));
    assert_eq!(classifier.calls(), 1);
    let proposed = events
        .iter()
        .find(|event| event.event_type == EventType::ToolProposed)
        .expect("tool.proposed");
    assert_eq!(proposed.data["decision"], "allow");
    assert!(!proposed.data["reason"]
        .as_str()
        .unwrap()
        .contains("advisory classifier"));

    // A neutral verdict still counts as a call, with zero escalations, and is
    // persisted by the normal completion path.
    assert!(outcome.classifier.attached);
    assert_eq!(outcome.classifier.calls, 1);
    assert_eq!(outcome.classifier.escalations, 0);
    assert_eq!(stored_audit(&harness, &outcome), outcome.classifier);
}

#[test]
fn ask_policy_makes_zero_classifier_calls() {
    let mut harness = Harness::new(Profile::Balanced, 10, Vec::new());
    harness.set_script(vec![
        tool_response(vec![write_intent("w1")]),
        ScriptedResponse::text("done"),
    ]);
    let classifier = scripted(vec![escalating()]);
    harness.classifier = Some(classifier.clone());
    // Default profiles include balanced; eligibility alone must prevent calls.
    harness.classifier_profiles = vec![Profile::Balanced];

    let (outcome, events) = harness.run("write notes", vec![true]);

    assert_eq!(outcome.state, TerminalState::Completed);
    assert!(harness.exists("notes.txt"));
    assert_eq!(
        classifier.calls(),
        0,
        "ask decisions never call the classifier"
    );
    assert!(
        outcome.classifier.attached,
        "instrumented run reports zero calls"
    );
    assert!(outcome.classifier.is_empty());
    assert_eq!(stored_audit(&harness, &outcome), outcome.classifier);
    let requested = events
        .iter()
        .find(|event| event.event_type == EventType::ApprovalRequested)
        .expect("approval.requested");
    assert!(!requested.data["summary"]
        .as_str()
        .unwrap()
        .contains("advisory classifier"));
}

#[test]
fn deny_policy_makes_zero_classifier_calls() {
    // read_only's inspection ceiling denies mutation outright: a deny must
    // never trigger a classifier call, even with a classifier attached.
    let mut harness = Harness::new(Profile::ReadOnly, 10, Vec::new());
    harness.set_script(vec![
        tool_response(vec![write_intent("w1")]),
        ScriptedResponse::text("done"),
    ]);
    let classifier = scripted(vec![escalating()]);
    harness.classifier = Some(classifier.clone());
    harness.classifier_profiles = vec![Profile::ReadOnly];

    let (outcome, events) = harness.run("write notes", Vec::new());

    assert_eq!(outcome.state, TerminalState::Completed);
    assert!(!harness.exists("notes.txt"));
    assert_eq!(
        classifier.calls(),
        0,
        "deny decisions never call the classifier"
    );
    assert!(outcome.classifier.attached);
    assert!(outcome.classifier.is_empty());
    assert!(events.iter().any(|event| {
        event.event_type == EventType::ToolResult && event.data["status"] == "denied"
    }));
}

#[test]
fn read_only_effect_makes_zero_classifier_calls() {
    let mut harness = Harness::new(Profile::Balanced, 10, Vec::new());
    harness.seed("README.md", "hello\n");
    harness.set_script(vec![
        tool_response(vec![read_intent("r1", "README.md")]),
        ScriptedResponse::text("done"),
    ]);
    let classifier = scripted(vec![escalating()]);
    harness.classifier = Some(classifier.clone());
    harness.classifier_profiles = vec![Profile::Balanced];

    let (outcome, _events) = harness.run("read the readme", Vec::new());

    assert_eq!(outcome.state, TerminalState::Completed);
    assert_eq!(
        classifier.calls(),
        0,
        "read-only effects are never escalated"
    );
    assert!(outcome.classifier.attached);
    assert!(outcome.classifier.is_empty());
}

#[test]
fn unselected_profile_makes_zero_classifier_calls() {
    let mut harness = Harness::new(Profile::Unrestricted, 10, Vec::new());
    harness.set_script(vec![
        tool_response(vec![write_intent("w1")]),
        ScriptedResponse::text("done"),
    ]);
    let classifier = scripted(vec![escalating()]);
    harness.classifier = Some(classifier.clone());
    // Default profiles exclude unrestricted; an allow must proceed untouched.
    harness.classifier_profiles = vec![Profile::Balanced, Profile::WorkspaceAuto];

    let (outcome, _events) = harness.run("write notes", Vec::new());

    assert_eq!(outcome.state, TerminalState::Completed);
    assert!(harness.exists("notes.txt"));
    assert_eq!(classifier.calls(), 0, "unselected profiles never call");
    assert!(outcome.classifier.attached);
    assert!(outcome.classifier.is_empty());
}

#[test]
fn unavailable_verdict_is_fail_neutral() {
    let mut harness = Harness::new(Profile::Unrestricted, 10, Vec::new());
    harness.set_script(vec![
        tool_response(vec![write_intent("w1")]),
        ScriptedResponse::text("done"),
    ]);
    let classifier = scripted(vec![ClassifierVerdict::unavailable(
        "jev-test",
        Availability::Timeout,
    )]);
    harness.classifier = Some(classifier.clone());
    harness.classifier_profiles = vec![Profile::Unrestricted];

    let (outcome, events) = harness.run("write notes", Vec::new());

    assert_eq!(outcome.state, TerminalState::Completed);
    assert!(
        harness.exists("notes.txt"),
        "a timeout must not deny local work"
    );
    assert_eq!(classifier.calls(), 1);
    let proposed = events
        .iter()
        .find(|event| event.event_type == EventType::ToolProposed)
        .expect("tool.proposed");
    assert_eq!(proposed.data["decision"], "allow");

    // Fail-neutral still means audited: a timeout is a call with availability
    // `timeout` and unknown cost, and it never escalates.
    assert_eq!(outcome.classifier.calls, 1);
    assert_eq!(outcome.classifier.escalations, 0);
    assert_eq!(
        outcome.classifier.availability.get(&Availability::Timeout),
        Some(&1)
    );
    assert_eq!(stored_audit(&harness, &outcome), outcome.classifier);
}

#[test]
fn escalated_approval_keeps_intent_hash_and_stays_consumable_once() {
    let mut harness = Harness::new(Profile::Unrestricted, 10, Vec::new());
    harness.set_script(vec![
        tool_response(vec![write_intent("w1")]),
        ScriptedResponse::text("done"),
    ]);
    let classifier = scripted(vec![escalating()]);
    harness.classifier = Some(classifier.clone());
    harness.classifier_profiles = vec![Profile::Unrestricted];

    let (outcome, events) = harness.run("write notes", vec![true]);

    assert_eq!(outcome.state, TerminalState::Completed);
    assert!(harness.exists("notes.txt"));
    assert_eq!(classifier.calls(), 1);
    let proposed = events
        .iter()
        .find(|event| event.event_type == EventType::ToolProposed)
        .expect("tool.proposed");
    let requested = events
        .iter()
        .find(|event| event.event_type == EventType::ApprovalRequested)
        .expect("approval.requested");
    assert_eq!(
        proposed.data["intent_hash"], requested.data["intent_hash"],
        "the hint is not part of the intent hash; an approval stays valid"
    );
    assert_eq!(classifier.remaining(), 0);
    assert_eq!(outcome.classifier.escalations, 1);
    assert_eq!(stored_audit(&harness, &outcome), outcome.classifier);
}

#[test]
fn run_without_a_gate_reports_and_persists_no_classifier_activity() {
    let mut harness = Harness::new(Profile::Unrestricted, 10, Vec::new());
    harness.set_script(vec![ScriptedResponse::text("done")]);

    let (outcome, _events) = harness.run("say hi", Vec::new());

    assert_eq!(outcome.state, TerminalState::Completed);
    assert!(!outcome.classifier.attached);
    assert!(outcome.classifier.is_empty());
    assert_eq!(outcome.classifier.cost_microusd, None);
    let detail = harness.store.run(&outcome.run_id).unwrap();
    assert!(
        detail.classifier_json.is_none(),
        "no attached gate means no audit row, not an empty one"
    );
}

/// Serialize a session's durable journal exactly as `bollo sessions export`
/// does: replay, then `ndjson::write_event` per envelope.
fn exported_ndjson(harness: &Harness) -> String {
    let events = harness
        .store
        .replay(&harness.session, 0)
        .expect("the journal replays");
    let mut bytes = Vec::new();
    for event in &events {
        bollo_protocol::ndjson::write_event(&mut bytes, event).expect("envelopes are exportable");
    }
    String::from_utf8(bytes).expect("NDJSON is UTF-8")
}

/// Mask only the genuinely per-run fields (`event_id`, `session_id`, `run_id`,
/// `timestamp`) by exact value, so every other exported byte survives the
/// comparison.
fn mask_per_run_fields(harness: &Harness, raw: &str) -> String {
    let events = harness
        .store
        .replay(&harness.session, 0)
        .expect("the journal replays");
    let mut masked = raw.to_string();
    for event in &events {
        masked = masked.replace(event.event_id.as_str(), "<event-id>");
        masked = masked.replace(event.session_id.as_str(), "<session-id>");
        if let Some(run_id) = &event.run_id {
            masked = masked.replace(run_id.as_str(), "<run-id>");
        }
        masked = masked.replace(&event.timestamp, "<timestamp>");
    }
    masked
}

/// Regression guard for the event-schema freeze (BH-021): the same scenario
/// must export byte-identical NDJSON with and without an attached classifier,
/// once only per-run identity/time fields are masked. An added `classifier`
/// field, a changed payload or a new event type would break the equality.
#[test]
fn exported_ndjson_is_byte_identical_with_and_without_a_classifier() {
    fn script() -> Vec<ScriptedResponse> {
        vec![
            tool_response(vec![write_intent("w1")]),
            ScriptedResponse::text("done"),
        ]
    }

    // Baseline: no gate is attached, so zero classifier calls are possible.
    let mut baseline = Harness::new(Profile::Unrestricted, 10, Vec::new());
    baseline.set_script(script());
    let (baseline_outcome, _events) = baseline.run("write notes", Vec::new());
    assert_eq!(baseline_outcome.state, TerminalState::Completed);
    assert!(!baseline_outcome.classifier.attached, "no gate attached");
    assert!(baseline.exists("notes.txt"));

    // Attached and consulted, but the neutral verdict changes no decision, so
    // any journal difference can only come from the gate itself.
    let mut attached = Harness::new(Profile::Unrestricted, 10, Vec::new());
    attached.set_script(script());
    let classifier = scripted(vec![neutral()]);
    attached.classifier = Some(classifier.clone());
    attached.classifier_profiles = vec![Profile::Unrestricted];
    let (attached_outcome, _events) = attached.run("write notes", Vec::new());
    assert_eq!(attached_outcome.state, TerminalState::Completed);
    assert!(attached.exists("notes.txt"));
    assert_eq!(classifier.calls(), 1, "the gate is actually consulted");
    assert_eq!(attached_outcome.classifier.escalations, 0);

    let baseline_raw = exported_ndjson(&baseline);
    let attached_raw = exported_ndjson(&attached);
    assert_ne!(
        baseline_raw, attached_raw,
        "separate executions differ in per-run identity fields"
    );

    let baseline_masked = mask_per_run_fields(&baseline, &baseline_raw);
    let attached_masked = mask_per_run_fields(&attached, &attached_raw);
    assert_eq!(
        baseline_masked, attached_masked,
        "classifier attachment must not change a single exported byte"
    );

    // Non-vacuous: real payloads survived the masking, and no classifier token
    // exists anywhere in the exported stream.
    assert!(attached_masked.contains("\"type\":\"tool_proposed\""));
    assert!(attached_masked.contains("\"decision\":\"allow\""));
    assert!(
        !attached_masked.to_lowercase().contains("classifier"),
        "the event schema is frozen: classifier activity never enters NDJSON"
    );
}
