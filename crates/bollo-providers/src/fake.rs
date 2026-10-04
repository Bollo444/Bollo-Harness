//! Deterministic scripted provider used by the runtime and integration tests.
//!
//! This is a first-class test double, not a mock of the code under test: it
//! implements the same [`Provider`] port the adapters do, so the whole loop
//! (including tool dispatch) runs without network access.

use std::collections::VecDeque;
use std::sync::Mutex;

use bollo_protocol::cancel::CancellationToken;

use crate::{
    cancelled_error, FinishReason, ModelRequest, Provider, ProviderCapabilities, ProviderError,
    ProviderEvent, StreamSummary, ToolIntent, Usage,
};

#[derive(Debug, Clone)]
pub struct ScriptedResponse {
    pub deltas: Vec<String>,
    pub tool_intents: Vec<ToolIntent>,
    pub usage: Usage,
    pub finish: FinishReason,
    pub error: Option<ProviderError>,
}

impl ScriptedResponse {
    /// A plain text response.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            deltas: vec![text.into()],
            tool_intents: Vec::new(),
            usage: Usage {
                input_tokens: Some(100),
                output_tokens: Some(10),
            },
            finish: FinishReason::EndTurn,
            error: None,
        }
    }

    /// A response proposing tool calls.
    pub fn tools(intents: Vec<ToolIntent>) -> Self {
        Self {
            deltas: Vec::new(),
            tool_intents: intents,
            usage: Usage {
                input_tokens: Some(200),
                output_tokens: Some(20),
            },
            finish: FinishReason::ToolUse,
            error: None,
        }
    }

    /// A transport/provider failure.
    pub fn failure(error: ProviderError) -> Self {
        Self {
            deltas: Vec::new(),
            tool_intents: Vec::new(),
            usage: Usage::default(),
            finish: FinishReason::EndTurn,
            error: Some(error),
        }
    }
}

pub struct FakeProvider {
    provider_id: &'static str,
    capabilities: ProviderCapabilities,
    script: Mutex<VecDeque<ScriptedResponse>>,
    /// Every request this provider received, for tests that assert what the
    /// runtime actually sent (tool list, tool results).
    requests: Mutex<Vec<ModelRequest>>,
}

impl FakeProvider {
    pub fn anthropic(script: Vec<ScriptedResponse>) -> Self {
        Self::new("anthropic", script)
    }

    pub fn xai(script: Vec<ScriptedResponse>) -> Self {
        Self::new("xai", script)
    }

    pub fn new(provider_id: &'static str, script: Vec<ScriptedResponse>) -> Self {
        Self {
            provider_id,
            capabilities: ProviderCapabilities {
                streaming: true,
                tool_calls: true,
                max_context_tokens: 200_000,
                max_output_tokens: 4_096,
                usage_reported: true,
            },
            script: Mutex::new(script.into()),
            requests: Mutex::new(Vec::new()),
        }
    }

    pub fn remaining(&self) -> usize {
        self.script.lock().unwrap().len()
    }

    /// Requests received so far, in order.
    pub fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl Provider for FakeProvider {
    fn id(&self) -> &'static str {
        self.provider_id
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
        self.requests.lock().unwrap().push(request.clone());
        if cancel.is_cancelled() {
            return Err(cancelled_error());
        }
        let response = self
            .script
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| ProviderError::new(bollo_protocol::ErrorCode::ProviderError, "fake provider script exhausted"))?;
        if let Some(error) = response.error {
            return Err(error);
        }
        for delta in &response.deltas {
            if cancel.is_cancelled() {
                return Err(cancelled_error());
            }
            sink(ProviderEvent::TextDelta(delta.clone()));
        }
        sink(ProviderEvent::Usage(response.usage.clone()));
        Ok(StreamSummary {
            finish: response.finish,
            tool_intents: response.tool_intents,
            usage: response.usage,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request() -> ModelRequest {
        ModelRequest {
            model: "fake".into(),
            system: vec![],
            messages: vec![],
            tools: vec![],
            max_output_tokens: 100,
            deadline: std::time::Duration::from_secs(10),
        }
    }

    #[test]
    fn scripts_text_and_tools_in_order() {
        let provider = FakeProvider::anthropic(vec![
            ScriptedResponse::tools(vec![ToolIntent {
                call_id: "c1".into(),
                name: "read_file".into(),
                arguments: json!({"path": "main.rs"}),
            }]),
            ScriptedResponse::text("done"),
        ]);
        let cancel = CancellationToken::new();
        let mut deltas = Vec::new();
        let first;
        let second;
        {
            let mut sink = |event: ProviderEvent| {
                if let ProviderEvent::TextDelta(text) = event {
                    deltas.push(text);
                }
            };
            first = provider.stream(&request(), &cancel, &mut sink).unwrap();
            second = provider.stream(&request(), &cancel, &mut sink).unwrap();
        }
        assert_eq!(first.tool_intents.len(), 1);
        assert_eq!(second.finish, FinishReason::EndTurn);
        assert_eq!(deltas, vec!["done".to_string()]);
        assert_eq!(provider.remaining(), 0);
    }

    #[test]
    fn exhausted_script_and_cancellation_are_typed_errors() {
        let provider = FakeProvider::xai(vec![]);
        let cancel = CancellationToken::new();
        let err = provider.stream(&request(), &cancel, &mut |_| {}).unwrap_err();
        assert_eq!(err.code, bollo_protocol::ErrorCode::ProviderError);

        let provider = FakeProvider::anthropic(vec![ScriptedResponse::text("hi")]);
        let cancel = CancellationToken::new();
        cancel.cancel();
        let err = provider.stream(&request(), &cancel, &mut |_| {}).unwrap_err();
        assert_eq!(err.code, bollo_protocol::ErrorCode::Cancelled);
    }
}
