//! MCP tools discovered from a real fake server process join the same runtime
//! gate as built-ins: prepare → policy → mode → approval → journal intent →
//! execute once → after-hook. The test is the host that owns trust and the
//! stdio connection; `bollo-core` only sees the `McpDispatch` port.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};

use bollo_core::NoChannel;
use bollo_extensions::{McpClient, McpServerSpec};
use bollo_providers::fake::ScriptedResponse;
use bollo_protocol::vocab::{Profile, TerminalState};
use bollo_protocol::EventType;
use bollo_store::OperationState;
use bollo_tests::{tool_intent, tool_response, Harness};
use bollo_tools::{McpDispatch, McpDispatchOutcome, McpToolSpec};

const FIXTURE: &str = env!("CARGO_BIN_EXE_bollo-mcp-tests-fixture");

/// Dispatch adapter around the real stdio client, counting calls so tests can
/// prove whether the server was reached at all.
struct TestDispatch {
    client: McpClient,
    calls: Arc<AtomicUsize>,
    /// Client generation of the catalog currently attached to the harness.
    listed: u64,
}

/// Map discovered extension tools onto the runtime's catalog entries.
fn specs_for(tools: &[bollo_extensions::McpTool]) -> Vec<McpToolSpec> {
    tools
        .iter()
        .map(|tool| McpToolSpec {
            canonical_id: tool.canonical_id.clone(),
            provider_name: bollo_tools::provider_safe_tool_name(&tool.canonical_id),
            server_id: "fake".to_string(),
            tool_name: tool.name.clone(),
            description: tool.description.clone().unwrap_or_default(),
            input_schema: tool.input_schema.clone(),
        })
        .collect()
}

fn preview(content: &[Value]) -> String {
    let mut text = String::new();
    for block in content {
        if let Some(part) = block.get("text").and_then(Value::as_str) {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(part);
        }
    }
    text
}

impl McpDispatch for TestDispatch {
    fn dispatch(&mut self, canonical_id: &str, arguments: &Value) -> McpDispatchOutcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let tool = canonical_id.rsplit(':').next().unwrap_or_default().to_string();
        match self.client.call_tool(&tool, arguments.clone()) {
            Ok(result) => {
                let mut outcome =
                    McpDispatchOutcome::from_result(result.content, result.is_error, result.truncated);
                let text = preview(&outcome.content);
                outcome.summary = if text.is_empty() {
                    format!("{canonical_id} returned no content")
                } else {
                    format!("{canonical_id}: {text}")
                };
                outcome
            }
            Err(err) => McpDispatchOutcome::unknown(format!("{canonical_id} disconnected: {err}")),
        }
    }

    fn refresh_catalog(&mut self) -> Option<Vec<McpToolSpec>> {
        self.client.poll_notifications();
        let generation = self.client.tools_changed();
        if generation == self.listed {
            return None;
        }
        let tools = self.client.list_tools().ok()?;
        self.listed = self.client.tools_changed();
        Some(specs_for(&tools))
    }
}

/// Trust, start, handshake and discover the fake server, then attach it to the
/// harness exactly as the composition root would.
fn attach_fake_server(harness: &mut Harness) -> Arc<AtomicUsize> {
    let cwd = std::env::current_dir().unwrap();
    let spec = McpServerSpec {
        id: "fake".into(),
        command: FIXTURE.to_string(),
        args: Vec::new(),
        env_allowlist: Vec::new(),
        enabled: true,
    };
    let argv = vec![spec.command.clone()];
    harness.trust.grant(&argv, &cwd, &[]).unwrap();
    let mut client = McpClient::start(&spec, &cwd, &harness.trust).unwrap();
    client.initialize().unwrap();
    let tools = client.list_tools().unwrap();
    assert_eq!(
        tools.len(),
        5,
        "fake server advertises echo, fail, die, add_tool and remove_tool"
    );
    let listed = client.tools_changed();
    let calls = Arc::new(AtomicUsize::new(0));
    harness.mcp_tools = specs_for(&tools);
    harness.mcp = Some(Box::new(TestDispatch {
        client,
        calls: calls.clone(),
        listed,
    }));
    calls
}

fn proposed(events: &[bollo_protocol::events::EventEnvelope]) -> &bollo_protocol::events::EventEnvelope {
    events
        .iter()
        .find(|event| event.event_type == EventType::ToolProposed)
        .expect("a tool.proposed event")
}

fn tool_result(events: &[bollo_protocol::events::EventEnvelope]) -> &bollo_protocol::events::EventEnvelope {
    events
        .iter()
        .find(|event| event.event_type == EventType::ToolResult)
        .expect("a tool.result event")
}

#[test]
fn discovered_mcp_tools_join_the_model_list_and_pass_the_gate() {
    let script = vec![
        tool_response(vec![tool_intent(
            "c1",
            "mcp_fake_echo",
            json!({"text": "hello"}),
        )]),
        ScriptedResponse::text("done"),
    ];
    let mut harness = Harness::new(Profile::Unrestricted, 5, script);
    let calls = attach_fake_server(&mut harness);

    let (outcome, events) = harness.run("use the fake tool", vec![]);
    assert_eq!(outcome.exit_code, 0, "reason: {:?}", outcome.reason);
    assert_eq!(outcome.state, TerminalState::Completed);

    // The model-facing list carries the id-safe spelling next to the built-ins.
    let requests = harness.provider.requests();
    assert_eq!(requests.len(), 2, "tool turn plus final turn");
    let names: Vec<&str> = requests[0]
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect();
    assert!(names.contains(&"mcp_fake_echo"), "{names:?}");
    assert!(names.contains(&"mcp_fake_fail"), "{names:?}");
    assert_eq!(
        names.len(),
        12,
        "seven built-ins plus five discovered MCP tools"
    );

    // The result content reached the model on the next request.
    let second = serde_json::to_string(&requests[1].messages).unwrap();
    assert!(second.contains("echo:hello"), "{second}");

    let proposed = proposed(&events);
    assert_eq!(proposed.data["tool_name"], "mcp:fake:echo");
    assert_eq!(proposed.data["decision"], "allow");
    let result = tool_result(&events);
    assert_eq!(result.data["status"], "succeeded");
    assert!(
        result.data["summary"].as_str().unwrap().contains("echo:hello"),
        "{}",
        result.data["summary"]
    );

    // Intent was journaled before the effect, and the call happened exactly once.
    let operations = harness
        .store
        .operations_for_session(&harness.session)
        .unwrap();
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].tool_name, "mcp:fake:echo");
    assert_eq!(operations[0].state, OperationState::Succeeded);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn canonical_spelling_resolves_to_the_same_registered_tool() {
    let script = vec![
        tool_response(vec![tool_intent(
            "c1",
            "mcp:fake:echo",
            json!({"text": "again"}),
        )]),
        ScriptedResponse::text("done"),
    ];
    let mut harness = Harness::new(Profile::Unrestricted, 5, script);
    let calls = attach_fake_server(&mut harness);

    let (outcome, events) = harness.run("canonical spelling", vec![]);
    assert_eq!(outcome.exit_code, 0);
    assert_eq!(proposed(&events).data["tool_name"], "mcp:fake:echo");
    assert_eq!(tool_result(&events).data["status"], "succeeded");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn balanced_ask_blocks_headless_without_journal_or_dispatch() {
    let script = vec![tool_response(vec![tool_intent(
        "c1",
        "mcp_fake_echo",
        json!({"text": "hello"}),
    )])];
    let mut harness = Harness::new(Profile::Balanced, 5, script);
    let calls = attach_fake_server(&mut harness);

    let (outcome, events) = harness.run_with_channel("ask me", &mut NoChannel);
    assert_eq!(outcome.exit_code, 3);
    assert_eq!(outcome.state, TerminalState::Blocked);
    assert_eq!(outcome.reason.as_deref(), Some("approval_required"));
    assert_eq!(proposed(&events).data["decision"], "ask");
    assert!(events
        .iter()
        .any(|event| event.event_type == EventType::ApprovalRequested));
    assert!(
        !events.iter().any(|event| event.event_type == EventType::ToolResult),
        "a blocked ask has no tool result"
    );

    // Approval is requested before any journal entry; nothing reached the server.
    assert!(harness
        .store
        .operations_for_session(&harness.session)
        .unwrap()
        .is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn denied_approval_never_reaches_the_server() {
    let script = vec![
        tool_response(vec![tool_intent(
            "c1",
            "mcp_fake_echo",
            json!({"text": "hello"}),
        )]),
        ScriptedResponse::text("stopped"),
    ];
    let mut harness = Harness::new(Profile::Balanced, 5, script);
    let calls = attach_fake_server(&mut harness);

    let (outcome, events) = harness.run("deny me", vec![false]);
    assert_eq!(outcome.exit_code, 0);
    assert_eq!(tool_result(&events).data["status"], "denied");
    assert!(harness
        .store
        .operations_for_session(&harness.session)
        .unwrap()
        .is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn mid_call_death_is_journaled_unknown_exactly_once() {
    let trace_dir = tempfile::tempdir().unwrap();
    let trace = trace_dir.path().join("die-trace.txt");
    let script = vec![
        tool_response(vec![tool_intent(
            "c1",
            "mcp_fake_die",
            json!({"trace": trace.display().to_string()}),
        )]),
        ScriptedResponse::text("done"),
    ];
    let mut harness = Harness::new(Profile::Unrestricted, 5, script);
    let calls = attach_fake_server(&mut harness);

    let (outcome, events) = harness.run("kill the server mid-call", vec![]);
    assert_eq!(outcome.exit_code, 0, "reason: {:?}", outcome.reason);

    // The transport death is an unknown effect, not a failed tool: the server
    // never answered, so Bollo cannot claim the call did or did not happen.
    let result = tool_result(&events);
    assert_eq!(result.data["status"], "unknown");
    assert!(
        result.data["summary"]
            .as_str()
            .unwrap()
            .contains("disconnected"),
        "the transport death stays visible: {}",
        result.data["summary"]
    );

    let operations = harness
        .store
        .operations_for_session(&harness.session)
        .unwrap();
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].tool_name, "mcp:fake:die");
    assert_eq!(operations[0].state, OperationState::Unknown);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "a dead call is never retried");

    // One line means the fixture was reached exactly once; a replay would
    // append a second one.
    let recorded = std::fs::read_to_string(&trace).unwrap();
    assert_eq!(recorded.lines().count(), 1, "{recorded:?}");
}

#[test]
fn server_reported_tool_error_is_failed_not_unknown() {
    let script = vec![
        tool_response(vec![tool_intent("c1", "mcp_fake_fail", json!({}))]),
        ScriptedResponse::text("done"),
    ];
    let mut harness = Harness::new(Profile::Unrestricted, 5, script);
    let calls = attach_fake_server(&mut harness);

    let (outcome, events) = harness.run("fail the tool", vec![]);
    assert_eq!(outcome.exit_code, 0);
    let result = tool_result(&events);
    assert_eq!(result.data["status"], "failed");
    assert!(
        result.data["summary"]
            .as_str()
            .unwrap()
            .contains("intentional failure"),
        "server content stays visible: {}",
        result.data["summary"]
    );

    let operations = harness
        .store
        .operations_for_session(&harness.session)
        .unwrap();
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].state, OperationState::Failed);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn announced_tool_list_changes_refresh_the_model_view_mid_run() {
    let script = vec![
        tool_response(vec![tool_intent(
            "c1",
            "mcp_fake_add_tool",
            json!({"name": "extra"}),
        )]),
        tool_response(vec![tool_intent("c2", "mcp_fake_extra", json!({}))]),
        tool_response(vec![tool_intent(
            "c3",
            "mcp_fake_remove_tool",
            json!({"name": "extra"}),
        )]),
        tool_response(vec![tool_intent("c4", "mcp_fake_extra", json!({}))]),
        ScriptedResponse::text("done"),
    ];
    let mut harness = Harness::new(Profile::Unrestricted, 5, script);
    let calls = attach_fake_server(&mut harness);

    let (outcome, events) = harness.run("change the catalog mid-session", vec![]);
    assert_eq!(outcome.exit_code, 0, "reason: {:?}", outcome.reason);

    // The model-facing list follows the server's announcements in both
    // directions, within the same run.
    let requests = harness.provider.requests();
    assert_eq!(requests.len(), 5, "four tool turns plus the final turn");
    let names = |index: usize| -> Vec<String> {
        requests[index]
            .tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect()
    };
    assert!(!names(0).contains(&"mcp_fake_extra".to_string()));
    assert!(
        names(1).contains(&"mcp_fake_extra".to_string()),
        "an add announcement must reach the next request: {:?}",
        names(1)
    );
    assert!(names(2).contains(&"mcp_fake_extra".to_string()));
    assert!(
        !names(3).contains(&"mcp_fake_extra".to_string()),
        "a remove announcement must shrink the next request: {:?}",
        names(3)
    );

    let results: Vec<&bollo_protocol::events::EventEnvelope> = events
        .iter()
        .filter(|event| event.event_type == EventType::ToolResult)
        .collect();
    assert_eq!(results.len(), 4);
    assert_eq!(results[0].data["status"], "succeeded");
    assert_eq!(results[1].data["status"], "succeeded");
    assert!(
        results[1].data["summary"]
            .as_str()
            .unwrap()
            .contains("dynamic:extra"),
        "{}",
        results[1].data["summary"]
    );
    assert_eq!(results[2].data["status"], "succeeded");
    assert_eq!(
        results[3].data["status"], "denied",
        "the removed tool is refused before dispatch: {}",
        results[3].data["summary"]
    );
    assert!(
        results[3].data["summary"]
            .as_str()
            .unwrap()
            .contains("unknown_tool"),
        "{}",
        results[3].data["summary"]
    );

    // Only the three real effects were journaled; the stale proposal never
    // reached policy, the journal or the server.
    let operations = harness
        .store
        .operations_for_session(&harness.session)
        .unwrap();
    let journaled: Vec<&str> = operations.iter().map(|op| op.tool_name.as_str()).collect();
    assert_eq!(
        journaled,
        vec!["mcp:fake:add_tool", "mcp:fake:extra", "mcp:fake:remove_tool"]
    );
    assert!(operations
        .iter()
        .all(|op| op.state == OperationState::Succeeded));
    assert_eq!(calls.load(Ordering::SeqCst), 3, "the removed tool was never dispatched");
}
