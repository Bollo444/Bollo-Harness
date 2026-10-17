//! BH-016 privacy and credential isolation sweep. A canary secret is planted
//! exactly where a provider credential legitimately lives (the coordinator's
//! environment, named via `credential_env`) and every surface checked below is
//! verified clean. The journal's own contract carries metadata instead of
//! bodies — `tool.proposed` holds an `intent_hash` and `tool.result` a
//! summary (docs/reference/events.md) — and these tests hold that line.
//! Environment variables are only mutated by the single test that needs them,
//! so the suite is safe under parallel test threads.

use bollo_providers::fake::ScriptedResponse;
use bollo_protocol::ids::RunId;
use bollo_protocol::vocab::{Profile, TerminalState};
use bollo_tests::Harness;
use bollo_workspace::process::base_environment;

/// A value a scanner can grep for across every surface checked below.
const CANARY: &str = "sk-canary-BH016-never-where-it-must-not-be";

fn assert_clean(surface: &str, text: &str) {
    assert!(
        !text.contains(CANARY),
        "canary leaked into {surface}:\n{text}"
    );
}

fn first_run_id(events: &[bollo_protocol::EventEnvelope]) -> RunId {
    events
        .iter()
        .find_map(|event| event.run_id.clone())
        .expect("the run's events carry a run identity")
}

#[test]
fn credential_canary_never_reaches_a_child_env_journal_or_export() {
    std::env::set_var("ANTHROPIC_API_KEY", CANARY);
    // Sanity: the canary really is where a credential would live.
    assert_eq!(std::env::var("ANTHROPIC_API_KEY").unwrap(), CANARY);

    // The allowlisted child baseline never contains credential names.
    let allowed = base_environment();
    assert!(!allowed.contains_key("ANTHROPIC_API_KEY"));
    assert!(!allowed.values().any(|value| value == CANARY));

    // A real child confirms its own environment is clean.
    #[cfg(windows)]
    let mut command = {
        let mut command = std::process::Command::new("cmd");
        command.args(["/C", "set"]);
        command
    };
    #[cfg(unix)]
    let mut command = {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "env"]);
        command
    };
    command.env_clear();
    for (key, value) in allowed.iter() {
        command.env(key, value);
    }
    let output = command.output().unwrap();
    assert!(output.status.success());
    assert_clean("child environment", &String::from_utf8_lossy(&output.stdout));
    assert_clean("child stderr", &String::from_utf8_lossy(&output.stderr));

    // A full offline run while the canary sits in the coordinator env: the
    // streamed events, the durable journal, the run summaries and the NDJSON
    // a user would export must all be clean.
    let mut harness = Harness::new(Profile::Balanced, 10, vec![ScriptedResponse::text("plain answer.")]);
    harness.seed("notes.txt", "hello\n");
    let (outcome, events) = harness.run("read notes.txt", vec![]);
    assert_eq!(outcome.state, TerminalState::Completed);

    let mut journal = String::new();
    for event in events.iter().chain(harness.store.replay(&harness.session, 0).unwrap().iter()) {
        journal.push_str(&serde_json::to_string(event).unwrap());
        journal.push('\n');
    }
    for summary in harness.store.list_run_summaries(None).unwrap() {
        // `RunSummary` is a display/debug type, not a wire type; debug-string
        // it the way any surface would print it.
        journal.push_str(&format!("{summary:?}\n"));
        journal.push('\n');
    }
    assert_clean("journal, summaries and export surface", &journal);

    std::env::remove_var("ANTHROPIC_API_KEY");
}

#[test]
fn proposal_and_result_surfaces_carry_metadata_not_bodies() {
    // The body is deliberately unique so no other line can coincidentally
    // contain it; tool results record a summary, arguments are recorded as an
    // intent hash.
    const BODY: &str = "BH016-body-must-never-appear-in-journal";
    let mut harness = Harness::new(
        Profile::Balanced,
        10,
        vec![
            bollo_tests::tool_response(vec![bollo_tests::tool_intent(
                "c1",
                "read_file",
                serde_json::json!({"path": "secrets.txt"}),
            )]),
            ScriptedResponse::text("done reading."),
        ],
    );
    harness.seed("secrets.txt", &format!("token = {BODY}\n"));
    let (outcome, events) = harness.run("read secrets.txt", vec![true]);
    assert_eq!(outcome.state, TerminalState::Completed);

    for event in &events {
        let json = serde_json::to_value(event).unwrap();
        assert!(
            !json.to_string().contains(BODY),
            "file body leaked into the journal: {json}"
        );
        if event.event_type == bollo_protocol::EventType::ToolProposed {
            let data = json.get("data").and_then(|data| data.as_object()).unwrap();
            assert!(
                !data.contains_key("arguments"),
                "tool.proposed recorded argument bodies: {json}"
            );
        }
    }
}

#[test]
fn deletion_cascades_artifact_rows_and_retention_prunes_orphan_bodies() {
    let mut harness = Harness::new(Profile::Balanced, 10, vec![ScriptedResponse::text("transient.")]);
    harness.seed("keep-me.txt", "user content\n");
    let (outcome, events) = harness.run("one throwaway run", vec![]);
    assert_eq!(outcome.state, TerminalState::Completed);
    let run_id = first_run_id(&events);

    let artifacts = tempfile::tempdir().unwrap();
    harness
        .store
        .write_run_artifact(artifacts.path(), b"tool stdout", "text/plain", "tool-output", &run_id)
        .unwrap();
    harness
        .store
        .write_run_artifact(artifacts.path(), b"evidence", "text/plain", "evidence", &run_id)
        .unwrap();

    // Deletion removes the session, its runs and cascades the artifact rows;
    // the seeded source file is never touched.
    let deleted = harness.store.delete_session(&harness.session).unwrap();
    assert!(deleted > 0);
    assert!(harness.store.session(&harness.session).is_err());
    assert!(harness.store.list_sessions().unwrap().is_empty());
    assert!(harness.store.artifacts_for_run(&run_id).unwrap().is_empty());
    assert!(harness.exists("keep-me.txt"));
    assert_eq!(harness.read("keep-me.txt"), "user content\n");

    // After the rows are gone the bodies are orphans; one retention call
    // removes every content-addressed body that nothing references.
    assert_eq!(
        harness.store.prune_expired_artifacts(artifacts.path(), 0).unwrap(),
        0
    );
    let bodies: Vec<String> = std::fs::read_dir(artifacts.path())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().into_string().unwrap())
        .filter(|name| name.starts_with("sha256-"))
        .collect();
    assert!(bodies.is_empty(), "orphan bodies survived: {bodies:?}");
}
