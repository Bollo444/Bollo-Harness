//! The pure evaluator: allow/ask/deny with explicit provenance.
//!
//! Algorithm (docs/security/permissions.md):
//! 1. inspection ceiling — `read_only` denies anything beyond reads;
//! 2. workspace boundary — the workspace sandbox denies outside targets;
//! 3. explicit rules — deny wins, then ask, then allow, across all sources;
//! 4. preset fallback by profile and effect class.

use bollo_protocol::vocab::{Effect, EffectClass, Profile, SandboxMode};

use crate::layers::{PolicySnapshot, Rule};
use crate::normalize::NormalizedIntent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyDecision {
    pub effect: Effect,
    pub rule_id: Option<String>,
    pub provenance: Vec<String>,
    pub reason: String,
}

impl PolicyDecision {
    /// Human-readable one-liner used in approval summaries and events.
    pub fn explain(&self) -> String {
        match &self.rule_id {
            Some(rule) => format!("{} (rule {rule})", self.reason),
            None => self.reason.clone(),
        }
    }
}

pub fn evaluate(intent: &NormalizedIntent, snapshot: &PolicySnapshot) -> PolicyDecision {
    // 1. Inspection ceiling.
    if snapshot.profile.is_inspection_ceiling() && intent.effect != EffectClass::Read {
        return PolicyDecision {
            effect: Effect::Deny,
            rule_id: None,
            provenance: vec![format!("profile:{}", snapshot.profile.as_str())],
            reason: "inspection ceiling: read_only denies mutation, execution and external effects"
                .to_string(),
        };
    }

    // 2. Workspace boundary. (Symlinks are already resolved by the workspace
    //    layer; this sees the real normalized target.)
    if snapshot.sandbox == SandboxMode::Workspace && intent.paths.iter().any(|p| p.is_outside()) {
        return PolicyDecision {
            effect: Effect::Deny,
            rule_id: None,
            provenance: vec!["sandbox:workspace".to_string()],
            reason: "normalized target exceeds the workspace boundary".to_string(),
        };
    }

    // 3. Explicit rules: deny first, then ask, then allow. All matching deny
    //    rules win regardless of specificity.
    let matching: Vec<&Rule> = snapshot
        .rules
        .iter()
        .filter(|rule| rule_matches(rule, intent))
        .collect();
    for effect in [Effect::Deny, Effect::Ask, Effect::Allow] {
        if let Some(rule) = matching.iter().find(|rule| rule.effect == effect) {
            return PolicyDecision {
                effect,
                rule_id: Some(rule.id.clone()),
                provenance: vec![
                    format!("rule:{}", rule.id),
                    format!("source:{}", rule.source.as_str()),
                    format!("profile:{}", snapshot.profile.as_str()),
                ],
                reason: format!(
                    "explicit {} rule",
                    rule.effect.as_str()
                ),
            };
        }
    }

    // 4. Preset fallback.
    let (effect, reason) = preset_fallback(snapshot.profile, intent.effect);
    PolicyDecision {
        effect,
        rule_id: None,
        provenance: vec![
            format!("profile:{}", snapshot.profile.as_str()),
            format!("fallback:{}", effect_class_name(intent.effect)),
        ],
        reason: reason.to_string(),
    }
}

fn effect_class_name(class: EffectClass) -> &'static str {
    match class {
        EffectClass::Read => "read",
        EffectClass::WorkspaceMutation => "workspace_mutation",
        EffectClass::ExternalMutation => "external_mutation",
        EffectClass::OutsideWorkspace => "outside_workspace",
        EffectClass::Execution => "execution",
    }
}

fn rule_matches(rule: &Rule, intent: &NormalizedIntent) -> bool {
    if rule.tool != "*" && rule.tool != intent.tool {
        return false;
    }
    if let Some(glob) = &rule.path_glob {
        if !intent.paths.iter().any(|path| path.matches_glob(glob)) {
            return false;
        }
    }
    if let Some(prefix) = &rule.argv_prefix {
        let Some(argv) = &intent.argv else {
            return false;
        };
        if argv.len() < prefix.len() {
            return false;
        }
        if argv[..prefix.len()] != prefix[..] {
            return false;
        }
    }
    true
}

fn preset_fallback(profile: Profile, class: EffectClass) -> (Effect, &'static str) {
    use Effect::*;
    use EffectClass::*;
    match class {
        Read => (Allow, "read-only preset fallback"),
        WorkspaceMutation => match profile {
            Profile::ReadOnly => (Deny, "inspection ceiling denies workspace mutation"),
            Profile::Balanced => (Ask, "balanced mutation fallback"),
            Profile::WorkspaceAuto => (Allow, "contained workspace mutation"),
            Profile::Unrestricted => (Allow, "unrestricted preset fallback"),
        },
        Execution => match profile {
            Profile::ReadOnly => (Deny, "inspection ceiling denies arbitrary exec"),
            Profile::Balanced => (Ask, "balanced execution fallback"),
            Profile::WorkspaceAuto => (Allow, "contained execution fallback"),
            Profile::Unrestricted => (
                Allow,
                "host execution accepted; child behavior is not contained",
            ),
        },
        ExternalMutation => match profile {
            Profile::ReadOnly => (Deny, "inspection ceiling denies external effects"),
            Profile::Balanced | Profile::WorkspaceAuto => (
                Ask,
                "annotations do not prove absence of effects; external calls ask",
            ),
            Profile::Unrestricted => (Allow, "unrestricted external fallback"),
        },
        OutsideWorkspace => match profile {
            Profile::ReadOnly => (Deny, "inspection ceiling denies outside effects"),
            Profile::Balanced => (Ask, "outside-workspace effect requires explicit scope"),
            Profile::WorkspaceAuto => (
                Deny,
                "outside-workspace effects are denied under workspace_auto",
            ),
            Profile::Unrestricted => (Allow, "unrestricted outside-workspace fallback"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{parse_user_config, ProviderConfig};
    use crate::layers::Source;
    use crate::normalize::{effect_class_for, scoped_path, NormalizedIntent};
    use bollo_protocol::vocab::{Platform, SandboxCapabilities, ToolClass};

    fn root() -> &'static str {
        if cfg!(windows) {
            "C:/proj"
        } else {
            "/proj"
        }
    }

    fn provider() -> ProviderConfig {
        parse_user_config(include_str!("../../../docs/examples/config.balanced.json"))
            .unwrap()
            .provider
    }

    fn snapshot(
        profile: Profile,
        sandbox: SandboxMode,
        verified: bool,
        rules: Vec<Rule>,
    ) -> PolicySnapshot {
        PolicySnapshot {
            revision: 1,
            profile,
            sandbox,
            rules,
            limits: Default::default(),
            provider: provider(),
            capabilities: SandboxCapabilities {
                platform: Platform::Linux,
                filesystem_containment: verified,
                network_denied: verified,
                backend: "test".into(),
            },
            pricing_known: true,
            provenance: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn rule(
        id: &str,
        effect: Effect,
        tool: &str,
        path_glob: Option<&str>,
        argv_prefix: Option<Vec<&str>>,
    ) -> Rule {
        Rule {
            id: id.into(),
            effect,
            tool: tool.into(),
            path_glob: path_glob.map(str::to_string),
            argv_prefix: argv_prefix.map(|v| v.into_iter().map(str::to_string).collect()),
            source: Source::User,
        }
    }

    fn intent(
        tool: &str,
        class: ToolClass,
        paths: Vec<&str>,
        argv: Option<Vec<&str>>,
    ) -> NormalizedIntent {
        let scoped: Vec<_> = paths
            .iter()
            .map(|p| scoped_path(root(), p).unwrap())
            .collect();
        let any_outside = scoped.iter().any(|p| p.is_outside());
        NormalizedIntent {
            tool: tool.into(),
            class,
            effect: effect_class_for(class, any_outside),
            paths: scoped,
            argv: argv.map(|v| v.into_iter().map(str::to_string).collect()),
            cwd: None,
            environment_names: Vec::new(),
            summary: format!("{tool} test intent"),
            arguments_digest: "0".repeat(64),
        }
    }

    #[test]
    fn documented_policy_cases_match_engine() {
        #[derive(serde::Deserialize)]
        struct CaseFile {
            cases: Vec<Case>,
        }
        #[derive(serde::Deserialize)]
        struct Case {
            id: String,
            profile: Profile,
            sandbox: SandboxMode,
            scenario: String,
            expected: String,
            reason: String,
        }

        let file: CaseFile = serde_json::from_str(include_str!(
            "../../../docs/examples/policy-cases.json"
        ))
        .unwrap();

        for case in &file.cases {
            // PC-014 is a hook-layer case (AT-015), not pure policy.
            if case.id == "PC-014" {
                continue;
            }
            // Startup cases are validated through validate_startup.
            if case.id == "PC-004" || case.id == "PC-013" {
                let caps = match case.id.as_str() {
                    "PC-004" => (true, true),
                    _ => (true, false),
                };
                let snap = snapshot(
                    case.profile,
                    case.sandbox,
                    caps.0 && caps.1,
                    Vec::new(),
                );
                let mut snap = snap;
                snap.capabilities.filesystem_containment = caps.0;
                snap.capabilities.network_denied = caps.1;
                assert!(
                    crate::layers::validate_startup(&snap).is_err(),
                    "{}: expected startup error ({})",
                    case.id,
                    case.reason
                );
                assert_eq!(case.expected, "startup_error");
                continue;
            }

            let (intent, snapshot) = match case.id.as_str() {
                "PC-001" => (
                    intent("read_file", ToolClass::Read, vec!["src/main.rs"], None),
                    snapshot(case.profile, case.sandbox, true, Vec::new()),
                ),
                "PC-002" => (
                    intent("write_file", ToolClass::Write, vec!["src/main.rs"], None),
                    snapshot(case.profile, case.sandbox, true, Vec::new()),
                ),
                "PC-003" => (
                    intent(
                        "exec",
                        ToolClass::Exec,
                        Vec::new(),
                        Some(vec!["cargo", "test"]),
                    ),
                    snapshot(case.profile, case.sandbox, true, Vec::new()),
                ),
                "PC-005" => (
                    intent(
                        "exec",
                        ToolClass::Exec,
                        Vec::new(),
                        Some(vec!["git", "status"]),
                    ),
                    snapshot(case.profile, case.sandbox, true, Vec::new()),
                ),
                "PC-006" => (
                    intent("read_file", ToolClass::Read, vec![".env"], None),
                    snapshot(
                        case.profile,
                        case.sandbox,
                        true,
                        vec![rule("private-env", Effect::Deny, "*", Some("**/.env"), None)],
                    ),
                ),
                "PC-007" => (
                    intent("exec", ToolClass::Exec, Vec::new(), Some(vec!["$SHELL"])),
                    snapshot(
                        case.profile,
                        case.sandbox,
                        true,
                        vec![rule("ask-exec", Effect::Ask, "exec", None, None)],
                    ),
                ),
                "PC-008" => (
                    intent("write_file", ToolClass::Write, vec!["src/main.rs"], None),
                    snapshot(
                        case.profile,
                        case.sandbox,
                        true,
                        vec![
                            rule("deny-writes", Effect::Deny, "write_file", None, None),
                            rule(
                                "allow-src",
                                Effect::Allow,
                                "write_file",
                                Some("src/**"),
                                None,
                            ),
                        ],
                    ),
                ),
                "PC-009" => (
                    intent("mcp:server:read", ToolClass::Mcp, Vec::new(), None),
                    snapshot(case.profile, case.sandbox, true, Vec::new()),
                ),
                "PC-010" => (
                    intent("write_file", ToolClass::Write, vec!["../escape.txt"], None),
                    snapshot(case.profile, case.sandbox, true, Vec::new()),
                ),
                "PC-011" => (
                    intent("exec", ToolClass::Exec, Vec::new(), Some(vec!["sh", "-c", "id"])),
                    snapshot(case.profile, case.sandbox, true, Vec::new()),
                ),
                "PC-012" => (
                    intent(
                        "exec",
                        ToolClass::Exec,
                        Vec::new(),
                        Some(vec!["cargo", "test"]),
                    ),
                    snapshot(case.profile, case.sandbox, true, Vec::new()),
                ),
                "PC-015" => (
                    intent("write_file", ToolClass::Write, vec!["src/main.rs"], None),
                    snapshot(
                        case.profile,
                        case.sandbox,
                        true,
                        vec![rule("allow-write", Effect::Allow, "write_file", None, None)],
                    ),
                ),
                other => panic!("policy case {other} has no engine mapping"),
            };

            assert_eq!(
                intent.effect,
                match case.id.as_str() {
                    "PC-001" | "PC-006" => EffectClass::Read,
                    "PC-010" => EffectClass::OutsideWorkspace,
                    _ => intent.effect,
                },
                "{}: unexpected effect class",
                case.id
            );

            let decision = evaluate(&intent, &snapshot);
            assert_eq!(
                decision.effect.as_str(),
                case.expected,
                "{} ({}): {} -> {}",
                case.id,
                case.scenario,
                decision.reason,
                case.reason
            );
        }
    }

    #[test]
    fn deny_beats_specific_allow() {
        let snap = snapshot(
            Profile::Balanced,
            SandboxMode::Workspace,
            true,
            vec![
                rule("deny-write", Effect::Deny, "write_file", None, None),
                rule("allow-src", Effect::Allow, "write_file", Some("src/**"), None),
            ],
        );
        let decision = evaluate(
            &intent("write_file", ToolClass::Write, vec!["src/main.rs"], None),
            &snap,
        );
        assert_eq!(decision.effect, Effect::Deny);
        assert_eq!(decision.rule_id.as_deref(), Some("deny-write"));
    }

    #[test]
    fn argv_prefix_matches_whole_elements_only() {
        let snap = snapshot(
            Profile::Balanced,
            SandboxMode::Workspace,
            true,
            vec![rule(
                "allow-git-status",
                Effect::Allow,
                "exec",
                None,
                Some(vec!["git", "status"]),
            )],
        );
        let allowed = evaluate(
            &intent("exec", ToolClass::Exec, Vec::new(), Some(vec!["git", "status"])),
            &snap,
        );
        assert_eq!(allowed.effect, Effect::Allow);
        let not_allowed = evaluate(
            &intent(
                "exec",
                ToolClass::Exec,
                Vec::new(),
                Some(vec!["git", "status; rm -rf /"]),
            ),
            &snap,
        );
        assert_eq!(not_allowed.effect, Effect::Ask);
    }

    #[test]
    fn ask_rule_without_argv_never_matches() {
        let snap = snapshot(
            Profile::Unrestricted,
            SandboxMode::Off,
            true,
            vec![rule("ask-git", Effect::Ask, "exec", None, Some(vec!["git"]))],
        );
        let decision = evaluate(
            &intent("exec", ToolClass::Exec, Vec::new(), None),
            &snap,
        );
        assert_eq!(decision.effect, Effect::Allow);
    }
}
