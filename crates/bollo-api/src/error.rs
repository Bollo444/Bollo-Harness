//! Documented HTTP error envelope.
//!
//! Codes and statuses follow `docs/reference/api.md`; the transport layer adds
//! the per-request `request_id`. Runtime tool errors stay inside run events
//! and never masquerade as transport failures.

use serde::Serialize;

/// One API failure before it is rendered with a request id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub status: u16,
    pub code: String,
    pub message: String,
    pub retryable: bool,
    /// Set for 429 responses (seconds until the rate window frees).
    pub retry_after: Option<u64>,
}

impl ApiError {
    fn new(status: u16, code: &str, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            status,
            code: code.to_string(),
            message: message.into(),
            retryable,
            retry_after: None,
        }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(400, "invalid_request", message, false)
    }

    pub fn invalid_cursor(message: impl Into<String>) -> Self {
        Self::new(400, "invalid_cursor", message, false)
    }

    pub fn unauthenticated(message: impl Into<String>) -> Self {
        Self::new(401, "unauthenticated", message, false)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(403, "forbidden", message, false)
    }

    pub fn origin_denied(message: impl Into<String>) -> Self {
        Self::new(403, "origin_denied", message, false)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(404, "not_found", message, false)
    }

    pub fn conflict(code: &str, message: impl Into<String>) -> Self {
        Self::new(409, code, message, false)
    }

    pub fn gone(code: &str, message: impl Into<String>) -> Self {
        Self::new(410, code, message, false)
    }

    pub fn rate_limited(retry_after: u64) -> Self {
        let mut error = Self::new(
            429,
            "rate_limited",
            "too many control requests for this token",
            true,
        );
        error.retry_after = Some(retry_after);
        error
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(500, "internal_error", message, false)
    }

    pub fn storage(message: impl Into<String>) -> Self {
        Self::new(500, "storage_unavailable", message, false)
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} ({}): {}",
            self.status, self.code, self.message
        )
    }
}

impl std::error::Error for ApiError {}

/// The OpenAPI `Error` object as serialized on the wire.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
    pub request_id: String,
    pub retryable: bool,
}

impl ErrorBody {
    pub fn render(error: &ApiError, request_id: &str) -> String {
        let body = Self {
            code: error.code.clone(),
            message: error.message.clone(),
            request_id: request_id.to_string(),
            retryable: error.retryable,
        };
        serde_json::to_string(&body).expect("error body is serializable")
    }
}

/// Generate the opaque per-request id used in error bodies and diagnostics.
pub fn request_id() -> String {
    format!("req_{}", uuid::Uuid::new_v4().simple())
}
