//! The P2 run bridge end-to-end: through a real `bollo serve` daemon, an
//! API-created run executes the CLI runtime on a worker thread, an approval
//! request surfaces on the SSE stream and in the approvals view, and a
//! decision through `POST /v1/approvals/{id}/decision` unblocks the run,
//! which then finishes durably with its journal intact.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;

const BINARY: &str = env!("CARGO_BIN_EXE_bollo");

struct Fixture {
    _root: tempfile::TempDir,
    workspace: PathBuf,
    state: PathBuf,
    config: PathBuf,
    script: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("ws");
        let state = root.path().join("state");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        let config = root.path().join("config.json");
        std::fs::write(
            &config,
            r#"{
                "schema_version": "0.1",
                "provider": {
                    "kind": "anthropic",
                    "base_url": "https://api.anthropic.com",
                    "model": "test-model",
                    "credential_env": "ANTHROPIC_API_KEY",
                    "context_tokens": 20000,
                    "input_microusd_per_token": 3,
                    "output_microusd_per_token": 15
                },
                "permissions": { "profile": "balanced", "sandbox": "off", "rules": [ { "id": "approve-writes", "effect": "ask", "tool": "write_file" } ] },
                "limits": {
                    "max_tool_calls": 10,
                    "max_run_seconds": 120,
                    "max_output_tokens": 1024,
                    "max_spend_cents": 500,
                    "tool_output_bytes": 65536
                },
                "privacy": { "telemetry": false, "content_retention_days": 7 },
                "mcp_servers": [],
                "hooks": []
            }"#,
        )
        .unwrap();
        let script = root.path().join("script.json");
        Self {
            _root: root,
            workspace,
            state,
            config,
            script,
        }
    }

    fn write_script(&self, json: &str) -> &Self {
        std::fs::write(&self.script, json).unwrap();
        self
    }
}

struct Daemon {
    child: Child,
    base: String,
    token: String,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Daemon {
    /// Start `bollo serve` and capture the two startup lines on stderr
    /// (base URL + single-use token). Raises if they do not arrive in time.
    fn spawn(fixture: &Fixture) -> Self {
        let mut child = Command::new(BINARY)
            .args([
                "--workspace",
                fixture.workspace.to_str().unwrap(),
                "--state-dir",
                fixture.state.to_str().unwrap(),
                "--provider",
                "replay",
                "--script",
                fixture.script.to_str().unwrap(),
                // The worker runs with sandbox off; the operator's daemon owns
                // the acknowledged host risk, exactly as the headless path does.
                "--acknowledge-risk",
                "serve",
                "--bind",
                "127.0.0.1:0",
            ])
            .env("BOLLO_CONFIG", &fixture.config)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn bollo serve");

        let stderr = child.stderr.take().expect("stderr piped");
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            let mut base = None;
            let mut token = None;
            for line in reader.lines() {
                let Ok(text) = line else { break };
                if text.starts_with("bollo: local API at ") {
                    base = Some(text.trim_start_matches("bollo: local API at ").trim().to_string());
                }
                if let Some(stripped) = text.trim().strip_prefix("bollo: BOLLO_TOKEN=") {
                    token = Some(stripped.split_whitespace().next().unwrap_or("").to_string());
                }
                if base.is_some() && token.is_some() {
                    let _ = sender.send((base.clone().unwrap(), token.clone().unwrap()));
                    break;
                }
            }
            let _ = sender.send(("".into(), "".into()));
        });
        let (base, token) = receiver
            .recv_timeout(Duration::from_secs(60))
            .expect("daemon startup lines");
        assert!(!base.is_empty(), "daemon printed no base URL");
        assert!(!token.is_empty(), "daemon printed no token");
        Self { child, base, token }
    }

    fn auth(&self) -> String {
        format!("Authorization: Bearer {}", self.token)
    }
}

/// `http://127.0.0.1:8420/v1` → `127.0.0.1:8420`
fn socket_of(base: &str) -> String {
    let authority = base
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or(base);
    authority.to_string()
}

/// One plain HTTP/1.1 request; the response is a fixed-length body on this
/// daemon. Returns (status, body).
fn request(addr: &str, head: &[&str], body: Option<&str>) -> (u16, String) {
    let socket = socket_of(addr);
    let mut stream = TcpStream::connect(&socket).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    write!(
        stream,
        "{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
        head[0],
        head[1],
        socket
    )
    .unwrap();
    for header in &head[2..] {
        write!(stream, "{header}\r\n").unwrap();
    }
    match body {
        None => {
            stream.write_all(b"\r\n").unwrap();
        }
        Some(body) => {
            write!(
                stream,
                "Content-Length: {}\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        }
    }
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read response");
    let separator: &[u8] = b"\r\n\r\n";
    let split = raw
        .windows(separator.len())
        .position(|chunk| chunk == separator)
        .expect("response separator");
    let head_text = String::from_utf8_lossy(&raw[..split]).to_string();
    let status = head_text
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse::<u16>()
        .unwrap();
    (status, String::from_utf8_lossy(&raw[split + 4..]).to_string())
}

/// Read the session event stream until `stop_at` appears in the SSE bytes
/// (or `done at the deadline`). Meant for short replay/live windows.
fn read_events_until(addr: &str, auth: &str, path: &str, stop_at: &str, deadline: Instant) -> String {
    let mut stream = TcpStream::connect(socket_of(addr)).expect("connect events");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {}\r\n{auth}\r\nAccept: text/event-stream\r\nConnection: close\r\n\r\n",
        socket_of(addr)
    )
    .unwrap();
    let mut collected = Vec::new();
    while Instant::now() < deadline {
        let mut chunk = [0u8; 4096];
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => collected.extend_from_slice(&chunk[..count]),
            Err(err)
                if err.kind()
                    == std::io::ErrorKind::WouldBlock
                    || err.kind() == std::io::ErrorKind::TimedOut =>
            {
                break
            }
            Err(_) => break,
        }
        let text = String::from_utf8_lossy(&collected);
        if text.contains(stop_at) {
            break;
        }
    }
    String::from_utf8_lossy(&collected).to_string()
}

/// Pull a string field out of an SSE `data:` line's JSON payload.
fn sse_field(frames: &str, line_anchor: &str, field: &str) -> Option<String> {
    for line in frames.lines() {
        if let Some(rest) = line.strip_prefix("data: ") {
            let value: Value = serde_json::from_str(rest).ok()?;
            let data = value.get("data")?;
            if let Some(found) = data.get(field).and_then(Value::as_str) {
                if line_anchor.is_empty() {
                    return Some(found.to_string());
                }
            }
        }
    }
    None
}

fn sse_numeric(frames: &str, field: &str) -> Option<u64> {
    for line in frames.lines() {
        if let Some(rest) = line.strip_prefix("data: ") {
            let value: Value = serde_json::from_str(rest).ok()?;
            if let Some(found) = value.get("data").and_then(|d| d.get(field)).and_then(Value::as_u64) {
                return Some(found);
            }
        }
    }
    None
}

fn wait_run_terminal(base: &str, daemon: &Daemon, run: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        assert!(
            Instant::now() < deadline,
            "run {run} never reached a terminal state"
        );
        let (status, body) = request(
            base,
            &[
                "GET",
                &format!("/v1/runs/{run}"),
                &daemon.auth(),
            ],
            None,
        );
        assert_eq!(status, 200, "run read: {body}");
        let value: Value = serde_json::from_str(&body).unwrap();
        let state = value.get("state").and_then(Value::as_str).unwrap_or("");
        if state == "completed" || state == "failed" || state == "cancelled" {
            return value;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn api_run_executes_the_cli_runtime_and_approves_through_the_api() {
    let fixture = Fixture::new();
    // Turn 1 proposes a balanced asks about it; turn 2 ends the turn.
    fixture.write_script(
        r#"[
            {
                "deltas": ["writing the note\n"],
                "tool_calls": [
                    {
                        "call_id": "call_notes",
                        "name": "write_file",
                        "arguments": {"path": "approval-note.txt", "content": "approved by the api\n", "expected_sha256": null}
                    }
                ]
            },
            { "deltas": ["done\n"], "finish": "end_turn" }
        ]"#,
    );

    let daemon = Daemon::spawn(&fixture);
    let base = daemon.base.clone();

    // 1) The bearer gate stays up even at the start of the new lane.
    let (status, _) = request(&base, &["GET", "/v1/health"], None);
    assert_eq!(status, 401, "unauthenticated health must be refused");

    // 2) Create a session and start a run (202: the resource exists and a
    //    worker now drives the CLI runtime).
    let (status, body) = request(
        &base,
        &[
            "POST",
            "/v1/sessions",
            &daemon.auth(),
            "Idempotency-Key: bridge-test-key-0001",
            "Content-Type: application/json",
        ],
        Some(r#"{"label":"bridge"}"#),
    );
    assert_eq!(status, 201, "session create: {body}");
    let session: Value = serde_json::from_str(&body).unwrap();
    let session_id = session
        .get("id")
        .and_then(Value::as_str)
        .unwrap()
        .to_string();

    let (status, body) = request(
        &base,
        &[
            "POST",
            &format!("/v1/sessions/{session_id}/runs"),
            &daemon.auth(),
            "Idempotency-Key: bridge-run-key-0001",
            "Content-Type: application/json",
        ],
        Some(r#"{"prompt":"write a small note through the approval gate"}"#),
    );
    assert_eq!(status, 202, "run start: {body}");
    let run: Value = serde_json::from_str(&body).unwrap();
    let run_id = run.get("id").and_then(Value::as_str).unwrap().to_string();

    // Diagnostic: prove the journal actually received events before the
    // stream read (otherwise the failure is upstream of the SSE view).
    let mut last_seq_seen = 0u64;
    for _ in 0..40 {
        let (status, body) = request(
            &base,
            &["GET", &format!("/v1/sessions/{session_id}"), &daemon.auth()],
            None,
        );
        if status == 200 {
            let session_read: Value = serde_json::from_str(&body).unwrap();
            last_seq_seen = session_read
                .get("last_seq")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            if last_seq_seen > 0 {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    println!("last_seq seen after run start: {last_seq_seen}");

    // 3) Watch the event stream for the approval request; the runtime is
    //    parked inside the worker thread until we decide.
    let frames = read_events_until(
        &base,
        &daemon.auth(),
        &format!("/v1/sessions/{session_id}/events"),
        "\"approval_id\"",
        Instant::now() + Duration::from_secs(45),
    );
    let approval_id = sse_field(&frames, "", "approval_id")
        .or_else(|| {
            // Fall back to scanning the whole stream text for the id shape.
            let index = frames.find("\"approval_id\":\"").map(|at| &frames[at + 15..])?;
            let end = index.find('"')?;
            Some(index[..end].to_string())
        })
        .unwrap_or_else(|| panic!("no approval surfaced on the event stream: {frames}"));
    let approval_id = approval_id.trim_end_matches('"').to_string();
    assert!(approval_id.starts_with("apr_"), "approval id shape: {approval_id}");
    let intent_hash = sse_field(&frames, "", "intent_hash").expect("intent hash on the approval event");
    let policy_revision = sse_numeric(&frames, "policy_revision").expect("policy revision on the approval event");

    // 4) The external view shows a complete, untruncated ask.
    let (status, body) = request(
        &base,
        &[
            "GET",
            &format!("/v1/approvals/{approval_id}"),
            &daemon.auth(),
        ],
        None,
    );
    assert_eq!(status, 200, "approval view: {body}");
    assert!(body.contains("write_file"), "approval view must show the tool: {body}");
    assert!(body.contains("approval-note.txt"), "approval view must show the target: {body}");

    // 5) Decide through the API; the run wakes and finishes durably.
    let decision = format!(
        r#"{{"decision":"approve","intent_hash":"{}","policy_revision":{}}}"#,
        intent_hash, policy_revision
    );
    let (status, body) = request(
        &base,
        &[
            "POST",
            &format!("/v1/approvals/{approval_id}/decision"),
            &daemon.auth(),
            "Content-Type: application/json",
        ],
        Some(&decision),
    );
    assert_eq!(status, 200, "decision: {body}");

    let detail = wait_run_terminal(&base, &daemon, &run_id);
    assert_eq!(
        detail.get("state").and_then(Value::as_str),
        Some("completed"),
        "approved run must complete: {detail}"
    );

    // 6) The approved effect actually happened in the workspace.
    let note = Path::new(&fixture.workspace).join("approval-note.txt");
    let written = std::fs::read_to_string(&note).expect("approved write_file must land");
    assert!(written.contains("approved by the api"), "workspace content: {written}");

    // 7) An unknown/foreign session id reads as 404, never leaked data.
    let (status, _) = request(
        &base,
        &[
            "GET",
            "/v1/sessions/sess_foreignabcdefghijklmnopqrstuvw",
            &daemon.auth(),
        ],
        None,
    );
    assert_eq!(status, 404, "foreign session must 404");
}

#[test]
fn api_run_denied_by_decision_completes_without_the_effect() {
    // Same setup; this time the answer is deny. The worker must wake on the
    // decision route, complete without the proposed effect, and never write.
    let fixture = Fixture::new();
    fixture.write_script(
        r#"[
            {
                "deltas": ["writing\n"],
                "tool_calls": [
                    {
                        "call_id": "call_notes",
                        "name": "write_file",
                        "arguments": {"path": "should-not-exist.txt", "content": "denied\n", "expected_sha256": null}
                    }
                ]
            },
            { "deltas": ["done\n"], "finish": "end_turn" }
        ]"#,
    );
    let daemon = Daemon::spawn(&fixture);
    let base = daemon.base.clone();

    let (status, body) = request(
        &base,
        &[
            "POST",
            "/v1/sessions",
            &daemon.auth(),
            "Idempotency-Key: deny-test-key-0001",
            "Content-Type: application/json",
        ],
        Some(r#"{"label":"deny"}"#),
    );
    assert_eq!(status, 201, "{body}");
    let session: Value = serde_json::from_str(&body).unwrap();
    let session_id = session.get("id").and_then(Value::as_str).unwrap().to_string();

    let (status, body) = request(
        &base,
        &[
            "POST",
            &format!("/v1/sessions/{session_id}/runs"),
            &daemon.auth(),
            "Idempotency-Key: deny-run-key-0001",
            "Content-Type: application/json",
        ],
        Some(r#"{"prompt":"permission-hungry prompt"}"#),
    );
    assert_eq!(status, 202, "{body}");
    let run: Value = serde_json::from_str(&body).unwrap();
    let run_id = run.get("id").and_then(Value::as_str).unwrap().to_string();

    // The approval surfaces on the stream; we deny it through the documented
    // route and the run completes without the proposed effect.
    let frames = read_events_until(
        &base,
        &daemon.auth(),
        &format!("/v1/sessions/{session_id}/events"),
        "approval_requested",
        Instant::now() + Duration::from_secs(45),
    );
    let approval_id = frames
        .find("\"approval_id\":\"")
        .map(|at| &frames[at + 15..])
        .and_then(|rest| Some(rest.split('"').next().unwrap_or("").to_string()))
        .unwrap_or_else(|| panic!("no approval surfaced on the event stream: {frames}"));
    let intent_hash = sse_field(&frames, "", "intent_hash").expect("intent hash on the approval event");
    let policy_revision = sse_numeric(&frames, "policy_revision").expect("policy revision event");

    let decision = format!(
        r#"{{"decision":"deny","intent_hash":"{}","policy_revision":{}}}"#,
        intent_hash, policy_revision
    );
    let (status, body) = request(
        &base,
        &[
            "POST",
            &format!("/v1/approvals/{approval_id}/decision"),
            &daemon.auth(),
            "Content-Type: application/json",
        ],
        Some(&decision),
    );
    assert_eq!(status, 200, "deny decision: {body}");

    let detail = wait_run_terminal(&base, &daemon, &run_id);
    assert_eq!(
        detail.get("state").and_then(Value::as_str),
        Some("completed"),
        "a denied run completes alive: {detail}"
    );
    assert!(
        !fixture.workspace.join("should-not-exist.txt").exists(),
        "a denied effect must never run"
    );
    // The stream shows the decision as approval-resolved with a deny.
    let resolved = read_events_until(
        &base,
        &daemon.auth(),
        &format!("/v1/sessions/{session_id}/events?after=2"),
        "approval_resolved",
        Instant::now() + Duration::from_secs(30),
    );
    assert!(
        resolved.contains("approval_resolved") && resolved.contains("deny"),
        "the resolved event must show the deny decision: {resolved}"
    );
}

/// The genuine expiry path: nobody answers, the run waits out the default
/// 300-second approval window, then the bounded wait answers deny. That takes
/// the full TTL, so it is opt-in (`cargo test -- --ignored`) rather than
/// routine suite noise.
#[test]
#[ignore = "waits out the full 300 s default approval TTL"]
fn api_run_without_a_decision_denies_at_expiry() {
    let fixture = Fixture::new();
    fixture.write_script(
        r#"[
            {
                "deltas": ["writing\n"],
                "tool_calls": [
                    {
                        "call_id": "call_notes",
                        "name": "write_file",
                        "arguments": {"path": "should-not-exist.txt", "content": "denied by expiry\n", "expected_sha256": null}
                    }
                ]
            },
            { "deltas": ["done\n"], "finish": "end_turn" }
        ]"#,
    );
    let daemon = Daemon::spawn(&fixture);
    let base = daemon.base.clone();

    let (status, body) = request(
        &base,
        &[
            "POST",
            "/v1/sessions",
            &daemon.auth(),
            "Idempotency-Key: expiry-test-key-0001",
            "Content-Type: application/json",
        ],
        Some(r#"{"label":"expiry"}"#),
    );
    assert_eq!(status, 201, "{body}");
    let session: Value = serde_json::from_str(&body).unwrap();
    let session_id = session.get("id").and_then(Value::as_str).unwrap().to_string();

    let (status, body) = request(
        &base,
        &[
            "POST",
            &format!("/v1/sessions/{session_id}/runs"),
            &daemon.auth(),
            "Idempotency-Key: expiry-run-key-0001",
            "Content-Type: application/json",
        ],
        Some(r#"{"prompt":"permission-hungry prompt"}"#),
    );
    assert_eq!(status, 202, "{body}");
    let run: Value = serde_json::from_str(&body).unwrap();
    let run_id = run.get("id").and_then(Value::as_str).unwrap().to_string();

    // Poll well past the TTL at one request every two seconds, safely inside
    // the 60/minute control limit. The resolve must be deny, never a default
    // yes, and the proposed file stays absent.
    let deadline = Instant::now() + Duration::from_secs(400);
    loop {
        assert!(
            Instant::now() < deadline,
            "run {run_id} never reached a terminal state after the approval TTL"
        );
        let (status, body) = request(&base, &["GET", &format!("/v1/runs/{run_id}"), &daemon.auth()], None);
        if status == 200 {
            let value: Value = serde_json::from_str(&body).unwrap();
            let state = value.get("state").and_then(Value::as_str).unwrap_or("");
            if state == "completed" || state == "failed" || state == "cancelled" {
                break;
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    assert!(
        !fixture.workspace.join("should-not-exist.txt").exists(),
        "an expired approval must never allow the effect"
    );
}
