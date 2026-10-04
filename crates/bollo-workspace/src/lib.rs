//! Workspace boundary: canonical root identity, handle-relative filesystem
//! operations, patch checkpoints, the child process broker and sandbox probes.
//!
//! This crate knows nothing about policy or credentials; it executes what the
//! upper layers have already authorized and reports observed facts.

pub mod checkpoints;
pub mod fs;
pub mod process;
pub mod root;
pub mod sandbox;

pub use checkpoints::{Checkpoint, CheckpointLog, RestorePlan};
pub use fs::{FilePatch, FileRead, FileWrite, WorkspaceFs};
pub use process::{run, ExecOutcome, ExecRequest, ExecStatus};
pub use root::WorkspaceRoot;
pub use sandbox::probe;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WsError {
    #[error("path outside workspace scope: {0}")]
    OutsideScope(String),
    #[error("invalid path: {0}")]
    InvalidPath(String),
    #[error("io: {0}")]
    Io(String),
    #[error("special file rejected: {0}")]
    SpecialFile(String),
    #[error("preimage conflict: {0}")]
    PreimageConflict(String),
    #[error("ambiguous match: {0}")]
    AmbiguousMatch(String),
    #[error("content too large: {0} bytes")]
    TooLarge(u64),
    #[error("binary content rejected")]
    BinaryContent,
    #[error("checkpoint not found: {0}")]
    CheckpointNotFound(String),
    #[error("checkpoint restore conflict: {0}")]
    Conflict(String),
}
