//! The run-execution boundary.
//!
//! `bollo-api` owns transport, auth and persistence; it does not own the agent
//! loop. The composition root (CLI) implements this trait with the real
//! runtime, so the API can never grow a second tool gate: a backend receives a
//! run that already passed the normal session/store path and publishes
//! progress on the same event bus the CLI uses.

use bollo_protocol::ids::{RunId, SessionId};
use bollo_protocol::vocab::RunState;

use crate::error::ApiError;
use crate::state::SharedState;

#[derive(Debug, Clone)]
pub struct BackendRun {
    pub session: SessionId,
    pub run: RunId,
    pub prompt: String,
}

/// Implemented by the composition root, injected through `ApiConfig`.
pub trait RunBackend: Send + Sync {
    /// Start executing a persisted run. Must return promptly (the API answers
    /// 202) and report progress through `state.append_event` / the event bus.
    fn start(&self, run: BackendRun, state: &SharedState) -> Result<(), ApiError>;

    /// Request cancellation; returns the state the run ended in. Cancelling a
    /// terminal run is not an error.
    fn cancel(&self, run: &RunId, state: &SharedState) -> Result<RunState, ApiError>;
}

/// Placeholder used when the API is embedded without a runtime bridge. It
/// refuses work loudly instead of silently doing nothing.
pub struct UnavailableBackend;

impl RunBackend for UnavailableBackend {
    fn start(&self, _run: BackendRun, _state: &SharedState) -> Result<(), ApiError> {
        Err(ApiError::internal(
            "no run backend is configured for this API daemon",
        ))
    }

    fn cancel(&self, _run: &RunId, _state: &SharedState) -> Result<RunState, ApiError> {
        Err(ApiError::internal(
            "no run backend is configured for this API daemon",
        ))
    }
}
