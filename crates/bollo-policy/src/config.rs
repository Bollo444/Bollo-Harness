//! Strict configuration parsing and semantic validation.
//!
//! Canonical schema: `docs/contracts/config.schema.json`; semantics:
//! `docs/reference/configuration.md`. Unknown keys, duplicate rule ids,
//! negated globs, non-HTTPS remote origins, embedded credentials, telemetry
//! enabled and out-of-range limits are all rejected with source-aware errors.

use serde::{Deserialize, Serialize};

use bollo_protocol::vocab::{Effect, Profile, SandboxMode};
use bollo_protocol::ids::is_valid_opaque_id;

use crate::PolicyError;

pub const CONFIG_SCHEMA_VERSION: &str = "0.1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    Anthropic,
    Xai,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    pub base_url: String,
    pub model: String,
    pub credential_env: String,
    pub context_tokens: u64,
    #[serde(default)]
    pub input_microusd_per_token: Option<u64>,
    #[serde(default)]
    pub output_microusd_per_token: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleConfig {
    pub id: String,
    pub effect: Effect,
    pub tool: String,
    #[serde(default)]
    pub path_glob: Option<String>,
    #[serde(default)]
    pub argv_prefix: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionsConfig {
    #[serde(default = "default_profile")]
    pub profile: Profile,
    #[serde(default = "default_sandbox")]
    pub sandbox: SandboxMode,
    #[serde(default)]
    pub rules: Vec<RuleConfig>,
}

fn default_profile() -> Profile {
    Profile::Balanced
}

fn default_sandbox() -> SandboxMode {
    SandboxMode::Workspace
}

impl Default for PermissionsConfig {
    fn default() -> Self {
        Self {
            profile: default_profile(),
            sandbox: default_sandbox(),
            rules: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsConfig {
    #[serde(default = "default_max_tool_calls")]
    pub max_tool_calls: u32,
    #[serde(default = "default_max_run_seconds")]
    pub max_run_seconds: u64,
    #[serde(default = "default_max_output_tokens")]
    pub max_output_tokens: u64,
    /// `null` means token/time/tool caps only, with an explicit warning.
    #[serde(default = "default_max_spend_cents")]
    pub max_spend_cents: Option<u64>,
    #[serde(default = "default_tool_output_bytes")]
    pub tool_output_bytes: u64,
}

fn default_max_tool_calls() -> u32 {
    40
}
fn default_max_run_seconds() -> u64 {
    600
}
fn default_max_output_tokens() -> u64 {
    4_096
}
fn default_max_spend_cents() -> Option<u64> {
    Some(500)
}
fn default_tool_output_bytes() -> u64 {
    1_048_576
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_tool_calls: default_max_tool_calls(),
            max_run_seconds: default_max_run_seconds(),
            max_output_tokens: default_max_output_tokens(),
            max_spend_cents: default_max_spend_cents(),
            tool_output_bytes: default_tool_output_bytes(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivacyConfig {
    /// v0.1 has no analytics implementation; `true` is rejected.
    #[serde(default)]
    pub telemetry: bool,
    #[serde(default = "default_retention_days")]
    pub content_retention_days: u32,
}

fn default_retention_days() -> u32 {
    30
}

impl Default for PrivacyConfig {
    fn default() -> Self {
        Self {
            telemetry: false,
            content_retention_days: default_retention_days(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    BeforeTool,
    AfterTool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookConfig {
    pub id: String,
    pub event: HookEvent,
    /// Enabled is not trusted; trust records are separate.
    #[serde(default)]
    pub enabled: bool,
    pub argv: Vec<String>,
    #[serde(default = "default_hook_timeout")]
    pub timeout_seconds: u32,
}

fn default_hook_timeout() -> u32 {
    10
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerConfig {
    pub id: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env_allowlist: Vec<String>,
    /// Enabled is not trusted; starting the server requires separate trust.
    #[serde(default)]
    pub enabled: bool,
}

/// Trusted user configuration (`~/.config/bollo/config.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BolloConfig {
    pub schema_version: String,
    pub provider: ProviderConfig,
    #[serde(default)]
    pub permissions: PermissionsConfig,
    #[serde(default)]
    pub limits: LimitsConfig,
    #[serde(default)]
    pub privacy: PrivacyConfig,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
    #[serde(default)]
    pub hooks: Vec<HookConfig>,
}

/// Project configuration (`<workspace>/.bollo/config.json`). Untrusted: it can
/// suggest a stricter profile/sandbox and add restrictions, and nothing else.
/// Every other field is rejected by `deny_unknown_fields`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    pub schema_version: String,
    #[serde(default)]
    pub permissions: ProjectPermissions,
    #[serde(default)]
    pub limits: Option<LimitsConfig>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectPermissions {
    #[serde(default)]
    pub profile: Option<Profile>,
    #[serde(default)]
    pub sandbox: Option<SandboxMode>,
    #[serde(default)]
    pub rules: Vec<RuleConfig>,
}

pub fn parse_user_config(json: &str) -> Result<BolloConfig, PolicyError> {
    let config: BolloConfig = parse_strict(json)?;
    validate_common(&config.schema_version, &config.permissions.rules)?;
    validate_provider(&config.provider)?;
    validate_limits(&config.limits)?;
    if config.privacy.telemetry {
        return Err(PolicyError::Config(
            "privacy.telemetry must be false in v0.1".into(),
        ));
    }
    if !(1..=3650).contains(&config.privacy.content_retention_days) {
        return Err(PolicyError::Config(
            "privacy.content_retention_days must be 1..=3650".into(),
        ));
    }
    for hook in &config.hooks {
        if hook.argv.is_empty() {
            return Err(PolicyError::Config(format!(
                "hook {} has an empty argv",
                hook.id
            )));
        }
        if !(1..=30).contains(&hook.timeout_seconds) {
            return Err(PolicyError::Config(format!(
                "hook {} timeout_seconds must be 1..=30",
                hook.id
            )));
        }
    }
    let mut server_ids = std::collections::BTreeSet::new();
    for server in &config.mcp_servers {
        if !server_ids.insert(&server.id) {
            return Err(PolicyError::DuplicateRule(format!(
                "mcp server id {}",
                server.id
            )));
        }
    }
    Ok(config)
}

pub fn parse_project_config(json: &str) -> Result<ProjectConfig, PolicyError> {
    let config: ProjectConfig = parse_strict(json)?;
    validate_common(&config.schema_version, &config.permissions.rules)?;
    if let Some(limits) = &config.limits {
        validate_limits(limits)?;
    }
    Ok(config)
}

fn parse_strict<T: serde::de::DeserializeOwned>(json: &str) -> Result<T, PolicyError> {
    serde_json::from_str(json).map_err(|err| PolicyError::Config(format!("{err}")))
}

fn validate_common(schema_version: &str, rules: &[RuleConfig]) -> Result<(), PolicyError> {
    if schema_version != CONFIG_SCHEMA_VERSION {
        return Err(PolicyError::Config(format!(
            "unsupported schema_version {schema_version:?}; expected {CONFIG_SCHEMA_VERSION:?}"
        )));
    }
    let mut seen = std::collections::BTreeSet::new();
    for rule in rules {
        if !is_valid_opaque_id(&rule.id) {
            return Err(PolicyError::Config(format!(
                "rule id {:?} is not a valid opaque id",
                rule.id
            )));
        }
        if !seen.insert(rule.id.clone()) {
            return Err(PolicyError::DuplicateRule(rule.id.clone()));
        }
        if rule.tool.trim().is_empty() {
            return Err(PolicyError::Config(format!(
                "rule {} has an empty tool selector",
                rule.id
            )));
        }
        if let Some(glob) = &rule.path_glob {
            if glob.starts_with('!') {
                return Err(PolicyError::Config(format!(
                    "rule {} uses a negated glob; MVP forbids negation",
                    rule.id
                )));
            }
            if glob.is_empty() {
                return Err(PolicyError::Config(format!(
                    "rule {} has an empty path_glob",
                    rule.id
                )));
            }
        }
        if let Some(prefix) = &rule.argv_prefix {
            if prefix.is_empty() {
                return Err(PolicyError::Config(format!(
                    "rule {} has an empty argv_prefix",
                    rule.id
                )));
            }
        }
    }
    Ok(())
}

fn validate_provider(provider: &ProviderConfig) -> Result<(), PolicyError> {
    validate_base_url(&provider.base_url)?;
    if provider.model.trim().is_empty() {
        return Err(PolicyError::Config("provider.model must not be empty".into()));
    }
    let env = &provider.credential_env;
    let valid_env = !env.is_empty()
        && !env.starts_with(|c: char| c.is_ascii_digit())
        && env
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    if !valid_env {
        return Err(PolicyError::Config(
            "provider.credential_env must match ^[A-Z_][A-Z0-9_]*$ (a variable name, never a secret)"
                .into(),
        ));
    }
    if provider.context_tokens < 8_192 {
        return Err(PolicyError::Config(
            "provider.context_tokens must be >= 8192".into(),
        ));
    }
    Ok(())
}

fn validate_base_url(url: &str) -> Result<(), PolicyError> {
    let rest = url.strip_prefix("https://").ok_or_else(|| {
        PolicyError::Config(format!(
            "provider.base_url {url:?} must be an https origin (no http://, no path)"
        ))
    })?;
    if rest.is_empty() {
        return Err(PolicyError::Config(
            "provider.base_url has an empty origin".into(),
        ));
    }
    if rest.contains('@') {
        return Err(PolicyError::Config(
            "provider.base_url must not embed credentials".into(),
        ));
    }
    if rest.contains('#') || rest.contains('?') {
        return Err(PolicyError::Config(
            "provider.base_url must not contain a fragment or query".into(),
        ));
    }
    let authority = rest.split('/').next().unwrap_or_default();
    let has_path = rest.len() > authority.len() && rest[authority.len()..].trim_matches('/').len() > 0;
    if has_path {
        return Err(PolicyError::Config(
            "provider.base_url must be an origin; the adapter appends the documented path".into(),
        ));
    }
    if authority.is_empty() || authority.starts_with(':') {
        return Err(PolicyError::Config(
            "provider.base_url has an invalid authority".into(),
        ));
    }
    Ok(())
}

fn validate_limits(limits: &LimitsConfig) -> Result<(), PolicyError> {
    if !(1..=10_000).contains(&limits.max_tool_calls) {
        return Err(PolicyError::Config(
            "limits.max_tool_calls must be 1..=10000".into(),
        ));
    }
    if !(1..=86_400).contains(&limits.max_run_seconds) {
        return Err(PolicyError::Config(
            "limits.max_run_seconds must be 1..=86400".into(),
        ));
    }
    if limits.max_output_tokens < 1 {
        return Err(PolicyError::Config(
            "limits.max_output_tokens must be >= 1".into(),
        ));
    }
    if let Some(cents) = limits.max_spend_cents {
        if cents < 1 {
            return Err(PolicyError::Config(
                "limits.max_spend_cents must be >= 1 or null".into(),
            ));
        }
    }
    if !(1_024..=10_485_760).contains(&limits.tool_output_bytes) {
        return Err(PolicyError::Config(
            "limits.tool_output_bytes must be 1024..=10485760".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = include_str!("../../../docs/examples/config.balanced.json");

    #[test]
    fn accepts_documented_example() {
        let config = parse_user_config(GOOD).expect("documented example must parse");
        assert_eq!(config.permissions.profile, Profile::Balanced);
        assert_eq!(config.permissions.rules.len(), 2);
        assert!(config.provider.input_microusd_per_token.is_none());
    }

    #[test]
    fn rejects_unknown_keys() {
        let json = GOOD.replace("\"schema_version\"", "\"schema_versoin\"");
        assert!(parse_user_config(&json).is_err());
    }

    #[test]
    fn rejects_telemetry_true() {
        let json = GOOD.replace("\"telemetry\": false", "\"telemetry\": true");
        assert!(parse_user_config(&json).is_err());
    }

    #[test]
    fn rejects_duplicate_rule_ids() {
        let mut config: serde_json::Value = serde_json::from_str(GOOD).unwrap();
        let rules = config["permissions"]["rules"].as_array_mut().unwrap();
        let dup = rules[0].clone();
        rules.push(dup);
        let json = serde_json::to_string(&config).unwrap();
        assert!(matches!(
            parse_user_config(&json),
            Err(PolicyError::DuplicateRule(_))
        ));
    }

    #[test]
    fn rejects_negated_glob_and_http_origin() {
        let negated = GOOD.replace("\"**/.env\"", "\"!**/.env\"");
        assert!(parse_user_config(&negated).is_err());
        let http = GOOD.replace("https://api.anthropic.com", "http://api.anthropic.com");
        assert!(parse_user_config(&http).is_err());
        let userinfo = GOOD.replace(
            "https://api.anthropic.com",
            "https://user:pass@api.anthropic.com",
        );
        assert!(parse_user_config(&userinfo).is_err());
        let pathy = GOOD.replace(
            "https://api.anthropic.com",
            "https://api.anthropic.com/evil",
        );
        assert!(parse_user_config(&pathy).is_err());
    }

    #[test]
    fn rejects_env_secret_literal() {
        let json = GOOD.replace("ANTHROPIC_API_KEY", "sk-live-secret-value");
        assert!(parse_user_config(&json).is_err());
    }

    #[test]
    fn project_config_rejects_provider_and_hooks() {
        let provider = r#"{"schema_version":"0.1","provider":{"kind":"xai"}}"#;
        assert!(parse_project_config(provider).is_err());
        let hooks = r#"{"schema_version":"0.1","hooks":[]}"#;
        assert!(parse_project_config(hooks).is_err());
        let allowed = r#"{"schema_version":"0.1","permissions":{"profile":"read_only","rules":[]}}"#;
        assert!(parse_project_config(allowed).is_ok());
    }

    #[test]
    fn project_config_limits_only_is_allowed() {
        let limits = r#"{"schema_version":"0.1","limits":{"max_tool_calls":5,"max_run_seconds":60,"max_output_tokens":1024,"max_spend_cents":null,"tool_output_bytes":4096}}"#;
        let parsed = parse_project_config(limits).unwrap();
        assert_eq!(parsed.limits.unwrap().max_tool_calls, 5);
    }
}
