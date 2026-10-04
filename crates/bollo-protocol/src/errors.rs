//! Stable machine-readable error codes and the protocol error type.

use serde::{Deserialize, Serialize};

/// Error codes are part of the public contract (docs/reference/tools.md).
/// Never renumber; add new variants at the end and keep serialization stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    // Tool and filesystem
    InvalidToolArguments,
    UnknownTool,
    PathOutsideScope,
    SensitivePathDenied,
    PreimageConflict,
    AmbiguousMatch,
    ApprovalRequired,
    ApprovalStale,
    SandboxUnavailable,
    ToolTimeout,
    OutputTruncated,
    OperationUnknown,
    // Policy / modes
    PolicyDenied,
    ModeDenied,
    HookDenied,
    TrustRequired,
    // Provider / budget
    ProviderError,
    BudgetExceeded,
    RunLimitExceeded,
    PricingUnknown,
    Cancelled,
    // Config / environment
    ConfigInvalid,
    CredentialMissing,
    UnsupportedCapability,
    McpError,
    Internal,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::InvalidToolArguments => "invalid_tool_arguments",
            ErrorCode::UnknownTool => "unknown_tool",
            ErrorCode::PathOutsideScope => "path_outside_scope",
            ErrorCode::SensitivePathDenied => "sensitive_path_denied",
            ErrorCode::PreimageConflict => "preimage_conflict",
            ErrorCode::AmbiguousMatch => "ambiguous_match",
            ErrorCode::ApprovalRequired => "approval_required",
            ErrorCode::ApprovalStale => "approval_stale",
            ErrorCode::SandboxUnavailable => "sandbox_unavailable",
            ErrorCode::ToolTimeout => "tool_timeout",
            ErrorCode::OutputTruncated => "output_truncated",
            ErrorCode::OperationUnknown => "operation_unknown",
            ErrorCode::PolicyDenied => "policy_denied",
            ErrorCode::ModeDenied => "mode_denied",
            ErrorCode::HookDenied => "hook_denied",
            ErrorCode::TrustRequired => "trust_required",
            ErrorCode::ProviderError => "provider_error",
            ErrorCode::BudgetExceeded => "budget_exceeded",
            ErrorCode::RunLimitExceeded => "run_limit_exceeded",
            ErrorCode::PricingUnknown => "pricing_unknown",
            ErrorCode::Cancelled => "cancelled",
            ErrorCode::ConfigInvalid => "config_invalid",
            ErrorCode::CredentialMissing => "credential_missing",
            ErrorCode::UnsupportedCapability => "unsupported_capability",
            ErrorCode::McpError => "mcp_error",
            ErrorCode::Internal => "internal",
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A structured error. `message` is for humans; `code` is the contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BolloError {
    pub code: ErrorCode,
    pub message: String,
}

impl BolloError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for BolloError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for BolloError {}

/// Protocol-level failures: malformed ids, envelopes and NDJSON lines.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("invalid opaque id: {0:?}")]
    InvalidId(String),
    #[error("invalid event envelope: {0}")]
    InvalidEvent(String),
    #[error("invalid ndjson line: {0}")]
    InvalidLine(String),
    #[error("invalid command: {0}")]
    InvalidCommand(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_stable_snake_case() {
        assert_eq!(ErrorCode::InvalidToolArguments.as_str(), "invalid_tool_arguments");
        assert_eq!(ErrorCode::ApprovalRequired.as_str(), "approval_required");
        let json = serde_json::to_string(&ErrorCode::PathOutsideScope).unwrap();
        assert_eq!(json, "\"path_outside_scope\"");
    }

    #[test]
    fn error_roundtrips() {
        let err = BolloError::new(ErrorCode::ModeDenied, "plan mode cannot write");
        let json = serde_json::to_string(&err).unwrap();
        let back: BolloError = serde_json::from_str(&json).unwrap();
        assert_eq!(err, back);
        assert!(err.to_string().contains("mode_denied"));
    }

}
