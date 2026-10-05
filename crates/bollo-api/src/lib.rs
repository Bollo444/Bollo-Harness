//! Optional local HTTP API (BH-017, P2).
//!
//! A loopback-only facade over the same runtime state the CLI uses: every
//! route requires a bearer token bound to the daemon workspace, mutations are
//! capability-checked, creations are idempotent, and session events stream as
//! SSE with replay from the durable journal.
//!
//! The server never binds a non-loopback address: remote exposure is a P2
//! design item that requires TLS and an origin allowlist review, so this
//! build refuses it instead of pretending.
//!
//! Contract: `docs/contracts/openapi.json`; behavior: `docs/reference/api.md`.

pub mod auth;
pub mod backend;
pub mod dto;
pub mod error;
pub mod events;
pub mod idempotency;
pub mod server;
pub mod state;
mod wire;

pub use auth::{ApiToken, Capability};
pub use backend::{BackendRun, RunBackend};
pub use error::ApiError;
pub use server::{ApiServer, RunningApi};
pub use state::{
    ApiConfig, ApiLimits, ApiState, PolicySnapshot, ProvenanceView, RuleView, SharedState,
};
