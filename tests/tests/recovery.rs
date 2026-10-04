//! Crash recovery. Journal intent *before* effect means a crash between spawn
//! and result leaves an operation `started`; on restart it becomes `unknown`
//! and is never automatically replayed. These tests kill at the journal
//! boundary by dropping the store handle and reopening the same database file.

use bollo_core::{NoChannel, Runtime};
use bollo_extensions::TrustStore;
use bollo_policy::MemoryApprovalStore;
use bollo_protocol::events::EventEnvelope;
use bollo_protocol::vocab::{ModeKind, TerminalState, VerificationStatus};
use bollo_providers::fake::{FakeProvider, ScriptedResponse};
use bollo_protocol::cancel::CancellationToken;
use bollo_store::{OperationState, SqliteStore};
use bollo_tests::snapshot;
use bollo_workspace::{CheckpointLog, WorkspaceFs, WorkspaceRoot};

#[test]
fn stale_operation_becomes_unknown_and_is_never_replayed() {
    let dir = tempfile::tempdir().unwrap();
    let root = WorkspaceRoot::discover(dir.path()).unwrap();
    let fs = WorkspaceFs::new(&root);
    let database = dir.path().join("state.sqlite3");

    // --- process 1: journal the intent, then die before the result ---
    let session = {
        let mut store = SqliteStore::open(&database).unwrap();
        let session = store.create_session(root.identity_hash(), Some("crash")).unwrap();
        let run = store.start_run(&session, "anthropic", "test-model", 1).unwrap();
        store
            .record_operation_intent(&run, "call_crashed", "exec", &"a".repeat(64))
            .unwrap();
        session
    };

    // --- process 2: restart, recover, then continue the session ---
    let mut store = SqliteStore::open(&database).unwrap();
    assert_eq!(store.mark_stale_operations_unknown().unwrap(), 1);
    let operations = store.operations_for_session(&session).unwrap();
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].state, OperationState::Unknown);
    assert_eq!(operations[0].tool_call_id, "call_crashed");
    assert_eq!(
        store.interrupted_runs(&session).unwrap().len(),
        1,
        "the crashed run is still nonterminal until it is finished"
    );

    let provider = FakeProvider::anthropic(vec![ScriptedResponse::text("resumed without replay")]);
    let policy = snapshot(bollo_protocol::vocab::Profile::Unrestricted, 5);
    let mut approvals = MemoryApprovalStore::new();
    let mut checkpoints = CheckpointLog::new();
    let trust = TrustStore::new();
    let cancel = CancellationToken::new();
    let mut events: Vec<EventEnvelope> = Vec::new();
    let outcome = {
        let mut channel = NoChannel;
        let mut sink = |event: &EventEnvelope| events.push(event.clone());
        let mut runtime = Runtime::new(
            &provider,
            &policy,
            root.identity_hash().to_string(),
            &fs,
            &mut store,
            &mut approvals,
            &mut checkpoints,
            &trust,
            &mut channel,
            session.clone(),
            "test-model".into(),
            ModeKind::Build,
        );
        runtime = runtime.with_cancel(cancel);
        runtime.run("continue the session", &mut sink)
    };

    assert_eq!(outcome.state, TerminalState::Completed);
    assert_eq!(outcome.verification, VerificationStatus::Skipped);

    // The unknown operation is untouched: same count, same state, no result.
    let after = store.operations_for_session(&session).unwrap();
    assert_eq!(after.len(), 1, "resuming must not re-issue the crashed effect");
    assert_eq!(after[0].state, OperationState::Unknown);
    assert!(after[0].result_ref.is_none());

    // The new run is observed as finished, and the session replays in order.
    let replayed = store.replay(&session, 0).unwrap();
    assert!(replayed
        .iter()
        .any(|event| event.event_type == bollo_protocol::EventType::RunFinished
            && event.data["state"] == "completed"));
    let seqs: Vec<u64> = replayed.iter().map(|event| event.seq).collect();
    let mut sorted = seqs.clone();
    sorted.sort_unstable();
    assert_eq!(seqs, sorted, "per-session seq strictly increases");
    assert_eq!(seqs.len(), seqs.iter().collect::<std::collections::BTreeSet<_>>().len());
}

#[test]
fn unknown_operations_survive_later_runs_in_the_same_store() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("state.sqlite3");
    let mut store = SqliteStore::open(&database).unwrap();
    let session = store.create_session("ws-identity", None).unwrap();
    let run = store.start_run(&session, "anthropic", "test-model", 1).unwrap();
    store
        .record_operation_intent(&run, "call_1", "apply_patch", &"b".repeat(64))
        .unwrap();
    assert_eq!(store.mark_stale_operations_unknown().unwrap(), 1);

    // A second run records its own operations; the unknown one is untouched and
    // there is no code path that re-executes it.
    let second = store.start_run(&session, "anthropic", "test-model", 1).unwrap();
    store
        .record_operation_intent(&second, "call_2", "read_file", &"c".repeat(64))
        .unwrap();
    store
        .record_operation_result(&second, "call_2", OperationState::Succeeded, None)
        .unwrap();

    let operations = store.operations_for_session(&session).unwrap();
    let unknown: Vec<_> = operations
        .iter()
        .filter(|operation| operation.state == OperationState::Unknown)
        .collect();
    assert_eq!(unknown.len(), 1);
    assert_eq!(unknown[0].tool_call_id, "call_1");
    assert_eq!(store.mark_stale_operations_unknown().unwrap(), 0);
}
