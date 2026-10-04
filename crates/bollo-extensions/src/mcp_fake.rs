//! Test fixture: a minimal MCP stdio server.
//!
//! This is not part of the product surface. It exists so the MCP client, the
//! trust binding and the runtime's MCP gate can be exercised against an
//! independent process speaking newline-delimited JSON-RPC. It implements
//! `initialize`, `ping`, `tools/list` and `tools/call` over a mutable catalog:
//!
//! - `echo` returns `echo:<text>`, `fail` reports `isError` (or, with
//!   `"protocol": true`, answers a JSON-RPC error instead of a result), and
//!   `die` appends one line to the file named by its `trace` argument and then
//!   exits the process without replying (the transport dies mid-call).
//! - `add_tool` and `remove_tool` mutate the catalog and send
//!   `notifications/tools/list_changed` — before the response by default, or
//!   after it when called with `"defer": true`, so both the in-flight and the
//!   idle-notification paths can be tested deterministically. `add_tool` with
//!   `"die_on_list": true` announces and then exits instead of answering the
//!   next `tools/list`, exercising the failed-re-list quarantine.
//! - Any tool added at runtime answers `dynamic:<name>`.
//!
//! Other crates ship a five-line binary that calls [`serve_stdio`], so their
//! integration tests can spawn the fixture without depending on this crate's
//! build artifacts.

use std::io::{self, BufRead, Write};

use serde_json::{json, Value};

/// Serve the fake server on stdin/stdout until the input closes.
pub fn serve_stdio() {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut catalog = base_tools();
    let mut announce_after_response = false;
    let mut die_before_next_list = false;
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let method = message["method"].as_str().unwrap_or_default();
        let id = message.get("id").cloned();
        let result: Option<Value> = match method {
            "initialize" => Some(json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "bollo-fake", "version": "0.1.0"}
            })),
            "ping" => Some(json!({})),
            "tools/list" => {
                if die_before_next_list {
                    // The announcement was a lie: the server dies instead of
                    // answering the re-list it triggered.
                    std::process::exit(9);
                }
                Some(json!({"tools": catalog.clone()}))
            }
            "tools/call" => {
                let tool = message["params"]["name"].as_str().unwrap_or_default();
                if tool == "die" {
                    record_and_exit(&message);
                }
                let arguments = &message["params"]["arguments"];
                let name = arguments["name"].as_str().unwrap_or_default();
                let defer = arguments["defer"].as_bool().unwrap_or(false);
                match tool {
                    "fail" => {
                        if arguments["protocol"].as_bool().unwrap_or(false) {
                            // No result: the caller receives a JSON-RPC error
                            // response, a definite failure on a live transport.
                            None
                        } else {
                            Some(json!({
                                "isError": true,
                                "content": [{"type": "text", "text": "intentional failure"}]
                            }))
                        }
                    }
                    "add_tool" => {
                        if !name.is_empty()
                            && !catalog.iter().any(|entry| entry["name"].as_str() == Some(name))
                        {
                            catalog.push(dynamic_tool(name));
                        }
                        die_before_next_list = arguments["die_on_list"].as_bool().unwrap_or(false);
                        announce_after_response = defer;
                        if !defer {
                            announce_list_changed(&mut stdout);
                        }
                        Some(json!({
                            "isError": false,
                            "content": [{"type": "text", "text": format!("added:{name}")}]
                        }))
                    }
                    "remove_tool" => {
                        catalog.retain(|entry| entry["name"].as_str() != Some(name));
                        announce_after_response = defer;
                        if !defer {
                            announce_list_changed(&mut stdout);
                        }
                        Some(json!({
                            "isError": false,
                            "content": [{"type": "text", "text": format!("removed:{name}")}]
                        }))
                    }
                    "echo" => {
                        let text = arguments["text"].as_str().unwrap_or_default();
                        Some(json!({
                            "isError": false,
                            "content": [{"type": "text", "text": format!("echo:{text}")}]
                        }))
                    }
                    _ if catalog
                        .iter()
                        .any(|entry| entry["name"].as_str() == Some(tool)) =>
                    {
                        Some(json!({
                            "isError": false,
                            "content": [{"type": "text", "text": format!("dynamic:{tool}")}]
                        }))
                    }
                    _ => {
                        let text = arguments["text"].as_str().unwrap_or_default();
                        Some(json!({
                            "isError": false,
                            "content": [{"type": "text", "text": format!("echo:{text}")}]
                        }))
                    }
                }
            }
            _ => None,
        };
        if let Some(id) = id {
            let response = match result {
                Some(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                None => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {"code": -32601, "message": "method not found"}
                }),
            };
            let _ = writeln!(stdout, "{response}");
            let _ = stdout.flush();
            if announce_after_response {
                announce_list_changed(&mut stdout);
                announce_after_response = false;
            }
        }
    }
}

/// The catalog the fixture starts with. `add_tool`/`remove_tool` are ordinary
/// listed tools on purpose: a catalog change is only reachable through the same
/// gate as any other call.
fn base_tools() -> Vec<Value> {
    vec![
        json!({
            "name": "echo",
            "description": "echo text",
            "inputSchema": {
                "type": "object",
                "properties": {"text": {"type": "string"}},
                "required": ["text"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "fail",
            "description": "always returns isError",
            "inputSchema": {"type": "object"}
        }),
        json!({
            "name": "die",
            "description": "records the call, then exits without replying",
            "inputSchema": {
                "type": "object",
                "properties": {"trace": {"type": "string"}},
                "additionalProperties": false
            }
        }),
        json!({
            "name": "add_tool",
            "description": "adds a tool to the catalog and announces the change",
            "inputSchema": {
                "type": "object",
                "properties": {"name": {"type": "string"}, "defer": {"type": "boolean"}},
                "required": ["name"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "remove_tool",
            "description": "removes a tool from the catalog and announces the change",
            "inputSchema": {
                "type": "object",
                "properties": {"name": {"type": "string"}, "defer": {"type": "boolean"}},
                "required": ["name"],
                "additionalProperties": false
            }
        }),
    ]
}

fn dynamic_tool(name: &str) -> Value {
    json!({
        "name": name,
        "description": format!("dynamic tool added by the fixture: {name}"),
        "inputSchema": {
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "additionalProperties": false
        }
    })
}

fn announce_list_changed(stdout: &mut impl Write) {
    let notification = json!({
        "jsonrpc": "2.0",
        "method": "notifications/tools/list_changed",
        "params": {}
    });
    let _ = writeln!(stdout, "{notification}");
    let _ = stdout.flush();
}

/// Record the invocation if a `trace` path was given, then die mid-call: no
/// response is ever written, so the caller only sees the transport close.
fn record_and_exit(message: &Value) -> ! {
    if let Some(trace) = message["params"]["arguments"]["trace"].as_str() {
        if !trace.is_empty() {
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(trace)
            {
                let _ = writeln!(file, "die");
            }
        }
    }
    std::process::exit(7);
}
