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

// Canonical classifier defaults (BH-021). `bollo-classifier` aliases these so
// the schema, the parser and the adapter cannot drift apart.
pub const CLASSIFIER_DEFAULT_ORIGIN: &str = "https://api.typesafe.ai";
pub const CLASSIFIER_DEFAULT_MODEL: &str = "jev-1.13.0";
pub const CLASSIFIER_DEFAULT_CREDENTIAL_ENV: &str = "TYPESAFE_API_KEY";
pub const CLASSIFIER_DEFAULT_TIMEOUT_MS: u64 = 1_500;
pub const CLASSIFIER_MIN_TIMEOUT_MS: u64 = 100;
pub const CLASSIFIER_MAX_TIMEOUT_MS: u64 = 10_000;
pub const CLASSIFIER_DEFAULT_ESCALATE_AT: f64 = 1.0;
pub const CLASSIFIER_DEFAULT_MIN_CONFIDENCE: f64 = 0.5;
pub const CLASSIFIER_DEFAULT_CREDENTIAL_THRESHOLD: f64 = 0.9;

/// Advisory classifier section (BH-021), strict like the rest of the schema.
/// Absent means disabled; present means every field is validated even while
/// `enabled` is false, so enabling later is a pure switch, never a surprise.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassifierConfig {
    /// Opt-in. The default build still has no live transport, so an enabled
    /// block alone cannot emit traffic without the `live-http` feature.
    pub enabled: bool,
    #[serde(default = "default_classifier_origin")]
    pub origin: String,
    #[serde(default = "default_classifier_model")]
    pub model: String,
    #[serde(default = "default_classifier_credential_env")]
    pub credential_env: String,
    #[serde(default = "default_classifier_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_classifier_escalate_at")]
    pub escalate_at: f64,
    #[serde(default = "default_classifier_min_confidence")]
    pub min_confidence: f64,
    #[serde(default = "default_classifier_credential_threshold")]
    pub credential_threshold: f64,
    #[serde(default = "default_classifier_profiles")]
    pub profiles: Vec<Profile>,
}

fn default_classifier_origin() -> String {
    CLASSIFIER_DEFAULT_ORIGIN.to_string()
}
fn default_classifier_model() -> String {
    CLASSIFIER_DEFAULT_MODEL.to_string()
}
fn default_classifier_credential_env() -> String {
    CLASSIFIER_DEFAULT_CREDENTIAL_ENV.to_string()
}
fn default_classifier_timeout_ms() -> u64 {
    CLASSIFIER_DEFAULT_TIMEOUT_MS
}
fn default_classifier_escalate_at() -> f64 {
    CLASSIFIER_DEFAULT_ESCALATE_AT
}
fn default_classifier_min_confidence() -> f64 {
    CLASSIFIER_DEFAULT_MIN_CONFIDENCE
}
fn default_classifier_credential_threshold() -> f64 {
    CLASSIFIER_DEFAULT_CREDENTIAL_THRESHOLD
}
fn default_classifier_profiles() -> Vec<Profile> {
    vec![Profile::Balanced, Profile::WorkspaceAuto]
}

impl Default for ClassifierConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            origin: default_classifier_origin(),
            model: default_classifier_model(),
            credential_env: default_classifier_credential_env(),
            timeout_ms: default_classifier_timeout_ms(),
            escalate_at: default_classifier_escalate_at(),
            min_confidence: default_classifier_min_confidence(),
            credential_threshold: default_classifier_credential_threshold(),
            profiles: default_classifier_profiles(),
        }
    }
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Advisory classifier (BH-021). Absent means the default: disabled, zero
    /// calls. Project configuration can never carry this block.
    #[serde(default)]
    pub classifier: ClassifierConfig,
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
    validate_classifier(&config.classifier)?;
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
    validate_https_origin(&provider.base_url, "provider.base_url")?;
    if provider.model.trim().is_empty() {
        return Err(PolicyError::Config("provider.model must not be empty".into()));
    }
    if !is_credential_env_name(&provider.credential_env) {
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

/// Credential references are environment variable names, never secrets.
fn is_credential_env_name(env: &str) -> bool {
    !env.is_empty()
        && !env.starts_with(|c: char| c.is_ascii_digit())
        && env
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Reject anything that is not an exact HTTPS origin. `label` names the field
/// so the diagnostics stay source-aware.
fn validate_https_origin(url: &str, label: &str) -> Result<(), PolicyError> {
    let rest = url.strip_prefix("https://").ok_or_else(|| {
        PolicyError::Config(format!(
            "{label} {url:?} must be an https origin (no http://, no path)"
        ))
    })?;
    if rest.is_empty() {
        return Err(PolicyError::Config(format!("{label} has an empty origin")));
    }
    if rest.contains('@') {
        return Err(PolicyError::Config(format!(
            "{label} must not embed credentials"
        )));
    }
    if rest.contains('#') || rest.contains('?') {
        return Err(PolicyError::Config(format!(
            "{label} must not contain a fragment or query"
        )));
    }
    let authority = rest.split('/').next().unwrap_or_default();
    let has_path =
        rest.len() > authority.len() && rest[authority.len()..].trim_matches('/').len() > 0;
    if has_path {
        return Err(PolicyError::Config(format!(
            "{label} must be an origin; the adapter appends the documented path"
        )));
    }
    if authority.is_empty() || authority.starts_with(':') {
        return Err(PolicyError::Config(format!(
            "{label} has an invalid authority"
        )));
    }
    Ok(())
}

/// The classifier block is strict even while `enabled` is false: thresholds,
/// origins and credential names are checked now, so flipping `enabled` cannot
/// surface a latent configuration error at the first eligible decision.
fn validate_classifier(classifier: &ClassifierConfig) -> Result<(), PolicyError> {
    validate_https_origin(&classifier.origin, "classifier.origin")?;
    if classifier.model.trim().is_empty() {
        return Err(PolicyError::Config(
            "classifier.model must not be empty".into(),
        ));
    }
    if !is_credential_env_name(&classifier.credential_env) {
        return Err(PolicyError::Config(
            "classifier.credential_env must match ^[A-Z_][A-Z0-9_]*$ (a variable name, never a secret)"
                .into(),
        ));
    }
    if !(CLASSIFIER_MIN_TIMEOUT_MS..=CLASSIFIER_MAX_TIMEOUT_MS).contains(&classifier.timeout_ms) {
        return Err(PolicyError::Config(format!(
            "classifier.timeout_ms must be {CLASSIFIER_MIN_TIMEOUT_MS}..={CLASSIFIER_MAX_TIMEOUT_MS}"
        )));
    }
    let thresholds = crate::classifier::EscalationThresholds {
        escalate_at: classifier.escalate_at,
        min_confidence: classifier.min_confidence,
        credential_threshold: classifier.credential_threshold,
    };
    thresholds.validate().map_err(PolicyError::Config)?;
    if classifier.profiles.is_empty() {
        return Err(PolicyError::Config(
            "classifier.profiles must not be empty".into(),
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for profile in &classifier.profiles {
        if !seen.insert(*profile) {
            return Err(PolicyError::Config(format!(
                "classifier.profiles contains duplicate {}",
                profile.as_str()
            )));
        }
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

    /// Inject a classifier block into the documented example. The example
    /// itself intentionally omits it: the block is optional and disabled by
    /// default.
    fn with_classifier(fragment: &str) -> String {
        let mut value: serde_json::Value = serde_json::from_str(GOOD).unwrap();
        value["classifier"] = serde_json::from_str(fragment).unwrap();
        serde_json::to_string(&value).unwrap()
    }

    #[test]
    fn classifier_absent_defaults_to_disabled() {
        let config = parse_user_config(GOOD).unwrap();
        let classifier = &config.classifier;
        assert!(!classifier.enabled);
        assert_eq!(classifier.origin, CLASSIFIER_DEFAULT_ORIGIN);
        assert_eq!(classifier.model, CLASSIFIER_DEFAULT_MODEL);
        assert_eq!(classifier.credential_env, CLASSIFIER_DEFAULT_CREDENTIAL_ENV);
        assert_eq!(classifier.timeout_ms, CLASSIFIER_DEFAULT_TIMEOUT_MS);
        assert_eq!(classifier.escalate_at, CLASSIFIER_DEFAULT_ESCALATE_AT);
        assert_eq!(classifier.min_confidence, CLASSIFIER_DEFAULT_MIN_CONFIDENCE);
        assert_eq!(
            classifier.credential_threshold,
            CLASSIFIER_DEFAULT_CREDENTIAL_THRESHOLD
        );
        assert_eq!(
            classifier.profiles,
            vec![Profile::Balanced, Profile::WorkspaceAuto]
        );
    }

    #[test]
    fn classifier_minimal_block_parses_as_disabled() {
        let json = with_classifier(r#"{"enabled": false}"#);
        let config = parse_user_config(&json).unwrap();
        assert!(!config.classifier.enabled);
        assert_eq!(config.classifier.model, CLASSIFIER_DEFAULT_MODEL);
    }

    #[test]
    fn classifier_full_block_parses() {
        let json = with_classifier(
            r#"{"enabled": true, "origin": "https://api.typesafe.ai", "model": "jev-1.13.0",
                "credential_env": "TYPESAFE_API_KEY", "timeout_ms": 2500, "escalate_at": 0.9,
                "min_confidence": 0.6, "credential_threshold": 0.95,
                "profiles": ["balanced", "unrestricted"]}"#,
        );
        let config = parse_user_config(&json).unwrap();
        assert!(config.classifier.enabled);
        assert_eq!(config.classifier.timeout_ms, 2500);
        assert_eq!(config.classifier.escalate_at, 0.9);
        assert_eq!(config.classifier.min_confidence, 0.6);
        assert_eq!(config.classifier.credential_threshold, 0.95);
        assert_eq!(
            config.classifier.profiles,
            vec![Profile::Balanced, Profile::Unrestricted]
        );
    }

    #[test]
    fn classifier_rejects_strict_shape_violations() {
        // `enabled` is required whenever the block is present.
        assert!(
            parse_user_config(&with_classifier(r#"{"origin": "https://api.typesafe.ai"}"#))
                .is_err()
        );
        // Unknown keys are rejected; secrets never become configuration.
        assert!(parse_user_config(&with_classifier(
            r#"{"enabled": true, "api_key": "sk-live-secret-value"}"#
        ))
        .is_err());
        // The origin must be an exact HTTPS origin.
        for origin in [
            "http://api.typesafe.ai",
            "https://api.typesafe.ai/v1",
            "https://api.typesafe.ai?x=1",
            "https://user:pass@api.typesafe.ai",
        ] {
            let json = with_classifier(&format!(r#"{{"enabled": true, "origin": "{origin}"}}"#));
            assert!(
                parse_user_config(&json).is_err(),
                "origin {origin} accepted"
            );
        }
        // The credential field is a variable name, never a secret literal.
        assert!(parse_user_config(&with_classifier(
            r#"{"enabled": true, "credential_env": "sk-live-secret-value"}"#
        ))
        .is_err());
        // The advisory deadline is bounded.
        for timeout in [50, 20_000] {
            let json = with_classifier(&format!(r#"{{"enabled": true, "timeout_ms": {timeout}}}"#));
            assert!(
                parse_user_config(&json).is_err(),
                "timeout {timeout} accepted"
            );
        }
        // Thresholds are finite probabilities in 0..=1.
        for fragment in [
            r#"{"enabled": true, "escalate_at": 1.5}"#,
            r#"{"enabled": true, "min_confidence": -0.1}"#,
            r#"{"enabled": true, "credential_threshold": 2}"#,
        ] {
            assert!(parse_user_config(&with_classifier(fragment)).is_err());
        }
        // Profiles are a non-empty, duplicate-free subset of known profiles.
        for fragment in [
            r#"{"enabled": true, "profiles": []}"#,
            r#"{"enabled": true, "profiles": ["balanced", "balanced"]}"#,
            r#"{"enabled": true, "profiles": ["yolo"]}"#,
        ] {
            assert!(parse_user_config(&with_classifier(fragment)).is_err());
        }
    }

    #[test]
    fn project_config_rejects_classifier_block() {
        let json = r#"{"schema_version":"0.1","classifier":{"enabled":true}}"#;
        assert!(parse_project_config(json).is_err());
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
