//! Configuration layering and the effective policy snapshot.
//!
//! Scalar merge order: defaults < trusted user < approved project < CLI session
//! flags. Rules accumulate by unique id and source; project rules may only
//! restrict. Deny/ask dominance spans all sources.

use serde::{Deserialize, Serialize};

use bollo_protocol::vocab::{Profile, SandboxCapabilities, SandboxMode};

use crate::config::{BolloConfig, LimitsConfig, ProjectConfig, RuleConfig};
use crate::PolicyError;

/// Where a value or rule came from. This is user-facing provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Default,
    User,
    Project,
    CliFlag,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Default => "default",
            Source::User => "user",
            Source::Project => "project",
            Source::CliFlag => "cli",
        }
    }
}

/// A rule with its provenance attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub id: String,
    pub effect: bollo_protocol::vocab::Effect,
    pub tool: String,
    pub path_glob: Option<String>,
    pub argv_prefix: Option<Vec<String>>,
    pub source: Source,
}

impl Rule {
    fn from_config(config: &RuleConfig, source: Source) -> Self {
        Self {
            id: config.id.clone(),
            effect: config.effect,
            tool: config.tool.clone(),
            path_glob: config.path_glob.clone(),
            argv_prefix: config.argv_prefix.clone(),
            source,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenanceEntry {
    pub field: String,
    pub value: String,
    pub source: Source,
}

/// CLI session flags; the top trusted layer for scalars.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CliOverrides {
    pub profile: Option<Profile>,
    pub sandbox: Option<SandboxMode>,
    pub model: Option<String>,
    pub max_tool_calls: Option<u32>,
    pub max_run_seconds: Option<u64>,
    pub max_spend_cents: Option<Option<u64>>,
    pub tool_output_bytes: Option<u64>,
}

/// The effective, immutable-for-the-run policy.
#[derive(Debug, Clone, PartialEq)]
pub struct PolicySnapshot {
    pub revision: u64,
    pub profile: Profile,
    pub sandbox: SandboxMode,
    pub rules: Vec<Rule>,
    pub limits: LimitsConfig,
    pub provider: crate::config::ProviderConfig,
    pub capabilities: SandboxCapabilities,
    pub pricing_known: bool,
    pub provenance: Vec<ProvenanceEntry>,
    pub warnings: Vec<String>,
}

fn profile_rank(profile: Profile) -> u8 {
    match profile {
        Profile::ReadOnly => 0,
        Profile::Balanced => 1,
        Profile::WorkspaceAuto => 2,
        Profile::Unrestricted => 3,
    }
}

fn sandbox_rank(sandbox: SandboxMode) -> u8 {
    match sandbox {
        SandboxMode::Workspace => 0,
        SandboxMode::Off => 1,
    }
}

/// Build the effective snapshot. Fails closed on any widening attempt from the
/// project layer and on duplicate rule ids across layers.
pub fn build_snapshot(
    user: &BolloConfig,
    project: Option<&ProjectConfig>,
    cli: &CliOverrides,
    capabilities: SandboxCapabilities,
    revision: u64,
) -> Result<PolicySnapshot, PolicyError> {
    let mut provenance = Vec::new();

    // --- profile ---
    let mut profile = user.permissions.profile;
    provenance.push(ProvenanceEntry {
        field: "permissions.profile".into(),
        value: profile.as_str().into(),
        source: Source::User,
    });
    if let Some(project) = project {
        if let Some(project_profile) = project.permissions.profile {
            if profile_rank(project_profile) > profile_rank(profile) {
                return Err(PolicyError::ProjectWidening(format!(
                    "widen permissions.profile from {} to {}",
                    profile.as_str(),
                    project_profile.as_str()
                )));
            }
            if project_profile != profile {
                profile = project_profile;
                provenance.push(ProvenanceEntry {
                    field: "permissions.profile".into(),
                    value: profile.as_str().into(),
                    source: Source::Project,
                });
            }
        }
    }
    if let Some(cli_profile) = cli.profile {
        profile = cli_profile;
        provenance.push(ProvenanceEntry {
            field: "permissions.profile".into(),
            value: profile.as_str().into(),
            source: Source::CliFlag,
        });
    }

    // --- sandbox ---
    let mut sandbox = user.permissions.sandbox;
    provenance.push(ProvenanceEntry {
        field: "permissions.sandbox".into(),
        value: format!("{sandbox:?}").to_ascii_lowercase(),
        source: Source::User,
    });
    if let Some(project) = project {
        if let Some(project_sandbox) = project.permissions.sandbox {
            if sandbox_rank(project_sandbox) > sandbox_rank(sandbox) {
                return Err(PolicyError::ProjectWidening(
                    "disable isolation (sandbox off)".into(),
                ));
            }
            if project_sandbox != sandbox {
                sandbox = project_sandbox;
                provenance.push(ProvenanceEntry {
                    field: "permissions.sandbox".into(),
                    value: format!("{sandbox:?}").to_ascii_lowercase(),
                    source: Source::Project,
                });
            }
        }
    }
    if let Some(cli_sandbox) = cli.sandbox {
        sandbox = cli_sandbox;
        provenance.push(ProvenanceEntry {
            field: "permissions.sandbox".into(),
            value: format!("{sandbox:?}").to_ascii_lowercase(),
            source: Source::CliFlag,
        });
    }

    // --- limits ---
    let mut limits = user.limits.clone();
    if let Some(project) = project {
        if let Some(project_limits) = &project.limits {
            lower_limits(&mut limits, project_limits)?;
            provenance.push(ProvenanceEntry {
                field: "limits".into(),
                value: "project ceilings applied".into(),
                source: Source::Project,
            });
        }
    }
    if let Some(value) = cli.max_tool_calls {
        limits.max_tool_calls = value;
    }
    if let Some(value) = cli.max_run_seconds {
        limits.max_run_seconds = value;
    }
    if let Some(value) = cli.max_spend_cents {
        limits.max_spend_cents = value;
    }
    if let Some(value) = cli.tool_output_bytes {
        limits.tool_output_bytes = value;
    }

    // --- rules ---
    let mut rules: Vec<Rule> = user
        .permissions
        .rules
        .iter()
        .map(|rule| Rule::from_config(rule, Source::User))
        .collect();
    if let Some(project) = project {
        for rule in &project.permissions.rules {
            if matches!(rule.effect, bollo_protocol::vocab::Effect::Allow) {
                return Err(PolicyError::ProjectWidening(format!(
                    "grant an allow rule ({})",
                    rule.id
                )));
            }
            rules.push(Rule::from_config(rule, Source::Project));
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for rule in &rules {
        if !seen.insert(rule.id.clone()) {
            return Err(PolicyError::DuplicateRule(rule.id.clone()));
        }
    }

    // --- provider overrides / pricing ---
    let mut provider = user.provider.clone();
    if let Some(model) = &cli.model {
        provider.model = model.clone();
    }
    let pricing_known = provider.input_microusd_per_token.is_some()
        && provider.output_microusd_per_token.is_some();
    let mut warnings = Vec::new();
    if limits.max_spend_cents.is_some() && !pricing_known {
        warnings.push(
            "pricing_unknown: a finite spend cap is set but no trusted price table is configured; \
             requests will block until prices are configured or the cap is set to null"
                .to_string(),
        );
    }

    Ok(PolicySnapshot {
        revision,
        profile,
        sandbox,
        rules,
        limits,
        provider,
        capabilities,
        pricing_known,
        provenance,
        warnings,
    })
}

fn lower_limits(limits: &mut LimitsConfig, project: &LimitsConfig) -> Result<(), PolicyError> {
    macro_rules! lower {
        ($field:ident) => {
            if project.$field > limits.$field {
                return Err(PolicyError::ProjectWidening(format!(
                    "raise limits.{} from {} to {}",
                    stringify!($field),
                    limits.$field,
                    project.$field
                )));
            }
            limits.$field = project.$field;
        };
    }
    lower!(max_tool_calls);
    lower!(max_run_seconds);
    lower!(max_output_tokens);
    lower!(tool_output_bytes);
    match (limits.max_spend_cents, project.max_spend_cents) {
        (_, None) => {
            return Err(PolicyError::ProjectWidening(
                "remove the spend cap (set it to null)".into(),
            ))
        }
        (Some(current), Some(projected)) => {
            if projected > current {
                return Err(PolicyError::ProjectWidening(format!(
                    "raise limits.max_spend_cents from {current} to {projected}"
                )));
            }
            limits.max_spend_cents = Some(projected);
        }
        (None, Some(_)) => {
            return Err(PolicyError::ProjectWidening(
                "add a spend cap where the trusted configuration has none".into(),
            ))
        }
    }
    Ok(())
}

/// Startup validation. Missing enforcement is a startup error, never a silent
/// host fallback.
pub fn validate_startup(snapshot: &PolicySnapshot) -> Result<(), PolicyError> {
    if snapshot.sandbox == SandboxMode::Workspace {
        if !snapshot.capabilities.filesystem_containment {
            return Err(PolicyError::Startup(
                "workspace sandbox requested but filesystem containment is unavailable on this \
                 host; refusing to run on host without an explicit sandbox=off selection"
                    .into(),
            ));
        }
        if !snapshot.capabilities.network_denied {
            return Err(PolicyError::Startup(
                "workspace sandbox requires verified tool-network denial; none is available"
                    .into(),
            ));
        }
    }
    if snapshot.profile == Profile::WorkspaceAuto
        && !(snapshot.sandbox == SandboxMode::Workspace
            && snapshot.capabilities.supports_workspace_auto())
    {
        return Err(PolicyError::Startup(
            "workspace_auto requires verified filesystem containment and tool network denial"
                .into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollo_protocol::vocab::Platform;

    fn user_config() -> BolloConfig {
        crate::config::parse_user_config(include_str!(
            "../../../docs/examples/config.balanced.json"
        ))
        .unwrap()
    }

    fn verified_caps() -> SandboxCapabilities {
        SandboxCapabilities {
            platform: Platform::Linux,
            filesystem_containment: true,
            network_denied: true,
            backend: "test-probe".into(),
        }
    }

    #[test]
    fn defaults_and_user_layer_merge() {
        let user = user_config();
        let snap = build_snapshot(&user, None, &CliOverrides::default(), verified_caps(), 1).unwrap();
        assert_eq!(snap.profile, Profile::Balanced);
        assert_eq!(snap.sandbox, SandboxMode::Workspace);
        assert_eq!(snap.rules.len(), 2);
        assert_eq!(snap.limits.max_tool_calls, 40);
        assert!(!snap.pricing_known);
        assert_eq!(snap.warnings.len(), 1);
        validate_startup(&snap).unwrap();
    }

    #[test]
    fn cli_flags_win_over_user_scalars() {
        let user = user_config();
        let cli = CliOverrides {
            profile: Some(Profile::ReadOnly),
            max_tool_calls: Some(3),
            ..Default::default()
        };
        let snap = build_snapshot(&user, None, &cli, verified_caps(), 2).unwrap();
        assert_eq!(snap.profile, Profile::ReadOnly);
        assert_eq!(snap.limits.max_tool_calls, 3);
    }

    #[test]
    fn project_may_restrict_but_not_widen() {
        let user = user_config();
        let tightening = crate::config::parse_project_config(
            r#"{"schema_version":"0.1","permissions":{"profile":"read_only","rules":[
                {"id":"no-logs","effect":"deny","tool":"read_file","path_glob":"logs/**"}]}}"#,
        )
        .unwrap();
        let snap =
            build_snapshot(&user, Some(&tightening), &CliOverrides::default(), verified_caps(), 3)
                .unwrap();
        assert_eq!(snap.profile, Profile::ReadOnly);
        assert_eq!(snap.rules.len(), 3);

        let widening = crate::config::parse_project_config(
            r#"{"schema_version":"0.1","permissions":{"profile":"unrestricted"}}"#,
        )
        .unwrap();
        assert!(matches!(
            build_snapshot(&user, Some(&widening), &CliOverrides::default(), verified_caps(), 4),
            Err(PolicyError::ProjectWidening(_))
        ));
    }

    #[test]
    fn project_allow_rule_is_rejected() {
        let user = user_config();
        let escalating = crate::config::parse_project_config(
            r#"{"schema_version":"0.1","permissions":{"rules":[
                {"id":"let-me-write","effect":"allow","tool":"write_file"}]}}"#,
        )
        .unwrap();
        assert!(matches!(
            build_snapshot(&user, Some(&escalating), &CliOverrides::default(), verified_caps(), 5),
            Err(PolicyError::ProjectWidening(_))
        ));
    }

    #[test]
    fn duplicate_rule_ids_across_layers_fail() {
        let user = user_config();
        let duplicate = crate::config::parse_project_config(
            r#"{"schema_version":"0.1","permissions":{"rules":[
                {"id":"private-env","effect":"deny","tool":"*","path_glob":"**/.env"}]}}"#,
        )
        .unwrap();
        assert!(matches!(
            build_snapshot(&user, Some(&duplicate), &CliOverrides::default(), verified_caps(), 6),
            Err(PolicyError::DuplicateRule(_))
        ));
    }

    #[test]
    fn startup_refuses_workspace_auto_without_verified_isolation() {
        let user = user_config();
        let caps_off = SandboxCapabilities {
            platform: Platform::Windows,
            filesystem_containment: false,
            network_denied: false,
            backend: "none".into(),
        };
        let cli = CliOverrides {
            profile: Some(Profile::WorkspaceAuto),
            sandbox: Some(SandboxMode::Off),
            ..Default::default()
        };
        let snap = build_snapshot(&user, None, &cli, caps_off.clone(), 7).unwrap();
        assert!(validate_startup(&snap).is_err());

        let cli = CliOverrides {
            profile: Some(Profile::WorkspaceAuto),
            sandbox: Some(SandboxMode::Workspace),
            ..Default::default()
        };
        let caps_no_network = SandboxCapabilities {
            network_denied: false,
            ..caps_off
        };
        let snap = build_snapshot(&user, None, &cli, caps_no_network, 8).unwrap();
        assert!(validate_startup(&snap).is_err());
    }
}
