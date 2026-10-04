//! Operational modes: `inspect`, `plan`, `build`, `parallel`, `headless`.
//!
//! A mode is a *constraint*, never a grant: it can deny a tool class, raise an
//! approval floor, or change output shape, and the effective decision is the
//! stricter of mode and policy. Modes cannot satisfy an ask, widen a sandbox or
//! expand a budget. `headless` is an output/decision-rights modifier composed
//! with another mode by the CLI.

use std::collections::{BTreeMap, BTreeSet};

use bollo_protocol::vocab::{Effect, EffectClass, ModeKind, ToolClass};
use bollo_policy::PolicySnapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputShape {
    Interactive,
    Ndjson,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModeDescriptor {
    pub kind: ModeKind,
    pub allowed_classes: BTreeSet<ToolClass>,
    /// Minimum effect per effect class (a floor: never lowers policy).
    pub class_floors: BTreeMap<EffectClass, Effect>,
    pub parallel_children: u32,
    pub output: OutputShape,
}

impl ModeDescriptor {
    pub fn allows(&self, class: ToolClass) -> bool {
        self.allowed_classes.contains(&class)
    }

    pub fn description(&self) -> &'static str {
        match self.kind {
            ModeKind::Inspect => "read-only inspection; mutation, exec and MCP are denied",
            ModeKind::Plan => "read-only planning; produce a plan, never mutate",
            ModeKind::Build => "full build mode under the permission profile",
            ModeKind::Parallel => "read-only children in parallel; no mutation (MVP)",
            ModeKind::Headless => "machine interface; asks block instead of prompting",
        }
    }
}

fn all_classes() -> BTreeSet<ToolClass> {
    [
        ToolClass::Read,
        ToolClass::Search,
        ToolClass::Write,
        ToolClass::Exec,
        ToolClass::Git,
        ToolClass::Mcp,
    ]
    .into_iter()
    .collect()
}

fn read_classes() -> BTreeSet<ToolClass> {
    [ToolClass::Read, ToolClass::Search, ToolClass::Git]
        .into_iter()
        .collect()
}

fn no_floors() -> BTreeMap<EffectClass, Effect> {
    BTreeMap::new()
}

/// The static descriptor for a mode.
pub fn descriptor(kind: ModeKind) -> ModeDescriptor {
    match kind {
        ModeKind::Inspect => ModeDescriptor {
            kind,
            allowed_classes: read_classes(),
            class_floors: no_floors(),
            parallel_children: 0,
            output: OutputShape::Interactive,
        },
        ModeKind::Plan => ModeDescriptor {
            kind,
            allowed_classes: read_classes(),
            class_floors: no_floors(),
            parallel_children: 0,
            output: OutputShape::Interactive,
        },
        ModeKind::Build => ModeDescriptor {
            kind,
            allowed_classes: all_classes(),
            class_floors: no_floors(),
            parallel_children: 0,
            output: OutputShape::Interactive,
        },
        ModeKind::Parallel => ModeDescriptor {
            kind,
            allowed_classes: read_classes(),
            class_floors: no_floors(),
            parallel_children: 4,
            output: OutputShape::Ndjson,
        },
        ModeKind::Headless => ModeDescriptor {
            kind,
            allowed_classes: all_classes(),
            class_floors: no_floors(),
            parallel_children: 0,
            output: OutputShape::Ndjson,
        },
    }
}

/// Effective mode for a policy snapshot. Today this only records the snapshot
/// revision; the API exists so floors can later derive from sandbox state.
pub fn effective(kind: ModeKind, snapshot: &PolicySnapshot) -> ModeDescriptor {
    let mut descriptor = descriptor(kind);
    // workspace_auto already refuses without verified isolation at startup;
    // here we only make sure a mode can never relax that posture.
    let _ = snapshot;
    descriptor.class_floors.retain(|_, effect| *effect != Effect::Allow);
    descriptor
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstrainedDecision {
    pub effect: Effect,
    /// True when the mode itself (not policy) denied the action.
    pub mode_denied: bool,
    pub reason: Option<String>,
}

/// Apply mode constraints to a policy effect.
pub fn constrain(
    descriptor: &ModeDescriptor,
    class: ToolClass,
    effect_class: EffectClass,
    policy_effect: Effect,
) -> ConstrainedDecision {
    if !descriptor.allows(class) {
        return ConstrainedDecision {
            effect: Effect::Deny,
            mode_denied: true,
            reason: Some(format!(
                "mode {} does not allow {} tools",
                descriptor.kind.as_str(),
                class.as_str()
            )),
        };
    }
    let floor = descriptor
        .class_floors
        .get(&effect_class)
        .copied()
        .unwrap_or(Effect::Allow);
    let effect = policy_effect.strictest(floor);
    ConstrainedDecision {
        effect,
        mode_denied: false,
        reason: if effect != policy_effect {
            Some(format!(
                "mode {} raised {} to {}",
                descriptor.kind.as_str(),
                policy_effect.as_str(),
                effect.as_str()
            ))
        } else {
            None
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollo_policy::layers::{CliOverrides, PolicySnapshot, Source};

    fn snapshot() -> PolicySnapshot {
        let user = bollo_policy::config::parse_user_config(include_str!(
            "../../../docs/examples/config.balanced.json"
        ))
        .unwrap();
        bollo_policy::layers::build_snapshot(
            &user,
            None,
            &CliOverrides::default(),
            bollo_protocol::vocab::SandboxCapabilities {
                platform: bollo_protocol::vocab::Platform::Linux,
                filesystem_containment: true,
                network_denied: true,
                backend: "test".into(),
            },
            1,
        )
        .unwrap()
    }

    #[test]
    fn inspect_denies_write_exec_and_mcp_by_class() {
        let mode = descriptor(ModeKind::Inspect);
        for class in [ToolClass::Write, ToolClass::Exec, ToolClass::Mcp] {
            let decision = constrain(&mode, class, EffectClass::Execution, Effect::Allow);
            assert_eq!(decision.effect, Effect::Deny);
            assert!(decision.mode_denied);
        }
        let read = constrain(&mode, ToolClass::Read, EffectClass::Read, Effect::Allow);
        assert_eq!(read.effect, Effect::Allow);
    }

    #[test]
    fn build_mode_preserves_policy_including_ask() {
        let mode = descriptor(ModeKind::Build);
        let decision = constrain(&mode, ToolClass::Write, EffectClass::WorkspaceMutation, Effect::Ask);
        assert_eq!(decision.effect, Effect::Ask);
        assert!(!decision.mode_denied);
        assert!(decision.reason.is_none());
    }

    #[test]
    fn parallel_mode_is_read_only_with_children() {
        let mode = descriptor(ModeKind::Parallel);
        assert_eq!(mode.parallel_children, 4);
        assert_eq!(mode.output, OutputShape::Ndjson);
        assert_eq!(
            constrain(&mode, ToolClass::Exec, EffectClass::Execution, Effect::Allow).effect,
            Effect::Deny
        );
    }

    #[test]
    fn headless_changes_output_not_ceilings() {
        let mode = descriptor(ModeKind::Headless);
        assert_eq!(mode.output, OutputShape::Ndjson);
        assert!(mode.allows(ToolClass::Write));
    }

    #[test]
    fn floors_never_lower_policy_effects() {
        let mut mode = descriptor(ModeKind::Build);
        mode.class_floors
            .insert(EffectClass::WorkspaceMutation, Effect::Ask);
        assert_eq!(
            constrain(&mode, ToolClass::Write, EffectClass::WorkspaceMutation, Effect::Deny).effect,
            Effect::Deny
        );
        assert_eq!(
            constrain(&mode, ToolClass::Write, EffectClass::WorkspaceMutation, Effect::Allow).effect,
            Effect::Ask
        );
        let effective = effective(ModeKind::Build, &snapshot());
        assert_eq!(effective.kind, ModeKind::Build);
        assert!(effective.class_floors.is_empty());
        let _ = Source::Default;
    }
}
