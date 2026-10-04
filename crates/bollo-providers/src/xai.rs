//! xAI Chat Completions adapter (`POST /v1/chat/completions`, streaming).
//!
//! OpenAI-style messages and function tools. Argument fragments arrive by tool
//! call index and are assembled by id before validation; only advertised
//! parameters are sent.

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

pub struct XaiAdapter {
    transport: Arc<dyn Transport>,
    api_key: String,
    base_url: String,
    capabilities: ProviderCapabilities,
    max_output_tokens: u64,
}

impl XaiAdapter {
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
        if !request.system.is_empty() {
            messages.push(json!({"role": "system", "content": request.system.join("\n\n")}));
        }
        for message in &request.messages {
            match message.role {
                Role::Assistant => {
                    let mut text = String::new();
                    let mut tool_calls = Vec::new();
                    for block in &message.content {
                        match block {
                            ContentBlock::Text { text: part } => text.push_str(part),
                            ContentBlock::ToolUse {
                                call_id,
                                name,
                                arguments,
                            } => tool_calls.push(json!({
                                "id": call_id,
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "arguments": serde_json::to_string(arguments).unwrap_or_default()
                                }
                            })),
                            ContentBlock::ToolResult { .. } => {}
                        }
                    }
                    let mut value = json!({"role": "assistant"});
                    value["content"] = if text.is_empty() {
                        Value::Null
                    } else {
                        json!(text)
                    };
                    if !tool_calls.is_empty() {
                        value["tool_calls"] = json!(tool_calls);
                    }
                    messages.push(value);
                }
                Role::User => {
                    let mut tool_results = Vec::new();
                    let mut text = String::new();
                    for block in &message.content {
                        match block {
                            ContentBlock::Text { text: part } => text.push_str(part),
                            ContentBlock::ToolResult {
                                call_id,
                                content,
                                ..
                            } => tool_results.push(json!({
                                "role": "tool",
                                "tool_call_id": call_id,
                                "content": content
                            })),
                            ContentBlock::ToolUse { .. } => {}
                        }
                    }
                    messages.extend(tool_results);
                    if !text.is_empty() {
                        messages.push(json!({"role": "user", "content": text}));
                    }
                }
            }
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
        if !request.tools.is_empty() {
            let tools: Vec<Value> = request
                .tools
                .iter()
                .map(|tool| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.input_schema
                        }
                    })
                })
                .collect();
            body["tools"] = json!(tools);
        }

        HttpRequest {
            url: format!("{}/v1/chat/completions", self.base_url.trim_end_matches('/')),
            headers: vec![
                ("content-type".into(), "application/json".into()),
                ("accept".into(), "text/event-stream".into()),
                ("authorization".into(), format!("Bearer {}", self.api_key)),
            ],
            body: serde_json::to_vec(&body).expect("request body is serializable"),
            timeout: request.deadline,
        }
    }
}

impl Provider for XaiAdapter {
    fn id(&self) -> &'static str {
        "xai"
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

/// Parse a Chat Completions SSE stream. Public for fixture tests.
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
        if payload == "[DONE]" {
            break;
        }
        let value: Value = serde_json::from_str(payload)
            .map_err(|err| stream_error(format!("malformed SSE payload: {err}")))?;
        if let Some(usage_value) = value.get("usage") {
            if !usage_value.is_null() {
                if let Some(prompt) = usage_value["prompt_tokens"].as_u64() {
                    usage.input_tokens = Some(prompt);
                }
                if let Some(completion) = usage_value["completion_tokens"].as_u64() {
                    usage.output_tokens = Some(completion);
                }
            }
        }
        let Some(choice) = value["choices"].as_array().and_then(|choices| choices.first()) else {
            continue;
        };
        if let Some(content) = choice["delta"]["content"].as_str() {
            if !content.is_empty() {
                sink(ProviderEvent::TextDelta(content.to_string()));
            }
        }
        if let Some(tool_calls) = choice["delta"]["tool_calls"].as_array() {
            for call in tool_calls {
                let index = call["index"].as_u64().unwrap_or(0);
                if let Some(id) = call["id"].as_str() {
                    if !id.is_empty() {
                        let name = call["function"]["name"].as_str().unwrap_or_default();
                        index_to_call.insert(index, id.to_string());
                        assembler.begin(id, name);
                    }
                }
                if let Some(fragment) = call["function"]["arguments"].as_str() {
                    if !fragment.is_empty() {
                        let call_id = index_to_call.get(&index).cloned().ok_or_else(|| {
                            stream_error(
                                "tool call arguments arrived before the id".to_string(),
                            )
                        })?;
                        assembler.push_arguments(&call_id, fragment)?;
                    }
                }
            }
        }
        if let Some(reason) = choice["finish_reason"].as_str() {
            finish = map_finish_reason(reason);
        }
    }
    let tool_intents = assembler.finish()?;
    if !tool_intents.is_empty() && finish != FinishReason::ToolUse {
        finish = FinishReason::ToolUse;
    }
    sink(ProviderEvent::Usage(usage.clone()));
    Ok(StreamSummary {
        finish,
        tool_intents,
        usage,
    })
}

fn map_finish_reason(reason: &str) -> FinishReason {
    match reason {
        "tool_calls" => FinishReason::ToolUse,
        "length" => FinishReason::MaxOutput,
        "content_filter" => FinishReason::Refusal,
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

    const FIXTURE: &str = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n",
        "\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Checking \"}}]}\n",
        "\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"tests.\"}}]}\n",
        "\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"exec\",\"arguments\":\"\"}}]}}]}\n",
        "\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"argv\\\":[\\\"cargo\\\",\"}}]}}]}\n",
        "\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"test\\\"],\\\"cwd\\\":\\\".\\\",\\\"timeout_seconds\\\":60}\"}}]}}]}\n",
        "\n",
        "data: {\"choices\":[{\"index\":0,\"finish_reason\":\"tool_calls\"}]}\n",
        "\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":20}}\n",
        "\n",
        "data: [DONE]\n",
    );

    fn adapter(body: &str) -> XaiAdapter {
        let transport = Arc::new(FakeTransport::new(200, body.as_bytes()));
        XaiAdapter::new(transport, "test-key", "https://api.x.ai", 131_072, 4096)
    }

    fn request() -> ModelRequest {
        ModelRequest {
            model: "grok-configured".into(),
            system: vec!["be careful".into()],
            messages: vec![crate::ChatMessage::user_text("run the tests")],
            tools: vec![],
            max_output_tokens: 1024,
            deadline: std::time::Duration::from_secs(120),
        }
    }

    #[test]
    fn request_shape_matches_chat_completions() {
        let adapter = adapter("");
        let http = adapter.build_request(&request());
        assert_eq!(http.url, "https://api.x.ai/v1/chat/completions");
        assert!(http
            .headers
            .iter()
            .any(|(k, v)| k == "authorization" && v == "Bearer test-key"));
        let body: Value = serde_json::from_slice(&http.body).unwrap();
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["stream"], true);
    }

    #[test]
    fn tool_use_messages_map_to_tool_calls_and_results() {
        let adapter = adapter("");
        let mut request = request();
        request.messages.push(crate::ChatMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                call_id: "call_1".into(),
                name: "exec".into(),
                arguments: json!({"argv": ["cargo", "test"]}),
            }],
        });
        request.messages.push(crate::ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                call_id: "call_1".into(),
                content: "exit 0".into(),
                is_error: false,
            }],
        });
        let http = adapter.build_request(&request);
        let body: Value = serde_json::from_slice(&http.body).unwrap();
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[2]["tool_calls"][0]["id"], "call_1");
        assert_eq!(messages[2]["tool_calls"][0]["function"]["name"], "exec");
        assert_eq!(messages[3]["role"], "tool");
        assert_eq!(messages[3]["tool_call_id"], "call_1");
    }

    #[test]
    fn parses_fragmented_tool_call_arguments() {
        let adapter = adapter(FIXTURE);
        let cancel = CancellationToken::new();
        let mut deltas = String::new();
        let summary = adapter
            .stream(&request(), &cancel, &mut |event| {
                if let ProviderEvent::TextDelta(text) = event {
                    deltas.push_str(&text);
                }
            })
            .unwrap();
        assert_eq!(deltas, "Checking tests.");
        assert_eq!(summary.finish, FinishReason::ToolUse);
        assert_eq!(summary.usage.input_tokens, Some(100));
        assert_eq!(summary.tool_intents.len(), 1);
        assert_eq!(summary.tool_intents[0].name, "exec");
        assert_eq!(summary.tool_intents[0].arguments["argv"][1], "test");
    }

    #[test]
    fn multiple_tool_calls_are_separated_by_index() {
        let fixture = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"a.rs\\\"}\"}}]}}]}\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"c2\",\"function\":{\"name\":\"git_status\",\"arguments\":\"{}\"}}]}}]}\n",
            "data: [DONE]\n",
        );
        let adapter = adapter(fixture);
        let cancel = CancellationToken::new();
        let summary = adapter.stream(&request(), &cancel, &mut |_| {}).unwrap();
        assert_eq!(summary.tool_intents.len(), 2);
        assert_eq!(summary.tool_intents[0].name, "read_file");
        assert_eq!(summary.tool_intents[0].arguments["path"], "a.rs");
        assert_eq!(summary.tool_intents[1].name, "git_status");
    }

    #[test]
    fn cancellation_and_errors_are_typed() {
        let subject = adapter(FIXTURE);
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert_eq!(
            subject.stream(&request(), &cancel, &mut |_| {}).unwrap_err().code,
            ErrorCode::Cancelled
        );
        let empty = adapter("\n");
        let cancel = CancellationToken::new();
        let summary = empty.stream(&request(), &cancel, &mut |_| {}).unwrap();
        assert_eq!(summary.finish, FinishReason::EndTurn);
    }
}
