//! Bollo shared protocol: opaque identifiers, event envelopes, client commands,
//! stable error codes and the NDJSON codec.
//!
//! This crate has no runtime dependencies and performs no I/O beyond the
//! caller-supplied writers/readers in [`ndjson`]. Every type here is part of a
//! public contract (docs/contracts/*.json); treat changes as breaking unless
//! additive and schema-versioned.

pub mod cancel;
pub mod commands;
pub mod errors;
pub mod events;
pub mod ids;
pub mod ndjson;
pub mod timeutil;
pub mod vocab;

pub use errors::{ErrorCode, ProtocolError};
pub use events::{EventEnvelope, EventType};
pub use ids::{ApprovalId, ArtifactId, EventId, RunId, SessionId, ToolCallId};
pub use vocab::{Effect, ModeKind, SandboxCapabilities, SandboxMode, ToolClass};
