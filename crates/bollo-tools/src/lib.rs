//! One schema-backed registry of built-in tools. Every proposal enters the
//! same broker: prepare (normalize + validate) → policy gate → execute with a
//! host-issued [`bollo_policy::Authorization`].
//!
//! Tool names are not permission grants; nothing here decides whether a call
//! is allowed.

pub mod execute;
pub mod prepare;
pub mod search;

use serde::Serialize;
use serde_json::{json, Value};
use sha2::Digest;

use bollo_protocol::errors::ErrorCode;
use bollo_protocol::vocab::ToolClass;
use bollo_workspace::WsError;

pub use execute::{
    execute, execute_with_mcp, ExecuteContext, McpDispatch, McpDispatchOutcome, ToolOutcome,
};
pub use prepare::{prepare, prepare_with_catalog, PreparedAction, PreparedTool};

/// A tool's canonical, versioned contract.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDescriptor {
    pub name: &'static str,
    pub class: ToolClass,
    pub description: &'static str,
    /// JSON Schema for the provider tool definition (canonical shape).
    pub input_schema: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError {
    pub code: ErrorCode,
    pub message: String,
}

impl ToolError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidToolArguments, message)
    }
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ToolError {}

impl From<bollo_policy::PolicyError> for ToolError {
    fn from(err: bollo_policy::PolicyError) -> Self {
        let code = match err {
            bollo_policy::PolicyError::Path(_) => ErrorCode::InvalidToolArguments,
            _ => ErrorCode::Internal,
        };
        ToolError::new(code, err.to_string())
    }
}

impl From<WsError> for ToolError {
    fn from(err: WsError) -> Self {
        let code = match err {
            WsError::OutsideScope(_) => ErrorCode::PathOutsideScope,
            WsError::InvalidPath(_) => ErrorCode::InvalidToolArguments,
            WsError::SpecialFile(_) => ErrorCode::SensitivePathDenied,
            WsError::PreimageConflict(_) => ErrorCode::PreimageConflict,
            WsError::AmbiguousMatch(_) => ErrorCode::AmbiguousMatch,
            WsError::BinaryContent => ErrorCode::SensitivePathDenied,
            WsError::TooLarge(_) => ErrorCode::InvalidToolArguments,
            WsError::CheckpointNotFound(_) => ErrorCode::OperationUnknown,
            WsError::Conflict(_) => ErrorCode::PreimageConflict,
            WsError::Io(_) => ErrorCode::Internal,
        };
        ToolError::new(code, err.to_string())
    }
}

/// The seven built-ins, in canonical order.
pub const BUILTIN_TOOLS: [&str; 7] = [
    "read_file",
    "search",
    "write_file",
    "apply_patch",
    "exec",
    "git_status",
    "git_diff",
];

pub fn tool_names() -> &'static [&'static str] {
    &BUILTIN_TOOLS
}

pub fn descriptor(name: &str) -> Option<ToolDescriptor> {
    let (class, description, input_schema) = match name {
        "read_file" => (
            ToolClass::Read,
            "Read a text file from the workspace with optional line slicing.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "minLength": 1 },
                    "offset": { "type": "integer", "minimum": 1 },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 2000 }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        ),
        "search" => (
            ToolClass::Search,
            "Search workspace text files with a bounded regular expression.",
            json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "minLength": 1, "maxLength": 4096 },
                    "path": { "type": "string", "minLength": 1 },
                    "max_results": { "type": "integer", "minimum": 1, "maximum": 1000 }
                },
                "required": ["pattern"],
                "additionalProperties": false
            }),
        ),
        "write_file" => (
            ToolClass::Write,
            "Create or atomically replace a workspace text file, guarded by expected_sha256.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "minLength": 1 },
                    "content": { "type": "string", "maxLength": 1048576 },
                    "expected_sha256": {
                        "anyOf": [
                            { "type": "string", "pattern": "^[a-f0-9]{64}$" },
                            { "type": "null" }
                        ]
                    }
                },
                "required": ["path", "content", "expected_sha256"],
                "additionalProperties": false
            }),
        ),
        "apply_patch" => (
            ToolClass::Write,
            "Replace a unique exact fragment in a workspace text file, guarded by expected_sha256.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "minLength": 1 },
                    "old_text": { "type": "string", "minLength": 1, "maxLength": 1048576 },
                    "new_text": { "type": "string", "maxLength": 1048576 },
                    "expected_sha256": { "type": "string", "pattern": "^[a-f0-9]{64}$" }
                },
                "required": ["path", "old_text", "new_text", "expected_sha256"],
                "additionalProperties": false
            }),
        ),
        "exec" => (
            ToolClass::Exec,
            "Run a program with argv (no implicit shell parsing) inside the workspace.",
            json!({
                "type": "object",
                "properties": {
                    "argv": {
                        "type": "array",
                        "items": { "type": "string" },
                        "minItems": 1,
                        "maxItems": 128
                    },
                    "cwd": { "type": "string", "minLength": 1 },
                    "timeout_seconds": { "type": "integer", "minimum": 1, "maximum": 600 }
                },
                "required": ["argv", "cwd", "timeout_seconds"],
                "additionalProperties": false
            }),
        ),
        "git_status" => (
            ToolClass::Git,
            "Show git status for the workspace with unsafe helpers disabled.",
            json!({
                "type": "object",
                "properties": {},
                "required": [],
                "additionalProperties": false
            }),
        ),
        "git_diff" => (
            ToolClass::Git,
            "Show a bounded git diff for the workspace or one path.",
            json!({
                "type": "object",
                "properties": { "path": { "type": "string", "minLength": 1 } },
                "required": [],
                "additionalProperties": false
            }),
        ),
        _ => return None,
    };
    Some(ToolDescriptor {
        name: match name {
            "read_file" => "read_file",
            "search" => "search",
            "write_file" => "write_file",
            "apply_patch" => "apply_patch",
            "exec" => "exec",
            "git_status" => "git_status",
            "git_diff" => "git_diff",
            _ => unreachable!(),
        },
        class,
        description,
        input_schema,
    })
}

/// All provider-facing built-in tool definitions (canonical contract for
/// adapters). MCP tools are added by [`tool_specs`].
pub fn provider_tool_definitions() -> Vec<ToolDescriptor> {
    BUILTIN_TOOLS
        .iter()
        .filter_map(|name| descriptor(name))
        .collect()
}

/// A tool discovered from a connected MCP server.
///
/// Two names exist on purpose: `canonical_id` (`mcp:<server>:<tool>`) is what
/// policy, the journal, approvals and the UI use, while `provider_name` is the
/// id-safe spelling sent to model providers (vendor tool-name patterns allow
/// `[A-Za-z0-9_-]` only). Policy rules therefore name the canonical id exactly,
/// as `docs/security/permissions.md` requires for non-`*` rules.
#[derive(Debug, Clone, PartialEq)]
pub struct McpToolSpec {
    pub canonical_id: String,
    pub provider_name: String,
    pub server_id: String,
    pub tool_name: String,
    pub description: String,
    pub input_schema: Value,
}

impl McpToolSpec {
    /// True when a model-proposed tool name refers to this MCP tool, either as
    /// the canonical id or as the id-safe provider spelling.
    pub fn matches(&self, proposed: &str) -> bool {
        proposed == self.canonical_id || proposed == self.provider_name
    }
}

/// One entry of the model-facing tool list. `tool_name` is the identity used
/// for policy and the journal; `provider_name` is what the model sees.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub provider_name: String,
    pub tool_name: String,
    pub description: String,
    pub input_schema: Value,
    pub class: ToolClass,
}

/// Derive the id-safe provider spelling of a canonical tool identity.
/// `:` becomes `_`; overlong names keep a stable digest suffix.
pub fn provider_safe_tool_name(canonical: &str) -> String {
    let mut safe: String = canonical
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect();
    const MAX: usize = 64;
    if safe.len() > MAX {
        let digest = hex::encode(sha2::Sha256::digest(canonical.as_bytes()));
        safe.truncate(MAX - 9);
        safe.push('_');
        safe.push_str(&digest[..8]);
    }
    safe
}

/// The model-facing tool list: built-ins plus discovered MCP tools. A provider
/// name can never shadow a built-in, because MCP names always start with `mcp_`
/// and duplicates are dropped rather than silently renamed.
#[derive(Debug, Clone, Default)]
pub struct ToolCatalog {
    pub specs: Vec<ToolSpec>,
    pub dropped: Vec<String>,
}

pub fn tool_specs(mcp: &[McpToolSpec]) -> ToolCatalog {
    let mut specs = Vec::new();
    let mut dropped = Vec::new();
    let mut taken = std::collections::BTreeSet::new();
    for descriptor in provider_tool_definitions() {
        taken.insert(descriptor.name.to_string());
        specs.push(ToolSpec {
            provider_name: descriptor.name.to_string(),
            tool_name: descriptor.name.to_string(),
            description: descriptor.description.to_string(),
            input_schema: descriptor.input_schema,
            class: descriptor.class,
        });
    }
    for spec in mcp {
        if !taken.insert(spec.provider_name.clone()) {
            dropped.push(spec.canonical_id.clone());
            continue;
        }
        specs.push(ToolSpec {
            provider_name: spec.provider_name.clone(),
            tool_name: spec.canonical_id.clone(),
            description: spec.description.clone(),
            input_schema: spec.input_schema.clone(),
            class: ToolClass::Mcp,
        });
    }
    ToolCatalog { specs, dropped }
}

/// Result payloads are typed; models receive `data`, never raw internals.
#[derive(Debug, Clone, Serialize)]
pub struct ReadFileData {
    pub text: String,
    pub start_line: u64,
    pub end_line: u64,
    pub total_lines: u64,
    pub sha256: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchMatch {
    pub path: String,
    pub line: u64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchData {
    pub matches: Vec<SearchMatch>,
    pub count: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct WriteFileData {
    pub pre_sha256: Option<String>,
    pub post_sha256: String,
    pub bytes: u64,
    pub checkpoint_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplyPatchData {
    pub pre_sha256: String,
    pub post_sha256: String,
    pub replaced_bytes: u64,
    pub checkpoint_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecData {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    pub duration_ms: u64,
    pub reaped: bool,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GitData {
    pub stdout: String,
    pub truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_seven_tools_have_descriptors_and_schemas() {
        assert_eq!(tool_names().len(), 7);
        for name in tool_names() {
            let descriptor = descriptor(name).unwrap_or_else(|| panic!("{name} missing"));
            assert!(!descriptor.description.is_empty());
            assert_eq!(descriptor.input_schema["type"], "object");
        }
    }

    #[test]
    fn provider_definitions_cover_the_catalog() {
        let definitions = provider_tool_definitions();
        assert_eq!(definitions.len(), 7);
        let names: Vec<&str> = definitions.iter().map(|d| d.name).collect();
        assert_eq!(names, tool_names());
    }

    #[test]
    fn workspace_errors_map_to_stable_codes() {
        let err: ToolError = WsError::OutsideScope("x".into()).into();
        assert_eq!(err.code, ErrorCode::PathOutsideScope);
        let err: ToolError = WsError::PreimageConflict("x".into()).into();
        assert_eq!(err.code, ErrorCode::PreimageConflict);
        let err: ToolError = WsError::AmbiguousMatch("x".into()).into();
        assert_eq!(err.code, ErrorCode::AmbiguousMatch);
    }
}
