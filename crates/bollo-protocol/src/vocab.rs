//! Shared vocabulary. These enums are the cross-crate language for policy,
//! modes, events and the runtime; they intentionally live in `protocol` so no
//! crate needs to depend on another just to name a concept.

use serde::{Deserialize, Serialize};

/// Permission preset (docs/security/permissions.md). Changing it is a trusted
/// user action, never a project-config or model action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    ReadOnly,
    Balanced,
    WorkspaceAuto,
    Unrestricted,
}

impl Profile {
    pub fn as_str(self) -> &'static str {
        match self {
            Profile::ReadOnly => "read_only",
            Profile::Balanced => "balanced",
            Profile::WorkspaceAuto => "workspace_auto",
            Profile::Unrestricted => "unrestricted",
        }
    }

    /// `read_only` is an inspection ceiling: no allow rule can unlock mutation
    /// or exec above it.
    pub fn is_inspection_ceiling(self) -> bool {
        matches!(self, Profile::ReadOnly)
    }
}

/// Policy effect: deny > ask > allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effect {
    Allow,
    Ask,
    Deny,
}

impl Effect {
    pub fn as_str(self) -> &'static str {
        match self {
            Effect::Allow => "allow",
            Effect::Ask => "ask",
            Effect::Deny => "deny",
        }
    }

    /// Return the stricter of two effects (used by the mode layer).
    pub fn strictest(self, other: Effect) -> Effect {
        use Effect::*;
        match (self, other) {
            (Deny, _) | (_, Deny) => Deny,
            (Ask, _) | (_, Ask) => Ask,
            (Allow, Allow) => Allow,
        }
    }
}

/// Tool classes used by modes and policy matchers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolClass {
    Read,
    Search,
    Write,
    Exec,
    Git,
    Mcp,
}

impl ToolClass {
    pub fn as_str(self) -> &'static str {
        match self {
            ToolClass::Read => "read",
            ToolClass::Search => "search",
            ToolClass::Write => "write",
            ToolClass::Exec => "exec",
            ToolClass::Git => "git",
            ToolClass::Mcp => "mcp",
        }
    }
}

/// Normalized effect category of an intent. MCP is conservatively external.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectClass {
    Read,
    WorkspaceMutation,
    ExternalMutation,
    OutsideWorkspace,
    Execution,
}

/// Execution isolation selection; independent of the permission profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SandboxMode {
    Workspace,
    Off,
}

/// Operational modes. Behavior lives in `bollo-modes`; the vocabulary is
/// shared here so client commands and events can name a mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModeKind {
    Inspect,
    Plan,
    Build,
    Parallel,
    Headless,
}

impl ModeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ModeKind::Inspect => "inspect",
            ModeKind::Plan => "plan",
            ModeKind::Build => "build",
            ModeKind::Parallel => "parallel",
            ModeKind::Headless => "headless",
        }
    }
}

/// Platform families for capability reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Linux,
    Macos,
    Windows,
    Other,
}

impl Platform {
    pub fn current() -> Platform {
        if cfg!(target_os = "linux") {
            Platform::Linux
        } else if cfg!(target_os = "macos") {
            Platform::Macos
        } else if cfg!(target_os = "windows") {
            Platform::Windows
        } else {
            Platform::Other
        }
    }
}

/// Result of probing the host execution boundary. Enforcement claims must be
/// backed by a real probe; absence of evidence is `false`, never optimistic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxCapabilities {
    pub platform: Platform,
    /// Filesystem containment covering shell/symlinks/rename, not just path filters.
    pub filesystem_containment: bool,
    /// Tool network denial actually enforced for child processes.
    pub network_denied: bool,
    /// Human-readable backend/version note, safe to display.
    pub backend: String,
}

impl SandboxCapabilities {
    /// Workspace-auto requires both enforcement properties.
    pub fn supports_workspace_auto(&self) -> bool {
        self.filesystem_containment && self.network_denied
    }
}

/// Durable tool result status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Succeeded,
    Failed,
    Denied,
    Unknown,
}

/// Verification outcome; "completed" never implies "passed".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    Passed,
    Failed,
    Skipped,
    Inconclusive,
}

/// Run lifecycle states (docs/architecture/runtime.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Queued,
    Running,
    WaitingApproval,
    Completed,
    Failed,
    Cancelled,
    Blocked,
    Interrupted,
}

/// Terminal subset permitted in `run.finished`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalState {
    Completed,
    Failed,
    Cancelled,
    Blocked,
    Interrupted,
}

impl RunState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            RunState::Completed
                | RunState::Failed
                | RunState::Cancelled
                | RunState::Blocked
                | RunState::Interrupted
        )
    }

    pub fn as_terminal(self) -> Option<TerminalState> {
        Some(match self {
            RunState::Completed => TerminalState::Completed,
            RunState::Failed => TerminalState::Failed,
            RunState::Cancelled => TerminalState::Cancelled,
            RunState::Blocked => TerminalState::Blocked,
            RunState::Interrupted => TerminalState::Interrupted,
            _ => return None,
        })
    }
}

/// Helper used by validators/tests to keep the enum exercised.
pub fn verification_status_matches(status: VerificationStatus) -> bool {
    matches!(
        status,
        VerificationStatus::Passed
            | VerificationStatus::Failed
            | VerificationStatus::Skipped
            | VerificationStatus::Inconclusive
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deny_dominates_and_ask_beats_allow() {
        assert_eq!(Effect::Allow.strictest(Effect::Deny), Effect::Deny);
        assert_eq!(Effect::Deny.strictest(Effect::Allow), Effect::Deny);
        assert_eq!(Effect::Ask.strictest(Effect::Allow), Effect::Ask);
        assert_eq!(Effect::Ask.strictest(Effect::Ask), Effect::Ask);
        assert_eq!(Effect::Allow.strictest(Effect::Allow), Effect::Allow);
    }

    #[test]
    fn workspace_auto_requires_both_guarantees() {
        let mut caps = SandboxCapabilities {
            platform: Platform::Linux,
            filesystem_containment: true,
            network_denied: true,
            backend: "probe".into(),
        };
        assert!(caps.supports_workspace_auto());
        caps.network_denied = false;
        assert!(!caps.supports_workspace_auto());
    }

    #[test]
    fn run_state_terminal_mapping() {
        assert!(!RunState::Running.is_terminal());
        assert!(RunState::Completed.is_terminal());
        assert_eq!(RunState::Interrupted.as_terminal(), Some(TerminalState::Interrupted));
    }
}
