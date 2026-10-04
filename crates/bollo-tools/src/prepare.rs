//! Prepare: parse, bound-check and normalize a proposal *before* any policy
//! decision. Nothing here has authority; it only produces the exact
//! [`PreparedTool`] that policy will judge and that the gate will bind.

use std::path::PathBuf;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Digest;

use bollo_policy::normalize::{effect_class_for, scoped_path, NormalizedIntent, ScopedPath};
use bollo_protocol::vocab::ToolClass;
use bollo_workspace::root::policy_string;
use bollo_workspace::WorkspaceFs;

use crate::{descriptor, McpToolSpec, ToolError};

pub const MAX_WRITE_BYTES: usize = 1_048_576;
pub const MAX_ARGV_ITEMS: usize = 128;
pub const MAX_ARGV_BYTES: usize = 32 * 1024;
pub const MAX_MCP_ARGUMENT_BYTES: usize = 1_048_576;

/// A validated proposal: the exact action plus the normalized intent the gate
/// hashes and the approval binds.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedTool {
    pub tool_name: String,
    pub class: ToolClass,
    pub intent: NormalizedIntent,
    pub action: PreparedAction,
    /// Bound at approval time when the target can change (write/patch).
    pub target_preimage: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreparedAction {
    ReadFile {
        path: String,
        offset: Option<u64>,
        limit: Option<u64>,
    },
    Search {
        pattern: String,
        path: String,
        max_results: usize,
    },
    WriteFile {
        path: String,
        content: String,
        expected_sha256: Option<String>,
    },
    ApplyPatch {
        path: String,
        old_text: String,
        new_text: String,
        expected_sha256: String,
    },
    Exec {
        argv: Vec<String>,
        cwd: String,
        timeout: Duration,
    },
    GitStatus,
    GitDiff {
        path: Option<String>,
    },
    /// A discovered MCP tool call. Arguments stay opaque to Bollo: the server's
    /// schema is authoritative and is never re-read as a path, argv or grant.
    Mcp {
        canonical_id: String,
        server_id: String,
        tool_name: String,
        arguments: Value,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadFileArgs {
    path: String,
    #[serde(default)]
    offset: Option<u64>,
    #[serde(default)]
    limit: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    max_results: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteFileArgs {
    path: String,
    content: String,
    expected_sha256: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyPatchArgs {
    path: String,
    old_text: String,
    new_text: String,
    expected_sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecArgs {
    argv: Vec<String>,
    cwd: String,
    timeout_seconds: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GitStatusArgs {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GitDiffArgs {
    #[serde(default)]
    path: Option<String>,
}

pub fn prepare(
    tool_name: &str,
    args: &Value,
    fs: &WorkspaceFs,
    policy_root: &str,
) -> Result<PreparedTool, ToolError> {
    prepare_with_catalog(tool_name, args, fs, policy_root, &[])
}

/// Prepare any model-proposed call: a discovered MCP tool when `catalog`
/// contains it, otherwise a built-in. Name resolution happens before policy,
/// so the journal, approvals and UI always bind the canonical id
/// (`mcp:<server>:<tool>`), never the provider-safe spelling.
pub fn prepare_with_catalog(
    tool_name: &str,
    args: &Value,
    fs: &WorkspaceFs,
    policy_root: &str,
    catalog: &[McpToolSpec],
) -> Result<PreparedTool, ToolError> {
    if let Some(spec) = catalog.iter().find(|spec| spec.matches(tool_name)) {
        return prepare_mcp(spec, args);
    }
    prepare_builtin(tool_name, args, fs, policy_root)
}

fn prepare_mcp(spec: &McpToolSpec, args: &Value) -> Result<PreparedTool, ToolError> {
    if !args.is_object() {
        return Err(ToolError::invalid("mcp arguments must be a JSON object"));
    }
    let encoded = serde_json::to_vec(args)
        .map_err(|err| ToolError::invalid(format!("invalid arguments: {err}")))?;
    if encoded.len() > MAX_MCP_ARGUMENT_BYTES {
        return Err(ToolError::invalid(format!(
            "mcp arguments exceed {MAX_MCP_ARGUMENT_BYTES} bytes"
        )));
    }
    let action = PreparedAction::Mcp {
        canonical_id: spec.canonical_id.clone(),
        server_id: spec.server_id.clone(),
        tool_name: spec.tool_name.clone(),
        arguments: args.clone(),
    };
    let intent = build_intent(
        &spec.canonical_id,
        ToolClass::Mcp,
        Vec::new(),
        None,
        None,
        format!("mcp {} / {}", spec.server_id, spec.tool_name),
        digest_of(&action),
    );
    Ok(PreparedTool {
        tool_name: spec.canonical_id.clone(),
        class: ToolClass::Mcp,
        intent,
        action,
        target_preimage: None,
    })
}

fn prepare_builtin(
    tool_name: &str,
    args: &Value,
    fs: &WorkspaceFs,
    policy_root: &str,
) -> Result<PreparedTool, ToolError> {
    let descriptor = descriptor(tool_name)
        .ok_or_else(|| ToolError::new(bollo_protocol::errors::ErrorCode::UnknownTool, tool_name))?;
    match tool_name {
        "read_file" => {
            require_fields(args, &["path"])?;
            let args: ReadFileArgs = parse(args)?;
            if args.offset == Some(0) {
                return Err(ToolError::invalid("offset must be >= 1"));
            }
            if let Some(limit) = args.limit {
                if !(1..=2_000).contains(&limit) {
                    return Err(ToolError::invalid("limit must be 1..=2000"));
                }
            }
            let path = args.path.clone();
            let scoped = scope(fs, policy_root, &path)?;
            let summary = format!("read {path}");
            let action = PreparedAction::ReadFile {
                path,
                offset: args.offset,
                limit: args.limit,
            };
            let intent = build_intent(
                tool_name,
                descriptor.class,
                vec![scoped],
                None,
                None,
                summary,
                digest_of(&action),
            );
            Ok(PreparedTool {
                tool_name: tool_name.into(),
                class: descriptor.class,
                intent,
                action,
                target_preimage: None,
            })
        }
        "search" => {
            require_fields(args, &["pattern"])?;
            let args: SearchArgs = parse(args)?;
            if args.pattern.is_empty() || args.pattern.len() > 4_096 {
                return Err(ToolError::invalid("pattern must be 1..=4096 bytes"));
            }
            let max_results = args.max_results.unwrap_or(100);
            if !(1..=1_000).contains(&max_results) {
                return Err(ToolError::invalid("max_results must be 1..=1000"));
            }
            let path = args.path.clone().unwrap_or_else(|| ".".into());
            let scoped = scope(fs, policy_root, &path)?;
            let summary = format!("search {:?} in {}", args.pattern, path);
            let action = PreparedAction::Search {
                pattern: args.pattern,
                path,
                max_results,
            };
            let intent = build_intent(
                tool_name,
                descriptor.class,
                vec![scoped],
                None,
                None,
                summary,
                digest_of(&action),
            );
            Ok(PreparedTool {
                tool_name: tool_name.into(),
                class: descriptor.class,
                intent,
                action,
                target_preimage: None,
            })
        }
        "write_file" => {
            require_fields(args, &["path", "content", "expected_sha256"])?;
            let args: WriteFileArgs = parse(args)?;
            if args.content.len() > MAX_WRITE_BYTES {
                return Err(ToolError::invalid(format!(
                    "content exceeds {MAX_WRITE_BYTES} bytes"
                )));
            }
            if let Some(expected) = &args.expected_sha256 {
                validate_sha256(expected)?;
            }
            let path = args.path.clone();
            let scoped = scope(fs, policy_root, &path)?;
            let target_preimage = fs.current_sha256(&path);
            let summary = format!("write {path} ({} bytes)", args.content.len());
            let action = PreparedAction::WriteFile {
                path,
                content: args.content,
                expected_sha256: args.expected_sha256,
            };
            let intent = build_intent(
                tool_name,
                descriptor.class,
                vec![scoped],
                None,
                None,
                summary,
                digest_of(&action),
            );
            Ok(PreparedTool {
                tool_name: tool_name.into(),
                class: descriptor.class,
                intent,
                action,
                target_preimage,
            })
        }
        "apply_patch" => {
            require_fields(args, &["path", "old_text", "new_text", "expected_sha256"])?;
            let args: ApplyPatchArgs = parse(args)?;
            if args.old_text.is_empty() || args.old_text.len() > MAX_WRITE_BYTES {
                return Err(ToolError::invalid("old_text must be 1..=1 MiB"));
            }
            if args.new_text.len() > MAX_WRITE_BYTES {
                return Err(ToolError::invalid("new_text must be <= 1 MiB"));
            }
            validate_sha256(&args.expected_sha256)?;
            let path = args.path.clone();
            let scoped = scope(fs, policy_root, &path)?;
            let target_preimage = fs.current_sha256(&path);
            let summary = format!("patch {path}");
            let action = PreparedAction::ApplyPatch {
                path,
                old_text: args.old_text,
                new_text: args.new_text,
                expected_sha256: args.expected_sha256,
            };
            let intent = build_intent(
                tool_name,
                descriptor.class,
                vec![scoped],
                None,
                None,
                summary,
                digest_of(&action),
            );
            Ok(PreparedTool {
                tool_name: tool_name.into(),
                class: descriptor.class,
                intent,
                action,
                target_preimage,
            })
        }
        "exec" => {
            require_fields(args, &["argv", "cwd", "timeout_seconds"])?;
            let args: ExecArgs = parse(args)?;
            if args.argv.is_empty() || args.argv.len() > MAX_ARGV_ITEMS {
                return Err(ToolError::invalid(format!(
                    "argv must have 1..={MAX_ARGV_ITEMS} items"
                )));
            }
            if args.argv[0].trim().is_empty() {
                return Err(ToolError::invalid("argv[0] must not be empty"));
            }
            let argv_bytes: usize = args.argv.iter().map(|arg| arg.len() + 1).sum();
            if argv_bytes > MAX_ARGV_BYTES {
                return Err(ToolError::invalid(format!(
                    "argv exceeds {MAX_ARGV_BYTES} bytes"
                )));
            }
            if !(1..=600).contains(&args.timeout_seconds) {
                return Err(ToolError::invalid("timeout_seconds must be 1..=600"));
            }
            if args.cwd.trim().is_empty() {
                return Err(ToolError::invalid("cwd must not be empty"));
            }
            let cwd_scoped = scope(fs, policy_root, &args.cwd)?;
            let summary = format!(
                "exec {}",
                args.argv.first().cloned().unwrap_or_default()
            );
            let action = PreparedAction::Exec {
                argv: args.argv,
                cwd: args.cwd,
                timeout: Duration::from_secs(args.timeout_seconds),
            };
            let intent = build_intent(
                tool_name,
                descriptor.class,
                Vec::new(),
                match &action {
                    PreparedAction::Exec { argv, .. } => Some(argv.clone()),
                    _ => unreachable!(),
                },
                Some(cwd_scoped),
                summary,
                digest_of(&action),
            );
            Ok(PreparedTool {
                tool_name: tool_name.into(),
                class: descriptor.class,
                intent,
                action,
                target_preimage: None,
            })
        }
        "git_status" => {
            require_fields(args, &[])?;
            let _args: GitStatusArgs = parse(args)?;
            let scoped = scoped_root(fs, policy_root)?;
            let action = PreparedAction::GitStatus;
            let intent = build_intent(
                tool_name,
                descriptor.class,
                vec![scoped],
                None,
                None,
                "git status".to_string(),
                digest_of(&action),
            );
            Ok(PreparedTool {
                tool_name: tool_name.into(),
                class: descriptor.class,
                intent,
                action,
                target_preimage: None,
            })
        }
        "git_diff" => {
            let args: GitDiffArgs = parse(args)?;
            let mut paths = Vec::new();
            if let Some(path) = &args.path {
                if path.trim().is_empty() {
                    return Err(ToolError::invalid("path must not be empty"));
                }
                paths.push(scope(fs, policy_root, path)?);
            } else {
                paths.push(scoped_root(fs, policy_root)?);
            }
            let summary = match &args.path {
                Some(path) => format!("git diff {path}"),
                None => "git diff".to_string(),
            };
            let action = PreparedAction::GitDiff { path: args.path };
            let intent = build_intent(
                tool_name,
                descriptor.class,
                paths,
                None,
                None,
                summary,
                digest_of(&action),
            );
            Ok(PreparedTool {
                tool_name: tool_name.into(),
                class: descriptor.class,
                intent,
                action,
                target_preimage: None,
            })
        }
        other => Err(ToolError::new(
            bollo_protocol::errors::ErrorCode::UnknownTool,
            other,
        )),
    }
}

fn parse<T: DeserializeOwned>(args: &Value) -> Result<T, ToolError> {
    serde_json::from_value(args.clone())
        .map_err(|err| ToolError::invalid(format!("invalid arguments: {err}")))
}

fn validate_sha256(value: &str) -> Result<(), ToolError> {
    let valid = value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if valid {
        Ok(())
    } else {
        Err(ToolError::invalid(
            "expected_sha256 must be 64 lowercase hex characters",
        ))
    }
}

fn scope(fs: &WorkspaceFs, policy_root: &str, raw: &str) -> Result<ScopedPath, ToolError> {
    let real = fs.resolve_relaxed(raw).map_err(ToolError::from)?;
    scoped_path(policy_root, &policy_string(&real)).map_err(ToolError::from)
}

fn scoped_root(fs: &WorkspaceFs, policy_root: &str) -> Result<ScopedPath, ToolError> {
    let real = fs
        .resolve_relaxed(".")
        .map_err(ToolError::from)?;
    scoped_path(policy_root, &policy_string(&real)).map_err(ToolError::from)
}

fn build_intent(
    tool: &str,
    class: ToolClass,
    paths: Vec<ScopedPath>,
    argv: Option<Vec<String>>,
    cwd: Option<ScopedPath>,
    summary: String,
    arguments_digest: String,
) -> NormalizedIntent {
    let any_outside = paths.iter().any(|p| p.is_outside())
        || cwd.as_ref().map(|c| c.is_outside()).unwrap_or(false);
    NormalizedIntent {
        tool: tool.to_string(),
        class,
        effect: effect_class_for(class, any_outside),
        paths,
        argv,
        cwd,
        environment_names: Vec::new(),
        summary,
        arguments_digest,
    }
}

/// Canonical digest of the exact prepared action: approvals bind these bytes,
/// so any changed argument (including content, limits or timeouts) invalidates
/// the receipt.
fn digest_of(action: &PreparedAction) -> String {
    let bytes = serde_json::to_vec(action).expect("PreparedAction is serializable");
    hex::encode(sha2::Sha256::digest(bytes))
}

/// Enforce required-field presence, including required-nullable fields that
/// serde would otherwise treat as absent → None.
fn require_fields(args: &Value, fields: &[&str]) -> Result<(), ToolError> {
    let object = args
        .as_object()
        .ok_or_else(|| ToolError::invalid("arguments must be a JSON object"))?;
    for field in fields {
        if !object.contains_key(*field) {
            return Err(ToolError::invalid(format!(
                "missing required field {field:?}"
            )));
        }
    }
    Ok(())
}

/// Path used at execution time; kept for callers that need the raw handle.
pub fn resolved_execution_path(fs: &WorkspaceFs, raw: &str) -> Result<PathBuf, ToolError> {
    fs.resolve(raw).map_err(ToolError::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollo_workspace::root::WorkspaceRoot;
    use serde_json::json;

    fn fixture() -> (tempfile::TempDir, WorkspaceFs) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        let root = WorkspaceRoot::discover(dir.path()).unwrap();
        (dir, WorkspaceFs::new(&root))
    }

    #[test]
    fn read_file_prepares_workspace_scoped_intent() {
        let (_dir, fs) = fixture();
        let prepared = prepare(
            "read_file",
            &json!({"path": "main.rs"}),
            &fs,
            &fs.policy_root(),
        )
        .unwrap();
        assert_eq!(prepared.class, ToolClass::Read);
        assert!(!prepared.intent.paths[0].is_outside());
        assert_eq!(prepared.intent.paths[0].display(), "main.rs");
    }

    #[test]
    fn malformed_arguments_are_rejected_pre_policy() {
        let (_dir, fs) = fixture();
        let root = fs.policy_root();
        assert!(prepare("read_file", &json!({}), &fs, &root).is_err());
        assert!(prepare("read_file", &json!({"path": "a", "limit": 2001}), &fs, &root).is_err());
        assert!(prepare("read_file", &json!({"path": "a", "extra": 1}), &fs, &root).is_err());
        assert!(prepare("exec", &json!({"argv": [], "cwd": ".", "timeout_seconds": 5}), &fs, &root).is_err());
        assert!(prepare(
            "write_file",
            &json!({"path": "a", "content": "x"}),
            &fs,
            &root
        )
        .is_err());
        assert!(prepare("nope", &json!({}), &fs, &root).is_err());
    }

    #[test]
    fn write_binds_target_preimage() {
        let (_dir, fs) = fixture();
        let prepared = prepare(
            "write_file",
            &json!({"path": "main.rs", "content": "new", "expected_sha256": null}),
            &fs,
            &fs.policy_root(),
        )
        .unwrap();
        assert_eq!(prepared.target_preimage, fs.current_sha256("main.rs"));
    }

    #[test]
    fn exec_normalizes_argv_and_cwd() {
        let (_dir, fs) = fixture();
        let prepared = prepare(
            "exec",
            &json!({"argv": ["cargo", "test"], "cwd": ".", "timeout_seconds": 60}),
            &fs,
            &fs.policy_root(),
        )
        .unwrap();
        assert_eq!(prepared.intent.argv.as_deref(), Some(&["cargo".to_string(), "test".to_string()][..]));
        assert!(!prepared.intent.cwd.as_ref().unwrap().is_outside());
    }

    fn mcp_spec() -> McpToolSpec {
        McpToolSpec {
            canonical_id: "mcp:fake:echo".into(),
            provider_name: crate::provider_safe_tool_name("mcp:fake:echo"),
            server_id: "fake".into(),
            tool_name: "echo".into(),
            description: "echo text".into(),
            input_schema: json!({"type": "object"}),
        }
    }

    #[test]
    fn mcp_tools_prepare_external_intents_for_both_spellings() {
        let (_dir, fs) = fixture();
        let catalog = [mcp_spec()];
        for proposed in ["mcp_fake_echo", "mcp:fake:echo"] {
            let prepared = prepare_with_catalog(
                proposed,
                &json!({"text": "hi"}),
                &fs,
                &fs.policy_root(),
                &catalog,
            )
            .unwrap();
            assert_eq!(prepared.tool_name, "mcp:fake:echo");
            assert_eq!(prepared.intent.tool, "mcp:fake:echo");
            assert_eq!(prepared.class, ToolClass::Mcp);
            assert_eq!(
                prepared.intent.effect,
                bollo_protocol::vocab::EffectClass::ExternalMutation
            );
            assert!(prepared.intent.paths.is_empty());
            assert!(prepared.intent.argv.is_none());
        }
    }

    #[test]
    fn mcp_arguments_are_bounded_and_digest_bound() {
        let (_dir, fs) = fixture();
        let catalog = [mcp_spec()];
        assert!(prepare_with_catalog(
            "mcp:fake:echo",
            &json!("not-an-object"),
            &fs,
            &fs.policy_root(),
            &catalog
        )
        .is_err());
        let first = prepare_with_catalog(
            "mcp:fake:echo",
            &json!({"text": "a"}),
            &fs,
            &fs.policy_root(),
            &catalog,
        )
        .unwrap();
        let second = prepare_with_catalog(
            "mcp:fake:echo",
            &json!({"text": "b"}),
            &fs,
            &fs.policy_root(),
            &catalog,
        )
        .unwrap();
        assert_ne!(first.intent.arguments_digest, second.intent.arguments_digest);
    }

    #[test]
    fn builtins_are_untouched_by_an_mcp_catalog() {
        let (_dir, fs) = fixture();
        let catalog = [mcp_spec()];
        let prepared = prepare_with_catalog(
            "read_file",
            &json!({"path": "main.rs"}),
            &fs,
            &fs.policy_root(),
            &catalog,
        )
        .unwrap();
        assert_eq!(prepared.tool_name, "read_file");
        assert_eq!(prepared.class, ToolClass::Read);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_target_is_normalized_as_outside() {
        let (dir, fs) = fixture();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), dir.path().join("link")).unwrap();
        let prepared = prepare("read_file", &json!({"path": "link"}), &fs, &fs.policy_root()).unwrap();
        assert!(prepared.intent.paths[0].is_outside());
    }
}
