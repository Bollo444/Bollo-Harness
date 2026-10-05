//! The bounded runtime: session/run orchestration, the per-step agent loop,
//! budgets, context assembly and compaction.
//!
//! Invariants this crate enforces:
//! - one side-effecting action at a time;
//! - the step order `validate → context → reserve → stream → validate →
//!   normalize → hooks → policy → mode → advisory classifier → approval →
//!   journal intent → execute → after-hook`; the classifier can only turn
//!   allow into ask and is absent (zero calls) by default;
//! - journal intent *before* effect; an inability to journal refuses the effect;
//! - cancellation reaches the provider stream, hooks, tools and approvals.

pub mod budget;
pub mod classifier_audit;
pub mod compaction;
pub mod context;
pub mod runtime;
pub mod scheduler;

pub use budget::{Budget, UsageTotals};
pub use classifier_audit::ClassifierAudit;
pub use compaction::{compact, estimate_tokens, CompactionOutcome, COMPACT_TRIGGER_PERCENT};
pub use context::{content_hash, discover_instructions, system_prompt, InstructionFile};
pub use runtime::{
    ApprovalChannel, ApprovalRequest, NoChannel, RunOutcome, Runtime, RuntimeConfig, ScriptedChannel,
};
pub use scheduler::{EffectGuard, RunScheduler};

use bollo_protocol::vocab::TerminalState;

/// CLI exit-code mapping (docs/reference/cli.md).
pub fn exit_code_for(state: TerminalState, reason: Option<&str>) -> i32 {
    match state {
        TerminalState::Completed => 0,
        TerminalState::Failed => match reason {
            Some("budget_exceeded") | Some("run_limit_exceeded") => 4,
            Some("operation_unknown") => 5,
            _ => 1,
        },
        TerminalState::Blocked => 3,
        TerminalState::Cancelled => 130,
        TerminalState::Interrupted => 5,
    }
}
