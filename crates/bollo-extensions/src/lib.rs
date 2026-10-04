//! Extensions: trusted command hooks and the MCP stdio client.
//!
//! Both extension kinds run in the workspace execution boundary with a
//! filtered environment (provider credentials are never inherited). "Enabled"
//! is not "trusted": trust binds executable identity, argv, cwd and requested
//! environment names, and project edits invalidate it.

pub mod hooks;
pub mod mcp;
/// Test fixture (a fake MCP server); not part of the product surface.
pub mod mcp_fake;
pub mod trust;

pub use hooks::{run_after_hook, run_before_hook, HookEvent, HookOutcome, HookPayload, HookSpec};
pub use mcp::{canonical_tool_id, McpCallResult, McpClient, McpServerSpec, McpTool};
pub use trust::{resolve_program, TrustRecord, TrustStore};

#[derive(Debug, thiserror::Error)]
pub enum ExtensionError {
    #[error("untrusted extension: {0}")]
    Untrusted(String),
    #[error("hook failed: {0}")]
    Hook(String),
    #[error("mcp protocol error: {0}")]
    Mcp(String),
    /// The server answered with a JSON-RPC error object. This is a definite
    /// negative outcome on a healthy transport, not a disconnect.
    #[error("mcp server error: {0}")]
    ServerError(String),
    #[error("mcp transport closed")]
    Disconnected,
    #[error("mcp timeout waiting for {0}")]
    Timeout(String),
    #[error("io: {0}")]
    Io(String),
    #[error("json: {0}")]
    Json(String),
}

impl From<std::io::Error> for ExtensionError {
    fn from(err: std::io::Error) -> Self {
        ExtensionError::Io(err.to_string())
    }
}

impl From<serde_json::Error> for ExtensionError {
    fn from(err: serde_json::Error) -> Self {
        ExtensionError::Json(err.to_string())
    }
}
