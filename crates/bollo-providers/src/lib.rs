//! Provider adapters behind one normalized port.
//!
//! The runtime never sees vendor-native events: adapters translate requests
//! and assemble streams into text deltas plus *complete, validated* tool
//! intents. No opaque cross-vendor forwarding and no automatic failover.

pub mod anthropic;
pub mod fake;
pub mod stream;
pub mod transport;
pub mod xai;

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use bollo_protocol::cancel::CancellationToken;
use bollo_protocol::errors::ErrorCode;

pub use stream::StreamAssembler;
pub use transport::{HttpRequest, HttpResponse, Transport, TransportError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderError {
    pub code: ErrorCode,
    pub message: String,
    pub status: Option<u16>,
    /// Whether a bounded retry before any effect is permitted.
    pub retryable: bool,
}

impl ProviderError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            status: None,
            retryable: false,
        }
    }

    pub fn http(status: u16, message: impl Into<String>) -> Self {
        let code = match status {
            401 | 403 => ErrorCode::CredentialMissing,
            429 => ErrorCode::ProviderError,
            _ => ErrorCode::ProviderError,
        };
        Self {
            code,
            message: message.into(),
            status: Some(status),
            retryable: matches!(status, 429 | 500 | 502 | 503 | 504),
        }
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ProviderError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        call_id: String,
        name: String,
        arguments: Value,
    },
    ToolResult {
        call_id: String,
        content: String,
        is_error: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl ChatMessage {
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    pub fn assistant_text(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCapabilities {
    pub streaming: bool,
    pub tool_calls: bool,
    pub max_context_tokens: u64,
    pub max_output_tokens: u64,
    pub usage_reported: bool,
}

#[derive(Debug, Clone)]
pub struct ModelRequest {
    pub model: String,
    pub system: Vec<String>,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolDefinition>,
    pub max_output_tokens: u64,
    pub deadline: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    EndTurn,
    ToolUse,
    MaxOutput,
    Refusal,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolIntent {
    pub call_id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StreamSummary {
    pub finish: FinishReason,
    pub tool_intents: Vec<ToolIntent>,
    pub usage: Usage,
}

/// Events the runtime reacts to while a response streams.
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderEvent {
    TextDelta(String),
    Usage(Usage),
}

pub trait Provider: Send + Sync {
    /// Stable provider id (`anthropic` or `xai`).
    fn id(&self) -> &'static str;

    fn capabilities(&self) -> &ProviderCapabilities;

    /// Stream one model response. Text arrives through `sink`; complete tool
    /// intents are returned only after their JSON validates.
    fn stream(
        &self,
        request: &ModelRequest,
        cancel: &CancellationToken,
        sink: &mut dyn FnMut(ProviderEvent),
    ) -> Result<StreamSummary, ProviderError>;
}

/// Shared helper: cancels produce a `Cancelled` error, never a fake success.
pub fn cancelled_error() -> ProviderError {
    ProviderError::new(ErrorCode::Cancelled, "request cancelled")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_errors_map_retryability() {
        assert!(ProviderError::http(429, "rate limited").retryable);
        assert!(ProviderError::http(503, "unavailable").retryable);
        assert!(!ProviderError::http(401, "bad key").retryable);
        assert_eq!(
            ProviderError::http(401, "bad key").code,
            ErrorCode::CredentialMissing
        );
    }

    #[test]
    fn cancellation_error_is_typed() {
        assert_eq!(cancelled_error().code, ErrorCode::Cancelled);
    }
}
