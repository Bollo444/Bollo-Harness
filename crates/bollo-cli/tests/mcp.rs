//! End-to-end MCP wiring: the real `bollo` binary starts the fake stdio server
//! from trusted user configuration, advertises its tools to the model, and
//! routes calls through the same normalize → policy → approval → journal gate
//! as built-ins.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use bollo_cli::composition::McpRegistry;
use bollo_extensions::TrustStore;
use bollo_policy::config::McpServerConfig;
use bollo_protocol::errors::ErrorCode;
use bollo_protocol::events::EventEnvelope;
use bollo_protocol::vocab::ToolStatus;
use bollo_protocol::EventType;
use bollo_store::{OperationState, SqliteStore};
use bollo_tools::McpDispatch;
use serde_json::json;

const BINARY: &str = env!("CARGO_BIN_EXE_bollo");
const FIXTURE: &str = env!("CARGO_BIN_EXE_bollo-mcp-fixture");

struct Fixture {
    _root: tempfile::TempDir,
    workspace: PathBuf,
    state: PathBuf,
    config: PathBuf,
    script: PathBuf,
}

impl Fixture {
    fn new(profile: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("ws");
        let state = root.path().join("state");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        let config = root.path().join("config.json");
        let script = root.path().join("script.json");
        std::fs::write(&script, "[]").unwrap();
        let fixture = Self {
            _root: root,
            workspace,
            state,
            config,
            script,
        };
        fixture.write_config(profile);
        fixture
    }

    fn write_config(&self, profile: &str) {
        let config = json!({
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
            "permissions": { "profile": profile, "sandbox": "off", "rules": [] },
            "limits": {
                "max_tool_calls": 10,
                "max_run_seconds": 120,
                "max_output_tokens": 1024,
                "max_spend_cents": 500,
                "tool_output_bytes": 65536
            },
            "privacy": { "telemetry": false, "content_retention_days": 7 },
            "mcp_servers": [{
                "id": "fake",
                "command": FIXTURE,
                "args": [],
                "env_allowlist": [],
                "enabled": true
            }],
            "hooks": []
        });
        std::fs::write(
            &self.config,
            serde_json::to_string_pretty(&config).unwrap(),
        )
        .unwrap();
    }

    fn write_script(&self, script: &str) {
        std::fs::write(&self.script, script).unwrap();
    }

    /// Path the `die` fixture tool appends one line to per invocation.
    fn trace_path(&self) -> PathBuf {
        self._root.path().join("die-trace.txt")
    }

    fn resume_args(&self, session: &str, profile: &str, acknowledge: bool) -> Vec<String> {
        let mut args: Vec<String> = [
            "resume",
            session,
            "--sandbox",
            "off",
            "--acknowledge-risk",
            "--profile",
            profile,
            "--provider",
            "replay",
            "--script",
            &self.script.display().to_string(),
            "--output",
            "ndjson",
            "--prompt",
            "continue",
        ]
        .iter()
        .map(|value| value.to_string())
        .collect();
        if acknowledge {
            args.push("--acknowledge-unknown".to_string());
        }
        args
    }

    fn run_args(&self, profile: &str) -> Vec<String> {
        [
            "run",
            "--sandbox",
            "off",
            "--acknowledge-risk",
            "--profile",
            profile,
            "--provider",
            "replay",
            "--script",
            &self.script.display().to_string(),
            "--output",
            "ndjson",
            "--prompt",
            "use the tool",
        ]
        .iter()
        .map(|value| value.to_string())
        .collect()
    }

    fn output(&self, args: &[String]) -> std::process::Output {
        let mut command = Command::new(BINARY);
        command
            .arg("--workspace")
            .arg(&self.workspace)
            .arg("--state-dir")
            .arg(&self.state)
            .args(args)
            .env("BOLLO_CONFIG", &self.config)
            .stdin(Stdio::null());
        command.output().unwrap()
    }

    fn database(&self) -> PathBuf {
        self.state.join("state.sqlite3")
    }
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn events(output: &std::process::Output) -> Vec<EventEnvelope> {
    stdout(output)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<EventEnvelope>(line)
                .unwrap_or_else(|err| panic!("not an event: {err}: {line}"))
        })
        .collect()
}

fn op_states(database: &PathBuf, session: &str) -> Vec<(String, OperationState)> {
    let store = SqliteStore::open(database).unwrap();
    let session = bollo_protocol::ids::SessionId::parse(session).unwrap();
    store
        .operations_for_session(&session)
        .unwrap()
        .into_iter()
        .map(|operation| (operation.tool_name, operation.state))
        .collect()
}

#[test]
fn balanced_mcp_call_asks_and_blocks_headless_before_any_effect() {
    let fixture = Fixture::new("balanced");
    fixture.write_script(
        r#"[{"deltas": [], "tool_calls": [{"call_id": "m1", "name": "mcp_fake_echo", "arguments": {"text": "hello"}}], "finish": "tool_use"}]"#,
    );
    let output = fixture.output(&fixture.run_args("balanced"));
    assert_eq!(output.status.code(), Some(3), "stderr: {}", stderr(&output));
    assert!(
        stderr(&output).contains("MCP tool(s) discovered"),
        "discovery is reported as a note: {}",
        stderr(&output)
    );

    let events = events(&output);
    let proposed = events
        .iter()
        .find(|event| event.event_type == EventType::ToolProposed)
        .expect("a tool.proposed event");
    assert_eq!(proposed.data["tool_name"], "mcp:fake:echo");
    assert_eq!(proposed.data["decision"], "ask");
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ApprovalRequested));
    assert!(
        !events.iter().any(|event| event.event_type == EventType::ToolResult),
        "a blocked ask has no tool result"
    );

    // The intent was never journaled and the server was never called.
    let operations = op_states(&fixture.database(), events[0].session_id.as_str());
    assert!(operations.is_empty(), "{operations:?}");
}

#[test]
fn unrestricted_mcp_calls_pass_the_gate_and_are_journaled() {
    let fixture = Fixture::new("unrestricted");
    fixture.write_script(
        r#"[
            {"deltas": [], "tool_calls": [{"call_id": "m1", "name": "mcp_fake_echo", "arguments": {"text": "hello"}}], "finish": "tool_use"},
            {"deltas": [], "tool_calls": [{"call_id": "m2", "name": "mcp:fake:fail", "arguments": {}}], "finish": "tool_use"},
            {"deltas": ["done\n"], "finish": "end_turn"}
        ]"#,
    );
    let output = fixture.output(&fixture.run_args("unrestricted"));
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));

    let events = events(&output);
    let results: Vec<&EventEnvelope> = events
        .iter()
        .filter(|event| event.event_type == EventType::ToolResult)
        .collect();
    assert_eq!(results.len(), 2, "one result per call");
    assert_eq!(results[0].data["status"], "succeeded");
    assert!(
        results[0].data["summary"]
            .as_str()
            .unwrap()
            .contains("echo:hello"),
        "server content stays visible: {}",
        results[0].data["summary"]
    );
    assert_eq!(results[1].data["status"], "failed");
    assert!(
        results[1].data["summary"]
            .as_str()
            .unwrap()
            .contains("intentional failure"),
        "{}",
        results[1].data["summary"]
    );

    // Intent journaled before each effect; canonical ids only.
    let operations = op_states(&fixture.database(), events[0].session_id.as_str());
    assert_eq!(
        operations,
        vec![
            ("mcp:fake:echo".to_string(), OperationState::Succeeded),
            ("mcp:fake:fail".to_string(), OperationState::Failed),
        ]
    );
}

#[test]
fn mid_call_disconnect_journals_unknown_quarantines_and_is_never_replayed() {
    let fixture = Fixture::new("unrestricted");
    let trace = fixture.trace_path();
    let script = json!([
        {"deltas": [], "tool_calls": [{"call_id": "m1", "name": "mcp_fake_die", "arguments": {"trace": trace.display().to_string()}}], "finish": "tool_use"},
        {"deltas": [], "tool_calls": [{"call_id": "m2", "name": "mcp_fake_echo", "arguments": {"text": "still there?"}}], "finish": "tool_use"},
        {"deltas": ["done\n"], "finish": "end_turn"}
    ]);
    fixture.write_script(&script.to_string());
    let output = fixture.output(&fixture.run_args("unrestricted"));
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));

    let events = events(&output);
    let results: Vec<&EventEnvelope> = events
        .iter()
        .filter(|event| event.event_type == EventType::ToolResult)
        .collect();
    assert_eq!(results.len(), 2, "one result per call");

    // The server died during the first call: an unknown effect, never retried.
    assert_eq!(results[0].data["status"], "unknown");
    let first = results[0].data["summary"].as_str().unwrap();
    assert!(
        first.contains("mcp transport closed"),
        "a mid-call death is a disconnect, not a timeout: {first}"
    );
    assert!(first.contains("quarantined"), "{first}");

    // Every later call to that server is refused before any IPC is attempted.
    assert_eq!(results[1].data["status"], "unknown");
    let second = results[1].data["summary"].as_str().unwrap();
    assert!(second.contains("quarantined"), "{second}");
    assert!(
        second.contains("fresh handshake"),
        "quarantine is a short-circuit, not another failed call: {second}"
    );

    let session = events[0].session_id.as_str();
    let operations = op_states(&fixture.database(), session);
    assert_eq!(
        operations,
        vec![
            ("mcp:fake:die".to_string(), OperationState::Unknown),
            ("mcp:fake:echo".to_string(), OperationState::Unknown),
        ]
    );

    // The fixture was reached exactly once; an internal retry would append a line.
    let recorded = std::fs::read_to_string(&trace).unwrap();
    assert_eq!(recorded.lines().count(), 1, "{recorded:?}");

    // Resume refuses until the unknown effect is acknowledged...
    fixture.write_script(r#"[{"deltas": ["resumed\n"], "finish": "end_turn"}]"#);
    let refused = fixture.output(&fixture.resume_args(session, "unrestricted", false));
    assert_eq!(refused.status.code(), Some(5), "stderr: {}", stderr(&refused));
    assert!(
        stderr(&refused).contains("--acknowledge-unknown"),
        "{}",
        stderr(&refused)
    );

    // ...and once acknowledged the unknown effect is never replayed: same
    // journal rows, same single fixture invocation.
    let resumed = fixture.output(&fixture.resume_args(session, "unrestricted", true));
    assert_eq!(resumed.status.code(), Some(0), "stderr: {}", stderr(&resumed));
    assert!(
        stderr(&resumed).contains("not replayed"),
        "{}",
        stderr(&resumed)
    );
    assert_eq!(op_states(&fixture.database(), session), operations);
    let recorded = std::fs::read_to_string(&trace).unwrap();
    assert_eq!(recorded.lines().count(), 1, "{recorded:?}");
}

#[test]
fn mid_session_tool_list_changes_refresh_catalog_and_policy_view() {
    let fixture = Fixture::new("unrestricted");
    fixture.write_script(
        r#"[
            {"deltas": [], "tool_calls": [{"call_id": "m1", "name": "mcp_fake_add_tool", "arguments": {"name": "extra"}}], "finish": "tool_use"},
            {"deltas": [], "tool_calls": [{"call_id": "m2", "name": "mcp_fake_extra", "arguments": {}}], "finish": "tool_use"},
            {"deltas": [], "tool_calls": [{"call_id": "m3", "name": "mcp_fake_remove_tool", "arguments": {"name": "extra"}}], "finish": "tool_use"},
            {"deltas": [], "tool_calls": [{"call_id": "m4", "name": "mcp_fake_extra", "arguments": {}}], "finish": "tool_use"},
            {"deltas": ["done\n"], "finish": "end_turn"}
        ]"#,
    );
    let output = fixture.output(&fixture.run_args("unrestricted"));
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));

    let events = events(&output);
    let results: Vec<&EventEnvelope> = events
        .iter()
        .filter(|event| event.event_type == EventType::ToolResult)
        .collect();
    assert_eq!(results.len(), 4, "one result per call");
    assert_eq!(results[0].data["status"], "succeeded");
    assert!(
        results[0].data["summary"]
            .as_str()
            .unwrap()
            .contains("added:extra"),
        "{}",
        results[0].data["summary"]
    );
    assert_eq!(results[1].data["status"], "succeeded");
    assert!(
        results[1].data["summary"]
            .as_str()
            .unwrap()
            .contains("dynamic:extra"),
        "the added tool was callable through the gate: {}",
        results[1].data["summary"]
    );
    assert_eq!(results[2].data["status"], "succeeded");
    assert!(
        results[2].data["summary"]
            .as_str()
            .unwrap()
            .contains("removed:extra"),
        "{}",
        results[2].data["summary"]
    );
    // A proposal for the removed tool is refused before policy and never
    // reaches the server or the journal.
    assert_eq!(results[3].data["status"], "denied");
    assert!(
        results[3].data["summary"]
            .as_str()
            .unwrap()
            .contains("unknown_tool"),
        "{}",
        results[3].data["summary"]
    );

    let operations = op_states(&fixture.database(), events[0].session_id.as_str());
    assert_eq!(
        operations,
        vec![
            ("mcp:fake:add_tool".to_string(), OperationState::Succeeded),
            ("mcp:fake:extra".to_string(), OperationState::Succeeded),
            ("mcp:fake:remove_tool".to_string(), OperationState::Succeeded),
        ]
    );
}

/// Trust, start and discover the fixture exactly as the composition root does.
fn connected_registry() -> McpRegistry {
    let cwd = std::env::current_dir().unwrap();
    let mut trust = TrustStore::new();
    let servers = vec![McpServerConfig {
        id: "fake".into(),
        command: FIXTURE.to_string(),
        args: vec![],
        env_allowlist: vec![],
        enabled: true,
    }];
    let (registry, warnings) = McpRegistry::connect(&servers, &cwd, &mut trust);
    assert!(warnings.is_empty(), "{warnings:?}");
    registry
}

#[test]
fn server_error_response_is_a_failed_call_not_a_quarantine() {
    let mut registry = connected_registry();

    // The server answers with a JSON-RPC error: definite failure, live
    // transport, no quarantine.
    let outcome = registry.dispatch("mcp:fake:fail", &json!({"protocol": true}));
    assert_eq!(outcome.status, ToolStatus::Failed);
    assert_eq!(
        outcome.error.as_ref().map(|error| error.code),
        Some(ErrorCode::McpError),
        "{}",
        outcome.summary
    );
    assert!(
        outcome.summary.contains("rejected the call"),
        "{}",
        outcome.summary
    );

    // The server answered, so the transport is healthy and its tools stay
    // usable.
    let echo = registry.dispatch("mcp:fake:echo", &json!({"text": "still here"}));
    assert_eq!(echo.status, ToolStatus::Succeeded, "{}", echo.summary);
    assert!(echo.summary.contains("echo:still here"), "{}", echo.summary);
}

#[test]
fn failed_relist_quarantines_the_server_and_shrinks_the_catalog() {
    let mut registry = connected_registry();
    assert_eq!(registry.tools.len(), 5);

    // The server adds a tool and then dies instead of answering the re-list its
    // own announcement triggered.
    let outcome = registry.dispatch(
        "mcp:fake:add_tool",
        &json!({"name": "extra", "die_on_list": true}),
    );
    assert_eq!(outcome.status, ToolStatus::Succeeded, "{}", outcome.summary);

    // Uncertainty shrinks the catalog, it never widens it: every tool of the
    // server disappears and the server is quarantined.
    let refreshed = registry
        .refresh_catalog()
        .expect("the announcement must force a re-list");
    assert!(refreshed.is_empty(), "{refreshed:?}");
    assert!(registry.tools.is_empty());

    let after = registry.dispatch("mcp:fake:echo", &json!({"text": "hi"}));
    assert_eq!(after.status, ToolStatus::Failed);
    assert!(
        after.summary.contains("unknown MCP tool"),
        "{}",
        after.summary
    );
}
