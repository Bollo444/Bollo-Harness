//! MCP stdio lifecycle against an independent fake server process.

use std::time::{Duration, Instant};

use serde_json::json;

use bollo_extensions::{McpClient, McpServerSpec, TrustStore};

fn spec() -> McpServerSpec {
    McpServerSpec {
        id: "fake".into(),
        command: env!("CARGO_BIN_EXE_bollo-mcp-fake-server").to_string(),
        args: vec![],
        env_allowlist: vec![],
        enabled: true,
    }
}

#[test]
fn untrusted_startup_is_refused() {
    let cwd = std::env::current_dir().unwrap();
    let trust = TrustStore::new();
    let error = McpClient::start(&spec(), &cwd, &trust)
        .err()
        .expect("untrusted startup must be refused");
    assert!(error.to_string().contains("trust"));
}

#[test]
fn handshake_list_call_and_shutdown() {
    let cwd = std::env::current_dir().unwrap();
    let spec = spec();
    let argv = vec![spec.command.clone()];
    let mut trust = TrustStore::new();
    trust.grant(&argv, &cwd, &[]).unwrap();

    let mut client = McpClient::start(&spec, &cwd, &trust).unwrap();
    let info = client.initialize().unwrap();
    assert_eq!(info["protocolVersion"], "2025-11-25");
    assert_eq!(info["serverInfo"]["name"], "bollo-fake");

    let tools = client.list_tools().unwrap();
    assert_eq!(tools.len(), 5);
    let echo = tools.iter().find(|tool| tool.name == "echo").unwrap();
    assert_eq!(echo.canonical_id, "mcp:fake:echo");
    assert!(echo.input_schema["properties"]["text"]["type"] == "string");

    let result = client.call_tool("echo", json!({"text": "hello"})).unwrap();
    assert!(!result.is_error);
    assert!(!result.truncated);
    assert_eq!(result.content[0]["text"], "echo:hello");

    let failure = client.call_tool("fail", json!({})).unwrap();
    assert!(failure.is_error);

    client.shutdown().unwrap();
}

#[test]
fn list_changed_announcement_triggers_a_catalog_refetch() {
    let cwd = std::env::current_dir().unwrap();
    let spec = spec();
    let argv = vec![spec.command.clone()];
    let mut trust = TrustStore::new();
    trust.grant(&argv, &cwd, &[]).unwrap();

    let mut client = McpClient::start(&spec, &cwd, &trust).unwrap();
    client.initialize().unwrap();
    let before = client.list_tools().unwrap();
    assert_eq!(before.len(), 5);
    assert!(!before.iter().any(|tool| tool.name == "extra"));

    // The announcement arrives while the call is in flight.
    let result = client
        .call_tool("add_tool", json!({"name": "extra"}))
        .unwrap();
    assert!(!result.is_error);
    assert_eq!(client.tools_changed(), 1, "the announcement is observed");

    let after = client.list_tools().unwrap();
    assert_eq!(after.len(), 6);
    let extra = after
        .iter()
        .find(|tool| tool.name == "extra")
        .expect("the added tool is advertised");
    assert_eq!(extra.canonical_id, "mcp:fake:extra");
    let result = client.call_tool("extra", json!({})).unwrap();
    assert_eq!(result.content[0]["text"], "dynamic:extra");

    // Removal announces as well and the catalog shrinks again.
    client
        .call_tool("remove_tool", json!({"name": "extra"}))
        .unwrap();
    assert_eq!(client.tools_changed(), 2);
    let after_removal = client.list_tools().unwrap();
    assert_eq!(after_removal.len(), 5);
    assert!(!after_removal.iter().any(|tool| tool.name == "extra"));
    client.shutdown().unwrap();
}

#[test]
fn announcement_sent_after_the_response_is_drained_while_idle() {
    let cwd = std::env::current_dir().unwrap();
    let spec = spec();
    let argv = vec![spec.command.clone()];
    let mut trust = TrustStore::new();
    trust.grant(&argv, &cwd, &[]).unwrap();

    let mut client = McpClient::start(&spec, &cwd, &trust).unwrap();
    client.initialize().unwrap();
    client.list_tools().unwrap();

    // The deferred announcement is written after the response, so it can still
    // be in flight when the call returns; the host polls between turns.
    client
        .call_tool("add_tool", json!({"name": "later", "defer": true}))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while client.tools_changed() == 0 && Instant::now() < deadline {
        client.poll_notifications();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        client.tools_changed(),
        1,
        "an idle announcement must not be lost"
    );

    let tools = client.list_tools().unwrap();
    assert_eq!(tools.len(), 6);
    assert!(tools.iter().any(|tool| tool.name == "later"));
    client.shutdown().unwrap();
}

#[test]
fn mid_call_death_is_a_disconnect_and_is_never_retried() {
    let cwd = std::env::current_dir().unwrap();
    let spec = spec();
    let argv = vec![spec.command.clone()];
    let mut trust = TrustStore::new();
    trust.grant(&argv, &cwd, &[]).unwrap();

    let trace_dir = tempfile::tempdir().unwrap();
    let trace = trace_dir.path().join("die-trace.txt");

    let mut client = McpClient::start(&spec, &cwd, &trust).unwrap();
    client.initialize().unwrap();
    client.list_tools().unwrap();

    // The server exits mid-call without replying: the caller must see the
    // transport die (not a timeout) and must not retry the call.
    let error = client
        .call_tool("die", json!({"trace": trace.display().to_string()}))
        .err()
        .expect("a dead server cannot answer");
    assert!(
        matches!(error, bollo_extensions::ExtensionError::Disconnected),
        "{error}"
    );

    // Quarantine is the host's only reaction: close the transport, never replay.
    client.quarantine();
    let recorded = std::fs::read_to_string(&trace).unwrap();
    assert_eq!(
        recorded.lines().count(),
        1,
        "the dead call must have been issued exactly once: {recorded:?}"
    );
}
