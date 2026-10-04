//! Single-action scheduling. The runtime performs at most one side-effecting
//! action at a time; this module owns that invariant explicitly instead of
//! leaving it implicit in the loop shape, and it is the place where the
//! cancellation token is consulted before each step.
//!
//! Cancellation reaches provider streams, hooks, tools, approvals and child
//! process groups: the guard here covers the local slot, while the token itself
//! is shared with those subsystems.

use std::cell::Cell;

use bollo_protocol::cancel::CancellationToken;
use bollo_protocol::errors::{BolloError, ErrorCode};

/// Tracks the run's in-flight effect slot and cancellation state.
pub struct RunScheduler {
    cancel: CancellationToken,
    in_flight: Cell<bool>,
}

impl RunScheduler {
    pub fn new(cancel: CancellationToken) -> Self {
        Self {
            cancel,
            in_flight: Cell::new(false),
        }
    }

    pub fn cancel_token(&self) -> &CancellationToken {
        &self.cancel
    }

    /// Refuse to start a step once cancellation was requested.
    pub fn ensure_running(&self) -> Result<(), BolloError> {
        if self.cancel.is_cancelled() {
            return Err(BolloError::new(ErrorCode::Cancelled, "run cancelled"));
        }
        Ok(())
    }

    /// Claim the single side-effect slot. Dropping the guard releases it; a
    /// second claim while one is held is a programming error turned into a
    /// refusal rather than a race.
    pub fn begin_effect(&self) -> Result<EffectGuard<'_>, BolloError> {
        self.ensure_running()?;
        if self.in_flight.replace(true) {
            return Err(BolloError::new(
                ErrorCode::Internal,
                "a side-effecting action is already in flight",
            ));
        }
        Ok(EffectGuard { slot: &self.in_flight })
    }
}

/// Releases the effect slot on drop, including on the error paths.
pub struct EffectGuard<'a> {
    slot: &'a Cell<bool>,
}

impl Drop for EffectGuard<'_> {
    fn drop(&mut self) {
        self.slot.set(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_effect_at_a_time() {
        let scheduler = RunScheduler::new(CancellationToken::new());
        let first = scheduler.begin_effect().unwrap();
        assert!(scheduler.begin_effect().is_err());
        drop(first);
        assert!(scheduler.begin_effect().is_ok());
    }

    #[test]
    fn cancellation_is_checked_before_each_step() {
        let cancel = CancellationToken::new();
        let scheduler = RunScheduler::new(cancel.clone());
        scheduler.ensure_running().unwrap();
        cancel.cancel();
        let err = scheduler.ensure_running().unwrap_err();
        assert_eq!(err.code, ErrorCode::Cancelled);
        assert!(scheduler.begin_effect().is_err());
    }
}
