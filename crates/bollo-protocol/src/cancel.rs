//! One shared cancellation token for a run. SIGINT, TUI Ctrl-C and (P2) API
//! cancel all set the same token; providers, hooks, tools, approvals and child
//! process groups observe it.
//!
//! First signal requests orderly cancellation; a second requests immediate
//! termination (the caller decides how to escalate).

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct CancellationToken {
    state: Arc<AtomicU8>,
}

const RUNNING: u8 = 0;
const CANCELLING: u8 = 1;
const FORCED: u8 = 2;

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancellationToken {
    pub fn new() -> Self {
        Self {
            state: Arc::new(AtomicU8::new(RUNNING)),
        }
    }

    /// Request orderly cancellation. Returns `true` if this call performed the
    /// first transition (false when already cancelling).
    pub fn cancel(&self) -> bool {
        self.state
            .compare_exchange(RUNNING, CANCELLING, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// Escalate to immediate termination.
    pub fn force(&self) {
        self.state.store(FORCED, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.state.load(Ordering::SeqCst) != RUNNING
    }

    pub fn is_forced(&self) -> bool {
        self.state.load(Ordering::SeqCst) == FORCED
    }
}

/// Standalone flag (used for tests that want a plain boolean).
#[derive(Debug, Default)]
pub struct Flag(AtomicBool);

impl Flag {
    pub fn set(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn get(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_is_idempotent_and_shared() {
        let token = CancellationToken::new();
        let clone = token.clone();
        assert!(!clone.is_cancelled());
        assert!(token.cancel());
        assert!(!token.cancel());
        assert!(clone.is_cancelled());
        token.force();
        assert!(clone.is_forced());
    }

    #[test]
    fn flag_works() {
        let flag = Flag::default();
        assert!(!flag.get());
        flag.set();
        assert!(flag.get());
    }
}
