//! MCP stdio client, protocol baseline pinned to `2025-11-25`.
//!
//! Lifecycle: trust check → supervised launch with filtered env → `initialize`
//! negotiation → `tools/list` (bounded pagination) → `tools/call` (through the
//! runtime gate; this client does not decide policy) → bounded shutdown.
//! A disconnect quarantines the server's tools; an uncertain side-effecting
//! call is never replayed.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, TryRecvError};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::trust::TrustStore;
use crate::ExtensionError;

pub const PROTOCOL_VERSION: &str = "2025-11-25";
pub const MAX_TOOLS_PER_SERVER: usize = 100;
pub const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);
pub const CALL_DEADLINE: Duration = Duration::from_secs(60);
pub const MAX_RESULT_BYTES: usize = 1_048_576;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerSpec {
    pub id: String,
    pub command: String,
    pub args: Vec<String>,
    pub env_allowlist: Vec<String>,
    pub enabled: bool,
}

/// Canonical MCP tool identity: `mcp:<server_id>:<tool_name>`.
pub fn canonical_tool_id(server_id: &str, tool_name: &str) -> String {
    format!("mcp:{server_id}:{tool_name}")
}

#[derive(Debug, Clone, PartialEq)]
pub struct McpTool {
    pub name: String,
    pub canonical_id: String,
    pub description: Option<String>,
    pub input_schema: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct McpCallResult {
    pub is_error: bool,
    pub content: Vec<Value>,
    pub truncated: bool,
}

pub struct McpClient {
    server_id: String,
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<Result<String, String>>,
    next_id: u64,
    tools: Vec<McpTool>,
    /// Monotonic count of `notifications/tools/list_changed` announcements.
    tools_changed: u64,
}

impl McpClient {
    /// Launch a configured server. Starting is execution: it requires an
    /// explicit trust record for the resolved executable, exact argv, cwd and
    /// requested environment names.
    pub fn start(
        spec: &McpServerSpec,
        cwd: &Path,
        trust: &TrustStore,
    ) -> Result<Self, ExtensionError> {
        if !spec.enabled {
            return Err(ExtensionError::Untrusted(format!(
                "mcp server {} is not enabled",
                spec.id
            )));
        }
        let mut argv = vec![spec.command.clone()];
        argv.extend(spec.args.clone());
        if !trust.verify(&argv, cwd, &spec.env_allowlist) {
            return Err(ExtensionError::Untrusted(format!(
                "mcp server {} has no matching trust record",
                spec.id
            )));
        }
        let mut command = Command::new(&spec.command);
        command
            .args(&spec.args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        command.env_clear();
        for key in [
            "PATH",
            "SystemRoot",
            "SYSTEMROOT",
            "TEMP",
            "TMP",
            "HOME",
        ] {
            if let Ok(value) = std::env::var(key) {
                command.env(key, value);
            }
        }
        for name in &spec.env_allowlist {
            if let Ok(value) = std::env::var(name) {
                command.env(name, value);
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let mut child = command
            .spawn()
            .map_err(|err| ExtensionError::Io(format!("cannot start {}: {err}", spec.id)))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ExtensionError::Mcp("child stdin unavailable".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ExtensionError::Mcp("child stdout unavailable".into()))?;
        let (sender, receiver) = channel();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(line) => {
                        if sender.send(Ok(line)).is_err() {
                            break;
                        }
                    }
                    Err(err) => {
                        let _ = sender.send(Err(err.to_string()));
                        break;
                    }
                }
            }
        });
        Ok(Self {
            server_id: spec.id.clone(),
            child,
            stdin,
            lines: receiver,
            next_id: 0,
            tools: Vec::new(),
            tools_changed: 0,
        })
    }

    pub fn server_id(&self) -> &str {
        &self.server_id
    }

    pub fn tools(&self) -> &[McpTool] {
        &self.tools
    }

    /// Monotonic count of `notifications/tools/list_changed` announcements seen
    /// from this server. The host re-lists when the count moves past the
    /// generation it last listed at, so an announcement that arrives during a
    /// request is never missed.
    pub fn tools_changed(&self) -> u64 {
        self.tools_changed
    }

    /// Drain server notifications and requests that arrived while no response
    /// was pending (an idle host between turns). Responses to abandoned ids are
    /// dropped; unsupported server requests are answered method-not-found.
    pub fn poll_notifications(&mut self) {
        loop {
            match self.lines.try_recv() {
                Ok(Ok(line)) => {
                    let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
                        continue;
                    };
                    let _ = self.handle_server_message(&value);
                }
                Ok(Err(_)) | Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
    }

    fn on_notification(&mut self, method: &str) {
        if method == "notifications/tools/list_changed" {
            self.tools_changed += 1;
        }
        // Unknown notifications are ignored by protocol design.
    }

    /// Notifications carry no id and only update client state; a server→client
    /// request is answered method-not-found instead of executing a generic
    /// fallback.
    fn handle_server_message(&mut self, value: &Value) -> Result<(), ExtensionError> {
        let Some(method) = value.get("method").and_then(|m| m.as_str()) else {
            return Ok(());
        };
        match value.get("id").cloned() {
            None => {
                self.on_notification(method);
                Ok(())
            }
            Some(request_id) => {
                let reply = json!({
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "error": {"code": -32601, "message": format!("unsupported method {method}")}
                });
                self.write_message(&reply)
            }
        }
    }

    fn write_message(&mut self, message: &Value) -> Result<(), ExtensionError> {
        let mut line = serde_json::to_string(message)?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.flush()?;
        Ok(())
    }

    fn request(&mut self, method: &str, params: Value) -> Result<u64, ExtensionError> {
        self.next_id += 1;
        let id = self.next_id;
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))?;
        Ok(id)
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), ExtensionError> {
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        }))
    }

    fn wait_for(&mut self, id: u64, deadline: Duration) -> Result<Value, ExtensionError> {
        let started = Instant::now();
        loop {
            let remaining = deadline.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(ExtensionError::Timeout(format!("response {id}")));
            }
            let line = match self.lines.recv_timeout(remaining) {
                Ok(Ok(line)) => line,
                // A reader error and a closed pipe are both the transport dying
                // mid-call, which must surface as `Disconnected`, not `Timeout`.
                Ok(Err(_)) => return Err(ExtensionError::Disconnected),
                Err(RecvTimeoutError::Disconnected) => return Err(ExtensionError::Disconnected),
                Err(RecvTimeoutError::Timeout) => {
                    return Err(ExtensionError::Timeout(format!("response {id}")))
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(line.trim())
                .map_err(|err| ExtensionError::Mcp(format!("invalid JSON-RPC line: {err}")))?;
            if let Some(error) = value.get("error") {
                if value.get("id").and_then(|v| v.as_u64()) == Some(id) {
                    // The server answered: a definite failure on a live
                    // transport, not a disconnect.
                    return Err(ExtensionError::ServerError(error.to_string()));
                }
            }
            if value.get("id").and_then(|v| v.as_u64()) == Some(id) {
                return Ok(value.get("result").cloned().unwrap_or(Value::Null));
            }
            self.handle_server_message(&value)?;
        }
    }

    /// Version/capability negotiation; unsupported versions are rejected.
    pub fn initialize(&mut self) -> Result<Value, ExtensionError> {
        let id = self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "bollo", "version": env!("CARGO_PKG_VERSION")}
            }),
        )?;
        let result = self.wait_for(id, HANDSHAKE_DEADLINE)?;
        let version = result["protocolVersion"].as_str().unwrap_or_default();
        if version != PROTOCOL_VERSION {
            let _ = self.shutdown_inner();
            return Err(ExtensionError::Mcp(format!(
                "unsupported protocol version {version:?}; Bollo is pinned to {PROTOCOL_VERSION}"
            )));
        }
        self.notify("notifications/initialized", json!({}))?;
        Ok(result)
    }

    /// Discover tools with bounded pagination and schema validation.
    pub fn list_tools(&mut self) -> Result<Vec<McpTool>, ExtensionError> {
        let mut tools: Vec<McpTool> = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = match &cursor {
                Some(cursor) => json!({"cursor": cursor}),
                None => json!({}),
            };
            let id = self.request("tools/list", params)?;
            let result = self.wait_for(id, HANDSHAKE_DEADLINE)?;
            for tool in result["tools"].as_array().cloned().unwrap_or_default() {
                let name = tool["name"].as_str().unwrap_or_default().to_string();
                if name.is_empty() {
                    continue;
                }
                let schema = tool
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object"}));
                if !schema.is_object() {
                    return Err(ExtensionError::Mcp(format!(
                        "tool {name} has a non-object input schema"
                    )));
                }
                if tools.iter().any(|existing| existing.name == name) {
                    continue;
                }
                tools.push(McpTool {
                    canonical_id: canonical_tool_id(&self.server_id, &name),
                    name,
                    description: tool["description"].as_str().map(str::to_string),
                    input_schema: schema,
                });
                if tools.len() > MAX_TOOLS_PER_SERVER {
                    return Err(ExtensionError::Mcp(format!(
                        "tool catalog exceeds {MAX_TOOLS_PER_SERVER} entries"
                    )));
                }
            }
            cursor = result["nextCursor"]
                .as_str()
                .filter(|cursor| !cursor.is_empty())
                .map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
        self.tools = tools.clone();
        Ok(tools)
    }

    /// Call a tool. The caller must have already passed the runtime policy gate
    /// and approval broker; MCP annotations never override policy.
    pub fn call_tool(
        &mut self,
        name: &str,
        arguments: Value,
    ) -> Result<McpCallResult, ExtensionError> {
        let id = self.request(
            "tools/call",
            json!({"name": name, "arguments": arguments}),
        )?;
        let result = self.wait_for(id, CALL_DEADLINE)?;
        let is_error = result["isError"].as_bool().unwrap_or(false);
        let mut content = Vec::new();
        let mut used = 0usize;
        let mut truncated = false;
        for block in result["content"].as_array().cloned().unwrap_or_default() {
            let size = block.to_string().len();
            if used + size > MAX_RESULT_BYTES {
                truncated = true;
                break;
            }
            used += size;
            content.push(block);
        }
        Ok(McpCallResult {
            is_error,
            content,
            truncated,
        })
    }

    /// Quarantine after a disconnect: close the transport, never replay calls.
    pub fn quarantine(mut self) {
        drop(self.stdin);
        let _ = wait_or_kill(&mut self.child, Duration::from_secs(2));
    }

    pub fn shutdown(mut self) -> Result<(), ExtensionError> {
        drop(self.stdin);
        wait_or_kill(&mut self.child, Duration::from_secs(3))
    }

    fn shutdown_inner(&mut self) {
        // Best-effort; the child owns the transport, and we hold only a handle.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn wait_or_kill(child: &mut Child, grace: Duration) -> Result<(), ExtensionError> {
    let deadline = Instant::now() + grace;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}

/// Environment keys Bollo injects for stdio servers (kept here so tests and
/// docs share one list).
pub fn default_env_allowlist() -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for key in ["PATH", "SystemRoot", "SYSTEMROOT", "TEMP", "TMP", "HOME"] {
        if let Ok(value) = std::env::var(key) {
            map.insert(key.to_string(), value);
        }
    }
    map
}

/// Path used when launching; kept for provenance display.
pub fn resolved_command(spec: &McpServerSpec) -> Option<PathBuf> {
    crate::trust::resolve_program(&spec.command)
}
