//! Pure, deterministic policy: configuration layering, intent normalization,
//! the allow/ask/deny decision and scoped approval receipts.
//!
//! This crate performs no I/O and never asks the user anything. It depends only
//! on `bollo-protocol`. The non-forgeable [`gate::Authorization`] token is the
//! only way for the execution layer to run an approved action, which is how the
//! "no tool executes without the gate" invariant is enforced structurally
//! rather than by convention.

pub mod approval;
pub mod classifier;
pub mod config;
pub mod evaluate;
pub mod gate;
pub mod layers;
pub mod normalize;

pub use approval::{
    pending_receipt, ApprovalExpectation, ApprovalOutcome, ApprovalReceipt, ApprovalState,
    ApprovalStore, MemoryApprovalStore, ResolveOutcome, DEFAULT_APPROVAL_TTL_SECONDS,
};
pub use classifier::{
    escalate, Availability, ClassifierGate, ClassifierState, ClassifierVerdict, Escalation,
    EscalationThresholds, RiskClassifier, ScriptedClassifier,
};
pub use config::{
    parse_project_config, parse_user_config, BolloConfig, ClassifierConfig, ProjectConfig,
};
pub use evaluate::{evaluate, PolicyDecision};
pub use gate::Authorization;
pub use layers::{build_snapshot, validate_startup, CliOverrides, PolicySnapshot, Source};
pub use normalize::{intent_hash, NormalizedIntent, ScopedPath};

/// Typed failures for configuration, normalization, startup and approvals.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PolicyError {
    #[error("config invalid: {0}")]
    Config(String),
    #[error("startup refused: {0}")]
    Startup(String),
    #[error("duplicate rule id: {0}")]
    DuplicateRule(String),
    #[error("project config may not {0}")]
    ProjectWidening(String),
    #[error("path invalid: {0}")]
    Path(String),
}
