//! End-to-end HTTP tests against a real loopback daemon.
//!
//! Every test drives the documented contract through a real socket: bearer
//! auth, capability ceilings, Host/Origin checks, idempotent creations, SSE
//! replay, rate/stream limits, approvals and artifacts.

use std::collections::HashMap;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;

use bollo_api::auth::{ApiToken, Capability};
use bollo_api::backend::{BackendRun, RunBackend};
use bollo_api::state::{
    ApiConfig, ApiLimits, PolicySnapshot, ProvenanceView, RuleView, SharedState,
};
use bollo_api::{ApiError, ApiServer, RunningApi};

use bollo_policy::approval::{ApprovalReceipt, ApprovalState, ApprovalStore};
use bollo_protocol::events::{AssistantDeltaData, EventType, RunFinishedData, RunStartedData};
use bollo_protocol::ids::{ApprovalId, RunId, SessionId, ToolCallId};
use bollo_protocol::vocab::{
    Effect, Profile, RunState, SandboxMode, TerminalState, VerificationStatus,
};

const HOLD: u8 = 1;
const FAIL: u8 = 2;

struct ScriptedBackend {
    mode: AtomicU8,
}

impl ScriptedBackend {
    fn new(mode: u8) -> Arc<Self> {
        Arc::new(Self {
            mode: AtomicU8::new(mode),
        })
    }
}

impl RunBackend for ScriptedBackend {
    fn start(&self, run: BackendRun, state: &SharedState) -> Result<(), ApiError> {
        match self.mode.load(Ordering::SeqCst) {
            HOLD => Ok(()),
            FAIL => Err(ApiError::internal("scripted backend refusal")),
            _ => {
                let state = Arc::clone(state);
                std::thread::spawn(move || {
                    let started = RunStartedData {
                        state: "running".into(),
                        policy_revision: 7,
                        // Envelope validation accepts only the two real adapter names.
                        provider: "anthropic".into(),
                        model: "replay-model".into(),
                    };
                    let _ = state.append_event(
                        &run.session,
                        Some(&run.run),
                        EventType::RunStarted,
                        &serde_json::to_value(started).unwrap(),
                    );
                    let finished = RunFinishedData {
                        state: TerminalState::Completed,
                        reason: None,
                        verification: VerificationStatus::Passed,
                    };
                    let _ = state.append_event(
                        &run.session,
                        Some(&run.run),
                        EventType::RunFinished,
                        &serde_json::to_value(finished).unwrap(),
                    );
                    let mut store = state.store();
                    let _ = store.finish_run(
                        &run.run,
                        TerminalState::Completed,
                        None,
                        VerificationStatus::Passed,
                    );
                    state.clear_active(&run.session);
                });
                Ok(())
            }
        }
    }

    fn cancel(&self, _run: &RunId, _state: &SharedState) -> Result<RunState, ApiError> {
        Ok(RunState::Cancelled)
    }
}

fn token(id: &str, capabilities: &[Capability]) -> ApiToken {
    ApiToken::new(id, format!("{id}-secret"), capabilities.to_vec())
}

fn all_caps() -> Vec<Capability> {
    vec![Capability::Read, Capability::Run, Capability::Approve]
}

fn policy() -> PolicySnapshot {
    PolicySnapshot {
        revision: 7,
        profile: Profile::Balanced,
        sandbox: SandboxMode::Off,
        isolation_verified: false,
        rules: vec![RuleView {
            id: "private-env".into(),
            effect: Effect::Deny,
            tool: "*".into(),
            path_glob: Some("**/.env".into()),
            argv_prefix: None,
        }],
        provenance: vec![ProvenanceView {
            rule_id: "private-env".into(),
            source: "~/.config/bollo/config.json".into(),
            layer: "user".into(),
        }],
    }
}

fn config(state_dir: &Path, tokens: Vec<ApiToken>, backend: Arc<dyn RunBackend>) -> ApiConfig {
    ApiConfig {
        bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        workspace_id: "ws_test".into(),
        workspace_scope: state_dir.display().to_string(),
        state_directory: state_dir.join("api-state"),
        provider: "replay".into(),
        model: "replay-model".into(),
        policy: policy(),
        tokens,
        allowed_origins: Vec::new(),
        limits: ApiLimits::default(),
        backend,
    }
}

fn start_with(
    dir: &Path,
    tokens: Vec<ApiToken>,
    backend: Arc<dyn RunBackend>,
    mutate: impl FnOnce(&mut ApiConfig),
) -> RunningApi {
    let mut config = config(dir, tokens, backend);
    mutate(&mut config);
    ApiServer::start(config).expect("api starts")
}

fn start(dir: &Path, tokens: Vec<ApiToken>, backend: Arc<dyn RunBackend>) -> RunningApi {
    start_with(dir, tokens, backend, |_| {})
}

fn temp() -> TempDir {
    tempfile::tempdir().expect("tempdir")
}

#[derive(Debug)]
struct Reply {
    status: u16,
    body: String,
    headers: HashMap<String, String>,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|err| {
            panic!("body is not JSON ({err}): {:?}", self.body);
        })
    }
}

fn call(
    api: &RunningApi,
    method: &str,
    path: &str,
    secret: Option<&str>,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> Reply {
    let url = format!("{}{}", api.base_url(), path);
    let mut request = ureq::request(method, &url);
    if let Some(secret) = secret {
        request = request.set("Authorization", &format!("Bearer {secret}"));
    }
    for (name, value) in headers {
        request = request.set(name, value);
    }
    let outcome = match body {
        Some(body) => request.send_string(body),
        None => request.call(),
    };
    let response = match outcome {
        Ok(response) => response,
        Err(ureq::Error::Status(_, response)) => response,
        Err(error) => panic!("transport error: {error}"),
    };
    let status = response.status();
    let mut captured = HashMap::new();
    for name in response.headers_names() {
        if let Some(value) = response.header(&name) {
            captured.insert(name.to_lowercase(), value.to_string());
        }
    }
    let body = response.into_string().unwrap_or_default();
    Reply {
        status,
        body,
        headers: captured,
    }
}

fn open_stream(
    api: &RunningApi,
    path: &str,
    secret: &str,
    last_event_id: Option<&str>,
) -> ureq::Response {
    let mut request = ureq::get(&format!("{}{}", api.base_url(), path))
        .set("Authorization", &format!("Bearer {secret}"));
    if let Some(last) = last_event_id {
        request = request.set("Last-Event-ID", last);
    }
    request.call().expect("stream opens")
}

/// Read until the expected frame text arrives (or a bounded deadline passes),
/// because one `read` may catch only the stream preamble.
fn read_until_frame(reader: &mut dyn Read, needle: &str) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut collected = String::new();
    let mut buffer = vec![0u8; 8192];
    while std::time::Instant::now() < deadline {
        if collected.contains(needle) {
            return collected;
        }
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => collected.push_str(&String::from_utf8_lossy(&buffer[..count])),
            Err(_) => break,
        }
    }
    collected
}

fn create_session(api: &RunningApi, secret: &str, key: &str) -> String {
    let reply = call(
        api,
        "POST",
        "/sessions",
        Some(secret),
        &[("Idempotency-Key", key)],
        Some("{}"),
    );
    assert_eq!(reply.status, 201, "create session: {}", reply.body);
    reply.json()["id"].as_str().unwrap().to_string()
}

fn create_run(api: &RunningApi, secret: &str, session: &str, key: &str, prompt: &str) -> Reply {
    call(
        api,
        "POST",
        &format!("/sessions/{session}/runs"),
        Some(secret),
        &[("Idempotency-Key", key)],
        Some(&json!({ "prompt": prompt }).to_string()),
    )
}

fn rfc3339(offset: time::OffsetDateTime) -> String {
    offset
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap()
}

fn pending_receipt(expires_in_seconds: i64) -> ApprovalReceipt {
    ApprovalReceipt {
        approval_id: ApprovalId::generate(),
        session_id: SessionId::generate(),
        run_id: RunId::generate(),
        tool_call_id: ToolCallId::generate(),
        tool_name: "exec".into(),
        intent_hash: "a".repeat(64),
        workspace_identity: "ws_test".into(),
        policy_revision: 7,
        target_preimage: None,
        state: ApprovalState::Pending,
        expires_at: rfc3339(
            time::OffsetDateTime::now_utc() + time::Duration::seconds(expires_in_seconds),
        ),
        consumed_at: None,
        note: None,
        summary: "argv: [\"cargo\", \"test\"]".into(),
    }
}

#[test]
fn health_and_capabilities_require_a_bearer_token() {
    let dir = temp();
    let api = start(
        dir.path(),
        vec![token("tok_read", &[Capability::Read])],
        ScriptedBackend::new(0),
    );

    let unauthorized = call(&api, "GET", "/health", None, &[], None);
    assert_eq!(unauthorized.status, 401);
    assert_eq!(unauthorized.json()["code"], "unauthenticated");
    assert_eq!(
        unauthorized
            .headers
            .get("www-authenticate")
            .map(String::as_str),
        Some("Bearer")
    );

    let health = call(&api, "GET", "/health", Some("tok_read-secret"), &[], None);
    assert_eq!(health.status, 200);
    let body = health.json();
    assert_eq!(body["status"], "ok");
    assert_eq!(body["api_version"], "v1");

    let capabilities = call(
        &api,
        "GET",
        "/capabilities",
        Some("tok_read-secret"),
        &[],
        None,
    );
    assert_eq!(capabilities.status, 200);
    let body = capabilities.json();
    assert_eq!(body["schema_version"], "0.1");
    let features = body["features"].as_array().unwrap();
    assert!(features.contains(&json!("sessions")));
    assert!(features.contains(&json!("policy")));
    let probe = bollo_workspace::sandbox::probe();
    assert_eq!(
        body["sandbox_verified"].as_bool().unwrap(),
        probe.filesystem_containment && probe.network_denied
    );
}

#[test]
fn host_and_origin_are_validated_before_auth() {
    let dir = temp();
    let api = start_with(
        dir.path(),
        vec![token("tok_read", &[Capability::Read])],
        ScriptedBackend::new(0),
        |config| config.allowed_origins = vec!["http://127.0.0.1:5173".into()],
    );

    let foreign_host = call(
        &api,
        "GET",
        "/health",
        Some("tok_read-secret"),
        &[("Host", "evil.example")],
        None,
    );
    assert_eq!(foreign_host.status, 403);
    assert_eq!(foreign_host.json()["code"], "origin_denied");

    let denied_origin = call(
        &api,
        "GET",
        "/health",
        Some("tok_read-secret"),
        &[("Origin", "http://evil.example")],
        None,
    );
    assert_eq!(denied_origin.status, 403);
    assert_eq!(denied_origin.json()["code"], "origin_denied");

    let allowed_origin = call(
        &api,
        "GET",
        "/health",
        Some("tok_read-secret"),
        &[("Origin", "http://127.0.0.1:5173")],
        None,
    );
    assert_eq!(allowed_origin.status, 200, "{}", allowed_origin.body);
}

#[test]
fn token_capabilities_bound_every_route() {
    let dir = temp();
    let api = start(
        dir.path(),
        vec![
            token("tok_read", &[Capability::Read]),
            token("tok_run", &[Capability::Run]),
        ],
        ScriptedBackend::new(HOLD),
    );

    let denied = call(
        &api,
        "POST",
        "/sessions",
        Some("tok_read-secret"),
        &[("Idempotency-Key", "0123456789abcdef")],
        Some("{}"),
    );
    assert_eq!(denied.status, 403);
    assert_eq!(denied.json()["code"], "forbidden");
    assert_eq!(denied.json()["code"], "forbidden");

    // A run token can create a session but cannot read approvals (Approve).
    let session = create_session(&api, "tok_run-secret", "0123456789abcdef");
    let approval = call(
        &api,
        "GET",
        &format!("/approvals/apr_{}", "0".repeat(8)),
        Some("tok_run-secret"),
        &[],
        None,
    );
    assert_eq!(approval.status, 403);
    let _ = session;

    // A read token cannot start runs.
    let session = create_session(&api, "tok_run-secret", "fedcba9876543210");
    let denied_run = create_run(&api, "tok_read-secret", &session, "aaaabbbbccccdddd", "hi");
    assert_eq!(denied_run.status, 403);
}

#[test]
fn session_creation_is_idempotent_and_survives_restart() {
    let dir = temp();
    let tokens = vec![token("tok_all", &all_caps())];
    let key = "0123456789abcdef";
    let session = {
        let api = start(dir.path(), tokens.clone(), ScriptedBackend::new(0));
        let first = call(
            &api,
            "POST",
            "/sessions",
            Some("tok_all-secret"),
            &[("Idempotency-Key", key)],
            Some("{}"),
        );
        assert_eq!(first.status, 201);
        let id = first.json()["id"].as_str().unwrap().to_string();

        let replay = call(
            &api,
            "POST",
            "/sessions",
            Some("tok_all-secret"),
            &[("Idempotency-Key", key)],
            Some("{}"),
        );
        assert_eq!(replay.status, 201);
        assert_eq!(replay.json()["id"].as_str().unwrap(), id);

        let conflict = call(
            &api,
            "POST",
            "/sessions",
            Some("tok_all-secret"),
            &[("Idempotency-Key", key)],
            Some(r#"{"label":"different"}"#),
        );
        assert_eq!(conflict.status, 409);
        assert_eq!(conflict.json()["code"], "idempotency_conflict");

        let missing_key = call(
            &api,
            "POST",
            "/sessions",
            Some("tok_all-secret"),
            &[],
            Some("{}"),
        );
        assert_eq!(missing_key.status, 400);

        let short_key = call(
            &api,
            "POST",
            "/sessions",
            Some("tok_all-secret"),
            &[("Idempotency-Key", "short")],
            Some("{}"),
        );
        assert_eq!(short_key.status, 400);

        assert_eq!(api.state().store().list_sessions().unwrap().len(), 1);
        api
    };
    drop(session);

    // Restart with the same state directory: the receipt replays the original
    // resource instead of creating a second session.
    let api = start(dir.path(), tokens, ScriptedBackend::new(0));
    let replay = call(
        &api,
        "POST",
        "/sessions",
        Some("tok_all-secret"),
        &[("Idempotency-Key", key)],
        Some("{}"),
    );
    assert_eq!(replay.status, 201);
    assert!(replay.json()["id"].as_str().unwrap().starts_with("sess_"));
    assert_eq!(api.state().store().list_sessions().unwrap().len(), 1);
}

#[test]
fn unknown_routes_and_ids_are_not_found() {
    let dir = temp();
    let api = start(
        dir.path(),
        vec![token("tok_all", &all_caps())],
        ScriptedBackend::new(0),
    );

    let unknown = call(&api, "GET", "/nope", Some("tok_all-secret"), &[], None);
    assert_eq!(unknown.status, 404);

    let unknown_session = call(
        &api,
        "GET",
        "/sessions/sess_missing000000",
        Some("tok_all-secret"),
        &[],
        None,
    );
    assert_eq!(unknown_session.status, 404);
    assert_eq!(unknown_session.json()["code"], "not_found");
}

#[test]
fn events_replay_as_sse_and_cursor_rules_are_enforced() {
    let dir = temp();
    let api = start(
        dir.path(),
        vec![token("tok_all", &all_caps())],
        ScriptedBackend::new(0),
    );
    let session = create_session(&api, "tok_all-secret", "0123456789abcdef");
    let session_id = SessionId::parse(session.clone()).unwrap();

    let event_run = RunId::generate();
    for text in ["first", "second"] {
        api.state()
            .append_event(
                &session_id,
                Some(&event_run),
                EventType::AssistantDelta,
                &serde_json::to_value(AssistantDeltaData { text: text.into() }).unwrap(),
            )
            .unwrap();
    }

    let path = format!("/sessions/{session}/events?after=1");
    let response = open_stream(&api, &path, "tok_all-secret", None);
    let mut reader = response.into_reader();
    let frame = read_until_frame(&mut reader, "id: 2");
    assert!(frame.contains("id: 2"), "frame: {frame}");
    assert!(!frame.contains("id: 1"), "frame: {frame}");
    assert!(frame.contains("\"text\":\"second\""), "frame: {frame}");

    let ahead = call(
        &api,
        "GET",
        &format!("/sessions/{session}/events?after=99"),
        Some("tok_all-secret"),
        &[],
        None,
    );
    assert_eq!(ahead.status, 400);
    assert_eq!(ahead.json()["code"], "invalid_cursor");

    let disagree = call(
        &api,
        "GET",
        &format!("/sessions/{session}/events?after=1"),
        Some("tok_all-secret"),
        &[("Last-Event-ID", "2")],
        None,
    );
    assert_eq!(disagree.status, 400);
    assert_eq!(disagree.json()["code"], "invalid_cursor");

    let agree = open_stream(
        &api,
        &format!("/sessions/{session}/events?after=1"),
        "tok_all-secret",
        Some("1"),
    );
    assert_eq!(agree.status(), 200);
}

#[test]
fn live_events_reach_an_open_stream() {
    let dir = temp();
    let api = start(
        dir.path(),
        vec![token("tok_all", &all_caps())],
        ScriptedBackend::new(0),
    );
    let session = create_session(&api, "tok_all-secret", "0123456789abcdef");
    let session_id = SessionId::parse(session.clone()).unwrap();

    let response = open_stream(
        &api,
        &format!("/sessions/{session}/events"),
        "tok_all-secret",
        None,
    );
    let mut reader = response.into_reader();

    api.state()
        .append_event(
            &session_id,
            Some(&RunId::generate()),
            EventType::AssistantDelta,
            &serde_json::to_value(AssistantDeltaData {
                text: "live".into(),
            })
            .unwrap(),
        )
        .unwrap();

    let frame = read_until_frame(&mut reader, "\"text\":\"live\"");
    assert!(frame.contains("id: 1"), "frame: {frame}");
    assert!(frame.contains("\"text\":\"live\""), "frame: {frame}");
}

#[test]
fn run_lifecycle_busy_cancel_and_state() {
    let dir = temp();
    let api = start(
        dir.path(),
        vec![token("tok_all", &all_caps())],
        ScriptedBackend::new(HOLD),
    );
    let session = create_session(&api, "tok_all-secret", "0123456789abcdef");

    let first = create_run(&api, "tok_all-secret", &session, "runkey0000000001", "hold");
    assert_eq!(first.status, 202, "{}", first.body);
    let run_id = first.json()["id"].as_str().unwrap().to_string();
    assert_eq!(first.json()["state"], "running");

    let busy = create_run(
        &api,
        "tok_all-secret",
        &session,
        "runkey0000000002",
        "again",
    );
    assert_eq!(busy.status, 409);
    assert_eq!(busy.json()["code"], "session_busy");

    let fetched = call(
        &api,
        "GET",
        &format!("/runs/{run_id}"),
        Some("tok_all-secret"),
        &[],
        None,
    );
    assert_eq!(fetched.status, 200);
    assert_eq!(fetched.json()["id"], run_id);

    let cancelled = call(
        &api,
        "POST",
        &format!("/runs/{run_id}/cancel"),
        Some("tok_all-secret"),
        &[],
        None,
    );
    assert_eq!(cancelled.status, 202);
    assert_eq!(cancelled.json()["state"], "cancelled");

    // Cancelling a terminal run is idempotent.
    let repeat = call(
        &api,
        "POST",
        &format!("/runs/{run_id}/cancel"),
        Some("tok_all-secret"),
        &[],
        None,
    );
    assert_eq!(repeat.status, 202);
    assert_eq!(repeat.json()["state"], "cancelled");
}

#[test]
fn run_read_exposes_the_persisted_classifier_audit() {
    let dir = temp();
    let api = start(
        dir.path(),
        vec![token("tok_all", &all_caps())],
        ScriptedBackend::new(HOLD),
    );
    let session = api.state().store().create_session("ws_test", None).unwrap();
    let run = api
        .state()
        .store()
        .start_run(&session, "replay", "replay-model", 7)
        .unwrap();

    // A run without an attached gate reports the property as null, not absent.
    let plain = call(
        &api,
        "GET",
        &format!("/runs/{run}"),
        Some("tok_all-secret"),
        &[],
        None,
    );
    assert_eq!(plain.status, 200, "{}", plain.body);
    let body = plain.json();
    assert!(body.as_object().unwrap().contains_key("classifier"));
    assert!(body["classifier"].is_null(), "{}", plain.body);

    // Plant the audit exactly as a live classifier run persists it.
    api.state()
        .store()
        .record_classifier_audit(
            &run,
            r#"{"attached":true,"calls":2,"availability":{"available":1,"timeout":1},"escalations":1,"cost_known":false,"cost_microusd":null}"#,
        )
        .unwrap();

    let fetched = call(
        &api,
        "GET",
        &format!("/runs/{run}"),
        Some("tok_all-secret"),
        &[],
        None,
    );
    assert_eq!(fetched.status, 200, "{}", fetched.body);
    assert_eq!(
        fetched.json()["classifier"],
        json!({
            "attached": true,
            "calls": 2,
            "availability": {"available": 1, "timeout": 1},
            "escalations": 1,
            "cost_known": false,
            "cost_microusd": null
        }),
        "{}",
        fetched.body
    );
}

#[test]
fn completed_runs_finish_background_work() {
    let dir = temp();
    let api = start(
        dir.path(),
        vec![token("tok_all", &all_caps())],
        ScriptedBackend::new(0),
    );
    let session = create_session(&api, "tok_all-secret", "0123456789abcdef");
    let created = create_run(
        &api,
        "tok_all-secret",
        &session,
        "runkey0000000001",
        "finish",
    );
    assert_eq!(created.status, 202);
    let run_id = created.json()["id"].as_str().unwrap().to_string();

    // The scripted backend finishes asynchronously; poll until terminal.
    let mut state = String::new();
    for _ in 0..50 {
        let fetched = call(
            &api,
            "GET",
            &format!("/runs/{run_id}"),
            Some("tok_all-secret"),
            &[],
            None,
        );
        state = fetched.json()["state"].as_str().unwrap().to_string();
        if state == "completed" {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(state, "completed");

    // The events route streams forever, so verify the durable journal instead.
    let session_id = SessionId::parse(session).unwrap();
    let replayed = api.state().store().replay(&session_id, 0).unwrap();
    assert_eq!(replayed.len(), 2);
    assert_eq!(replayed[0].event_type, EventType::RunStarted);
    assert_eq!(replayed[1].event_type, EventType::RunFinished);
}

#[test]
fn approval_decision_is_single_use_and_binding() {
    let dir = temp();
    let api = start(
        dir.path(),
        vec![
            token("tok_all", &all_caps()),
            token("tok_read", &[Capability::Read]),
        ],
        ScriptedBackend::new(0),
    );
    let receipt = pending_receipt(300);
    let approval_id = receipt.approval_id.to_string();
    api.state().approvals().insert(receipt).unwrap();

    let view = call(
        &api,
        "GET",
        &format!("/approvals/{approval_id}"),
        Some("tok_all-secret"),
        &[],
        None,
    );
    assert_eq!(view.status, 200);
    let body = view.json();
    assert_eq!(body["status"], "pending");
    assert_eq!(body["display_complete"], true);
    assert_eq!(body["intent_hash"], "a".repeat(64));
    assert_eq!(body["policy_revision"], 7);

    let stale = call(
        &api,
        "POST",
        &format!("/approvals/{approval_id}/decision"),
        Some("tok_all-secret"),
        &[],
        Some(
            &json!({
                "decision": "approve",
                "intent_hash": "b".repeat(64),
                "policy_revision": 7
            })
            .to_string(),
        ),
    );
    assert_eq!(stale.status, 409);
    assert_eq!(stale.json()["code"], "approval_stale");

    let stale_revision = call(
        &api,
        "POST",
        &format!("/approvals/{approval_id}/decision"),
        Some("tok_all-secret"),
        &[],
        Some(
            &json!({
                "decision": "approve",
                "intent_hash": "a".repeat(64),
                "policy_revision": 8
            })
            .to_string(),
        ),
    );
    assert_eq!(stale_revision.status, 409);
    assert_eq!(stale_revision.json()["code"], "approval_stale");

    let approved = call(
        &api,
        "POST",
        &format!("/approvals/{approval_id}/decision"),
        Some("tok_all-secret"),
        &[],
        Some(
            &json!({
                "decision": "approve",
                "intent_hash": "a".repeat(64),
                "policy_revision": 7
            })
            .to_string(),
        ),
    );
    assert_eq!(approved.status, 200, "{}", approved.body);
    assert_eq!(approved.json()["decision"], "approve");

    let again = call(
        &api,
        "POST",
        &format!("/approvals/{approval_id}/decision"),
        Some("tok_all-secret"),
        &[],
        Some(
            &json!({
                "decision": "deny",
                "intent_hash": "a".repeat(64),
                "policy_revision": 7
            })
            .to_string(),
        ),
    );
    assert_eq!(again.status, 409);
    assert_eq!(again.json()["code"], "approval_consumed");
}

#[test]
fn expired_approval_is_gone_not_decidable() {
    let dir = temp();
    let api = start(
        dir.path(),
        vec![token("tok_all", &all_caps())],
        ScriptedBackend::new(0),
    );
    let receipt = pending_receipt(-10);
    let approval_id = receipt.approval_id.to_string();
    api.state().approvals().insert(receipt).unwrap();

    let view = call(
        &api,
        "GET",
        &format!("/approvals/{approval_id}"),
        Some("tok_all-secret"),
        &[],
        None,
    );
    assert_eq!(view.json()["status"], "expired");

    let decision = call(
        &api,
        "POST",
        &format!("/approvals/{approval_id}/decision"),
        Some("tok_all-secret"),
        &[],
        Some(
            &json!({
                "decision": "approve",
                "intent_hash": "a".repeat(64),
                "policy_revision": 7
            })
            .to_string(),
        ),
    );
    assert_eq!(decision.status, 410);
    assert_eq!(decision.json()["code"], "approval_expired");
}

#[test]
fn artifacts_list_and_only_text_is_served() {
    let dir = temp();
    let api = start(
        dir.path(),
        vec![token("tok_all", &all_caps())],
        ScriptedBackend::new(0),
    );
    let session = api.state().store().create_session("ws_test", None).unwrap();
    let run = api
        .state()
        .store()
        .start_run(&session, "replay", "m", 7)
        .unwrap();
    let artifact_dir = api.state().config.state_directory.join("artifacts");
    let text = api
        .state()
        .store()
        .write_run_artifact(
            &artifact_dir,
            b"hello world",
            "text/plain",
            "tool-output",
            &run,
        )
        .unwrap();
    let binary = api
        .state()
        .store()
        .write_run_artifact(
            &artifact_dir,
            b"\x00\x01\x02",
            "application/octet-stream",
            "tool-output",
            &run,
        )
        .unwrap();

    let listing = call(
        &api,
        "GET",
        &format!("/runs/{run}/artifacts"),
        Some("tok_all-secret"),
        &[],
        None,
    );
    assert_eq!(listing.status, 200);
    assert_eq!(listing.json().as_array().unwrap().len(), 2);

    let text_body = call(
        &api,
        "GET",
        &format!("/artifacts/{}", text.id),
        Some("tok_all-secret"),
        &[],
        None,
    );
    assert_eq!(text_body.status, 200);
    assert_eq!(text_body.body, "hello world");
    assert!(text_body
        .headers
        .get("content-type")
        .unwrap()
        .starts_with("text/plain"));

    let binary_body = call(
        &api,
        "GET",
        &format!("/artifacts/{}", binary.id),
        Some("tok_all-secret"),
        &[],
        None,
    );
    assert_eq!(binary_body.status, 404);
}

#[test]
fn policy_endpoint_returns_the_effective_snapshot() {
    let dir = temp();
    let api = start(
        dir.path(),
        vec![token("tok_all", &all_caps())],
        ScriptedBackend::new(0),
    );
    let session = create_session(&api, "tok_all-secret", "0123456789abcdef");
    let policy = call(
        &api,
        "GET",
        &format!("/sessions/{session}/policy"),
        Some("tok_all-secret"),
        &[],
        None,
    );
    assert_eq!(policy.status, 200);
    let body = policy.json();
    assert_eq!(body["revision"], 7);
    assert_eq!(body["profile"], "balanced");
    assert_eq!(body["sandbox"], "off");
    assert_eq!(body["rules"][0]["id"], "private-env");
    assert_eq!(body["rules"][0]["effect"], "deny");
    assert_eq!(body["provenance"][0]["layer"], "user");
}

#[test]
fn rate_limit_returns_429_with_retry_after() {
    let dir = temp();
    let api = start_with(
        dir.path(),
        vec![token("tok_read", &[Capability::Read])],
        ScriptedBackend::new(0),
        |config| config.limits.requests_per_minute = 3,
    );

    for _ in 0..3 {
        let ok = call(&api, "GET", "/health", Some("tok_read-secret"), &[], None);
        assert_eq!(ok.status, 200);
    }
    let limited = call(&api, "GET", "/health", Some("tok_read-secret"), &[], None);
    assert_eq!(limited.status, 429);
    assert_eq!(limited.json()["code"], "rate_limited");
    assert!(limited.headers.contains_key("retry-after"));
}

#[test]
fn concurrent_event_streams_are_bounded() {
    let dir = temp();
    let api = start_with(
        dir.path(),
        vec![token("tok_all", &all_caps())],
        ScriptedBackend::new(0),
        |config| config.limits.max_concurrent_streams = 1,
    );
    let session = create_session(&api, "tok_all-secret", "0123456789abcdef");

    let first = open_stream(
        &api,
        &format!("/sessions/{session}/events"),
        "tok_all-secret",
        None,
    );
    assert_eq!(first.status(), 200);

    let second = ureq::get(&format!("{}/sessions/{}/events", api.base_url(), session))
        .set("Authorization", "Bearer tok_all-secret")
        .call();
    match second {
        Ok(_) => panic!("second stream should hit the concurrent-stream ceiling"),
        Err(ureq::Error::Status(code, _)) => assert_eq!(code, 429),
        Err(error) => panic!("transport error: {error}"),
    }
    drop(first);
}

#[test]
fn oversized_bodies_are_rejected_before_parsing() {
    let dir = temp();
    let api = start_with(
        dir.path(),
        vec![token("tok_all", &all_caps())],
        ScriptedBackend::new(0),
        |config| config.limits.max_body_bytes = 64,
    );
    let oversized = "x".repeat(256);
    let reply = call(
        &api,
        "POST",
        "/sessions",
        Some("tok_all-secret"),
        &[("Idempotency-Key", "0123456789abcdef")],
        Some(&json!({ "label": oversized }).to_string()),
    );
    assert_eq!(reply.status, 400);
    assert_eq!(reply.json()["code"], "invalid_request");
}

#[test]
fn failed_backend_start_leaves_no_active_run() {
    let dir = temp();
    let api = start(
        dir.path(),
        vec![token("tok_all", &all_caps())],
        ScriptedBackend::new(FAIL),
    );
    let session = create_session(&api, "tok_all-secret", "0123456789abcdef");
    let failed = create_run(&api, "tok_all-secret", &session, "runkey0000000001", "boom");
    assert_eq!(failed.status, 500);
    assert_eq!(failed.json()["code"], "internal_error");

    // The failed attempt did not leave an active or unfinished run behind.
    let session_id = SessionId::parse(session).unwrap();
    assert!(api.state().active_run(&session_id).is_none());
    assert!(api
        .state()
        .store()
        .interrupted_runs(&session_id)
        .unwrap()
        .is_empty());
}
