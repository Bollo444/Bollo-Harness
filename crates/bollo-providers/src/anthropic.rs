//! Anthropic Messages adapter (`POST /v1/messages`, streaming).
//!
//! System context is a top-level field; messages use user/assistant content
//! blocks; `tool_use` ids bind subsequent `tool_result` blocks. Fragmented
//! `input_json_delta` fragments are assembled and validated before dispatch.

use std::collections::BTreeMap;
use std::io::{BufRead, Read};
use std::sync::Arc;

use serde_json::{json, Value};

use bollo_protocol::cancel::CancellationToken;
use bollo_protocol::errors::ErrorCode;

use crate::stream::StreamAssembler;
use crate::{
    cancelled_error, ContentBlock, FinishReason, HttpRequest, ModelRequest, Provider,
    ProviderCapabilities, ProviderError, ProviderEvent, Role, StreamSummary, Transport, Usage,
};

/// Pinned, tested API version header.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct AnthropicAdapter {
    transport: Arc<dyn Transport>,
    api_key: String,
    base_url: String,
    capabilities: ProviderCapabilities,
    max_output_tokens: u64,
}

impl AnthropicAdapter {
    pub fn new(
        transport: Arc<dyn Transport>,
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        context_tokens: u64,
        max_output_tokens: u64,
    ) -> Self {
        Self {
            transport,
            api_key: api_key.into(),
            base_url: base_url.into(),
            capabilities: ProviderCapabilities {
                streaming: true,
                tool_calls: true,
                max_context_tokens: context_tokens,
                max_output_tokens,
                usage_reported: true,
            },
            max_output_tokens,
        }
    }

    pub fn build_request(&self, request: &ModelRequest) -> HttpRequest {
        let mut messages = Vec::new();
        for message in &request.messages {
            let role = match message.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            let mut content = Vec::new();
            for block in &message.content {
                match block {
                    ContentBlock::Text { text } => {
                        content.push(json!({"type": "text", "text": text}))
                    }
                    ContentBlock::ToolUse {
                        call_id,
                        name,
                        arguments,
                    } => content.push(json!({
                        "type": "tool_use",
                        "id": call_id,
                        "name": name,
                        "input": arguments
                    })),
                    ContentBlock::ToolResult {
                        call_id,
                        content: result,
                        is_error,
                    } => content.push(json!({
                        "type": "tool_result",
                        "tool_use_id": call_id,
                        "content": result,
                        "is_error": is_error
                    })),
                }
            }
            messages.push(json!({"role": role, "content": content}));
        }

        let max_tokens = if request.max_output_tokens == 0 {
            self.max_output_tokens
        } else {
            request.max_output_tokens
        };
        let mut body = json!({
            "model": request.model,
            "max_tokens": max_tokens,
            "stream": true,
            "messages": messages,
        });
        if !request.system.is_empty() {
            body["system"] = json!(request.system.join("\n\n"));
        }
        if !request.tools.is_empty() {
            let tools: Vec<Value> = request
                .tools
                .iter()
                .map(|tool| {
                    json!({
                        "name": tool.name,
                        "description": tool.description,
                        "input_schema": tool.input_schema
                    })
                })
                .collect();
            body["tools"] = json!(tools);
        }

        HttpRequest {
            url: format!("{}/v1/messages", self.base_url.trim_end_matches('/')),
            headers: vec![
                ("content-type".into(), "application/json".into()),
                ("accept".into(), "text/event-stream".into()),
                ("x-api-key".into(), self.api_key.clone()),
                ("anthropic-version".into(), ANTHROPIC_VERSION.into()),
            ],
            body: serde_json::to_vec(&body).expect("request body is serializable"),
            timeout: request.deadline,
        }
    }
}

impl Provider for AnthropicAdapter {
    fn id(&self) -> &'static str {
        "anthropic"
    }

    fn capabilities(&self) -> &ProviderCapabilities {
        &self.capabilities
    }

    fn stream(
        &self,
        request: &ModelRequest,
        cancel: &CancellationToken,
        sink: &mut dyn FnMut(ProviderEvent),
    ) -> Result<StreamSummary, ProviderError> {
        if cancel.is_cancelled() {
            return Err(cancelled_error());
        }
        let http = self.build_request(request);
        let response = self.transport.post(&http).map_err(|err| ProviderError {
            code: ErrorCode::ProviderError,
            message: err.to_string(),
            status: None,
            retryable: true,
        })?;
        if response.status != 200 {
            let mut body = String::new();
            let _ = response.body.take(65_536).read_to_string(&mut body);
            return Err(ProviderError::http(
                response.status,
                format!("provider rejected the request: {}", truncate(&body, 512)),
            ));
        }
        parse_stream(response.body, cancel, sink)
    }
}

/// Parse a Messages SSE stream. Public for fixture tests.
pub fn parse_stream<R: Read>(
    reader: R,
    cancel: &CancellationToken,
    sink: &mut dyn FnMut(ProviderEvent),
) -> Result<StreamSummary, ProviderError> {
    let mut reader = std::io::BufReader::new(reader);
    let mut assembler = StreamAssembler::new();
    let mut index_to_call: BTreeMap<u64, String> = BTreeMap::new();
    let mut usage = Usage::default();
    let mut finish = FinishReason::EndTurn;
    let mut line = String::new();
    loop {
        if cancel.is_cancelled() {
            return Err(cancelled_error());
        }
        line.clear();
        let read = reader
            .read_line(&mut line)
            .map_err(|err| stream_error(format!("stream read failed: {err}")))?;
        if read == 0 {
            break;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        let Some(payload) = trimmed.strip_prefix("data:") else {
            continue;
        };
        let payload = payload.trim();
        if payload.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(payload)
            .map_err(|err| stream_error(format!("malformed SSE payload: {err}")))?;
        match value["type"].as_str().unwrap_or("") {
            "message_start" => {
                if let Some(input) = value["message"]["usage"]["input_tokens"].as_u64() {
                    usage.input_tokens = Some(input);
                }
            }
            "content_block_start" => {
                let block = &value["content_block"];
                if block["type"] == "tool_use" {
                    let index = value["index"].as_u64().unwrap_or(0);
                    let call_id = block["id"].as_str().unwrap_or_default().to_string();
                    let name = block["name"].as_str().unwrap_or_default().to_string();
                    index_to_call.insert(index, call_id.clone());
                    assembler.begin(&call_id, &name);
                }
            }
            "content_block_delta" => {
                let index = value["index"].as_u64().unwrap_or(0);
                let delta = &value["delta"];
                match delta["type"].as_str().unwrap_or("") {
                    "text_delta" => {
                        if let Some(text) = delta["text"].as_str() {
                            sink(ProviderEvent::TextDelta(text.to_string()));
                        }
                    }
                    "input_json_delta" => {
                        let call_id = index_to_call.get(&index).cloned().ok_or_else(|| {
                            stream_error(
                                "input_json_delta arrived before its content_block_start"
                                    .to_string(),
                            )
                        })?;
                        if let Some(fragment) = delta["partial_json"].as_str() {
                            assembler.push_arguments(&call_id, fragment)?;
                        }
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(reason) = value["delta"]["stop_reason"].as_str() {
                    finish = map_stop_reason(reason);
                }
                if let Some(output) = value["usage"]["output_tokens"].as_u64() {
                    usage.output_tokens = Some(output);
                }
            }
            "message_stop" => break,
            "error" => {
                let message = value["error"]["message"]
                    .as_str()
                    .unwrap_or("provider stream error");
                return Err(stream_error(message.to_string()));
            }
            _ => {}
        }
    }
    let tool_intents = assembler.finish()?;
    if !tool_intents.is_empty() && finish == FinishReason::EndTurn {
        finish = FinishReason::ToolUse;
    }
    sink(ProviderEvent::Usage(usage.clone()));
    Ok(StreamSummary {
        finish,
        tool_intents,
        usage,
    })
}

fn map_stop_reason(reason: &str) -> FinishReason {
    match reason {
        "tool_use" => FinishReason::ToolUse,
        "max_tokens" => FinishReason::MaxOutput,
        "refusal" => FinishReason::Refusal,
        _ => FinishReason::EndTurn,
    }
}

fn stream_error(message: String) -> ProviderError {
    ProviderError::new(ErrorCode::ProviderError, message)
}

fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        text.to_string()
    } else {
        format!("{}…", &text[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::testing::FakeTransport;
    use crate::{ToolDefinition, ToolIntent};

    const FIXTURE: &str = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":120}}}\n",
        "\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n",
        "\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"I will \"}}\n",
        "\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"patch it.\"}}\n",
        "\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"apply_patch\",\"input\":{}}}\n",
        "\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"src/m\"}}\n",
        "\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"ain.rs\\\",\\\"old_te\"}}\n",
        "\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"xt\\\":\\\"a\\\",\\\"new_text\\\":\\\"b\\\"}\"}}\n",
        "\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":42}}\n",
        "\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n",
    );

    fn adapter(body: &str) -> (AnthropicAdapter, Arc<FakeTransport>) {
        let transport = Arc::new(FakeTransport::new(200, body.as_bytes()));
        (
            AnthropicAdapter::new(transport.clone(), "test-key", "https://api.anthropic.com", 200_000, 4096),
            transport,
        )
    }

    fn request() -> ModelRequest {
        ModelRequest {
            model: "configured-model".into(),
            system: vec!["be careful".into()],
            messages: vec![crate::ChatMessage::user_text("fix the parser")],
            tools: vec![ToolDefinition {
                name: "apply_patch".into(),
                description: "patch a file".into(),
                input_schema: json!({"type": "object"}),
            }],
            max_output_tokens: 1024,
            deadline: std::time::Duration::from_secs(120),
        }
    }

    #[test]
    fn request_shape_matches_the_messages_api() {
        let (adapter, transport) = adapter("");
        let http = adapter.build_request(&request());
        assert_eq!(http.url, "https://api.anthropic.com/v1/messages");
        assert!(http
            .headers
            .iter()
            .any(|(k, v)| k == "anthropic-version" && v == ANTHROPIC_VERSION));
        assert!(http.headers.iter().any(|(k, _)| k == "x-api-key"));
        let body: Value = serde_json::from_slice(&http.body).unwrap();
        assert_eq!(body["stream"], true);
        assert_eq!(body["system"], "be careful");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["tools"][0]["name"], "apply_patch");
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
        assert!(transport.last_request().is_none()); // build is pure
    }

    #[test]
    fn parses_text_and_fragmented_tool_arguments() {
        let (adapter, _transport) = adapter(FIXTURE);
        let cancel = CancellationToken::new();
        let mut deltas = String::new();
        let mut usage = Usage::default();
        let summary = adapter
            .stream(&request(), &cancel, &mut |event| match event {
                ProviderEvent::TextDelta(text) => deltas.push_str(&text),
                ProviderEvent::Usage(u) => usage = u,
            })
            .unwrap();
        assert_eq!(deltas, "I will patch it.");
        assert_eq!(usage.input_tokens, Some(120));
        assert_eq!(usage.output_tokens, Some(42));
        assert_eq!(summary.finish, FinishReason::ToolUse);
        assert_eq!(summary.tool_intents.len(), 1);
        let intent: &ToolIntent = &summary.tool_intents[0];
        assert_eq!(intent.call_id, "toolu_1");
        assert_eq!(intent.name, "apply_patch");
        assert_eq!(intent.arguments["path"], "src/main.rs");
        assert_eq!(intent.arguments["old_text"], "a");
    }

    #[test]
    fn invalid_tool_json_fails_without_dispatching() {
        let broken = FIXTURE.replace("\\\"new_text\\\":\\\"b\\\"}", "\\\"new_text\\\":");
        let (adapter, _transport) = adapter(&broken);
        let cancel = CancellationToken::new();
        let err = adapter
            .stream(&request(), &cancel, &mut |_| {})
            .unwrap_err();
        assert!(err.message.contains("not valid JSON"));
    }

    #[test]
    fn non_200_status_is_a_typed_error() {
        let transport = Arc::new(FakeTransport::new(401, b"{\"error\":\"bad key\"}".to_vec()));
        let adapter = AnthropicAdapter::new(transport, "k", "https://api.anthropic.com", 200_000, 4096);
        let cancel = CancellationToken::new();
        let err = adapter
            .stream(&request(), &cancel, &mut |_| {})
            .unwrap_err();
        assert_eq!(err.status, Some(401));
        assert_eq!(err.code, ErrorCode::CredentialMissing);
        assert!(!err.retryable);
    }

    #[test]
    fn cancellation_is_observed_before_reading() {
        let (adapter, _transport) = adapter(FIXTURE);
        let cancel = CancellationToken::new();
        cancel.cancel();
        let err = adapter
            .stream(&request(), &cancel, &mut |_| {})
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Cancelled);
    }
}
