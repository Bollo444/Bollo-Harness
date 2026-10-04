//! Versioned event envelope and the closed v0.1 event catalog.
//!
//! Canonical schema: `docs/contracts/event.schema.json`. This module enforces
//! the same shape so no invalid event can be persisted or published: unknown
//! `data` fields, missing required-nullable fields, wrong enums and out-of-range
//! values all fail validation.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::errors::ProtocolError;
use crate::ids::{is_valid_opaque_id, ApprovalId, ArtifactId, EventId, RunId, SessionId, ToolCallId};
use crate::timeutil;
use crate::vocab::{
    Effect, Profile, SandboxMode, TerminalState, ToolStatus, VerificationStatus,
};

pub const SCHEMA_VERSION: &str = "0.1";

const MAX_TEXT: usize = 65_536;
const MAX_SUMMARY: usize = 8_192;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    RunStarted,
    AssistantDelta,
    ToolProposed,
    ToolResult,
    ApprovalRequested,
    ApprovalResolved,
    PolicyChanged,
    VerificationResult,
    UsageUpdated,
    RunFinished,
}

impl EventType {
    pub fn as_str(self) -> &'static str {
        match self {
            EventType::RunStarted => "run.started",
            EventType::AssistantDelta => "assistant.delta",
            EventType::ToolProposed => "tool.proposed",
            EventType::ToolResult => "tool.result",
            EventType::ApprovalRequested => "approval.requested",
            EventType::ApprovalResolved => "approval.resolved",
            EventType::PolicyChanged => "policy.changed",
            EventType::VerificationResult => "verification.result",
            EventType::UsageUpdated => "usage.updated",
            EventType::RunFinished => "run.finished",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventEnvelope {
    pub schema_version: String,
    pub event_id: EventId,
    pub session_id: SessionId,
    pub run_id: Option<RunId>,
    pub seq: u64,
    pub timestamp: String,
    #[serde(rename = "type")]
    pub event_type: EventType,
    pub data: Value,
}

impl EventEnvelope {
    /// Build and validate an envelope from typed data.
    pub fn new<T: Serialize>(
        session_id: &SessionId,
        run_id: Option<&RunId>,
        seq: u64,
        event_type: EventType,
        data: &T,
    ) -> Result<Self, ProtocolError> {
        let data = serde_json::to_value(data)
            .map_err(|err| ProtocolError::InvalidEvent(err.to_string()))?;
        let envelope = Self {
            schema_version: SCHEMA_VERSION.to_string(),
            event_id: EventId::generate(),
            session_id: session_id.clone(),
            run_id: run_id.cloned(),
            seq,
            timestamp: timeutil::now_rfc3339(),
            event_type,
            data,
        };
        envelope.validate()?;
        Ok(envelope)
    }

    /// Strict validation of every contract rule for this envelope.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(invalid(format!(
                "schema_version {:?} != {SCHEMA_VERSION:?}",
                self.schema_version
            )));
        }
        for (name, value) in [
            ("event_id", self.event_id.as_str()),
            ("session_id", self.session_id.as_str()),
        ] {
            if !is_valid_opaque_id(value) {
                return Err(invalid(format!("{name} is not a valid opaque id")));
            }
        }
        if let Some(run_id) = &self.run_id {
            if !is_valid_opaque_id(run_id.as_str()) {
                return Err(invalid("run_id is not a valid opaque id"));
            }
        }
        if self.seq < 1 {
            return Err(invalid("seq must be >= 1"));
        }
        timeutil::parse_rfc3339(&self.timestamp)?;
        // v0.1: run_id may be null only for policy.changed.
        if self.run_id.is_none() && self.event_type != EventType::PolicyChanged {
            return Err(invalid(format!(
                "run_id may be null only for policy.changed, not {}",
                self.event_type.as_str()
            )));
        }
        validate_data(self.event_type, &self.data)
    }
}

fn invalid(message: impl Into<String>) -> ProtocolError {
    ProtocolError::InvalidEvent(message.into())
}

fn typed<T: DeserializeOwned>(data: &Value) -> Result<T, ProtocolError> {
    serde_json::from_value(data.clone())
        .map_err(|err| invalid(format!("data shape mismatch: {err}")))
}

fn object(data: &Value) -> Result<&Map<String, Value>, ProtocolError> {
    data.as_object()
        .ok_or_else(|| invalid("data must be a JSON object"))
}

fn require_present(
    map: &Map<String, Value>,
    keys: &[&str],
) -> Result<(), ProtocolError> {
    for key in keys {
        if !map.contains_key(*key) {
            return Err(invalid(format!(
                "required field {key:?} is missing (required-nullable fields must be present)"
            )));
        }
    }
    Ok(())
}

fn validate_data(event_type: EventType, data: &Value) -> Result<(), ProtocolError> {
    let map = object(data)?;
    match event_type {
        EventType::RunStarted => {
            let d: RunStartedData = typed(data)?;
            if d.state != "running" {
                return Err(invalid("run.started state must be \"running\""));
            }
            if d.policy_revision < 1 {
                return Err(invalid("policy_revision must be >= 1"));
            }
            if !matches!(d.provider.as_str(), "anthropic" | "xai") {
                return Err(invalid("provider must be anthropic or xai"));
            }
            if d.model.is_empty() {
                return Err(invalid("model must not be empty"));
            }
            Ok(())
        }
        EventType::AssistantDelta => {
            let d: AssistantDeltaData = typed(data)?;
            if d.text.len() > MAX_TEXT {
                return Err(invalid(format!("text exceeds {MAX_TEXT} bytes")));
            }
            Ok(())
        }
        EventType::ToolProposed => {
            let d: ToolProposedData = typed(data)?;
            if d.tool_name.is_empty() {
                return Err(invalid("tool_name must not be empty"));
            }
            if !is_sha256(&d.intent_hash) {
                return Err(invalid("intent_hash must be 64 lowercase hex characters"));
            }
            if d.reason.is_empty() {
                return Err(invalid("reason must not be empty"));
            }
            Ok(())
        }
        EventType::ToolResult => {
            require_present(map, &["artifact_id"])?;
            let d: ToolResultData = typed(data)?;
            if d.summary.len() > MAX_SUMMARY {
                return Err(invalid(format!("summary exceeds {MAX_SUMMARY} bytes")));
            }
            Ok(())
        }
        EventType::ApprovalRequested => {
            let d: ApprovalRequestedData = typed(data)?;
            if !is_sha256(&d.intent_hash) {
                return Err(invalid("intent_hash must be 64 lowercase hex characters"));
            }
            if d.policy_revision < 1 {
                return Err(invalid("policy_revision must be >= 1"));
            }
            timeutil::parse_rfc3339(&d.expires_at)?;
            if d.summary.len() > MAX_SUMMARY {
                return Err(invalid(format!("summary exceeds {MAX_SUMMARY} bytes")));
            }
            Ok(())
        }
        EventType::ApprovalResolved => {
            let _d: ApprovalResolvedData = typed(data)?;
            Ok(())
        }
        EventType::PolicyChanged => {
            let d: PolicyChangedData = typed(data)?;
            if d.policy_revision < 1 {
                return Err(invalid("policy_revision must be >= 1"));
            }
            let _profile: Profile = d.profile;
            let _sandbox: SandboxMode = d.sandbox;
            Ok(())
        }
        EventType::VerificationResult => {
            require_present(map, &["command", "exit_code", "artifact_id"])?;
            let d: VerificationResultData = typed(data)?;
            if let Some(command) = &d.command {
                if command.is_empty() {
                    return Err(invalid("command must be null or non-empty"));
                }
            }
            Ok(())
        }
        EventType::UsageUpdated => {
            let d: UsageUpdatedData = typed(data)?;
            match (d.cost_known, d.cost_microusd) {
                (false, Some(_)) => {
                    return Err(invalid("cost_microusd must be null when cost_known is false"))
                }
                (true, None) => {
                    return Err(invalid("cost_microusd must be an integer when cost_known is true"))
                }
                _ => {}
            }
            Ok(())
        }
        EventType::RunFinished => {
            require_present(map, &["reason"])?;
            let d: RunFinishedData = typed(data)?;
            if let Some(reason) = &d.reason {
                if reason.is_empty() {
                    return Err(invalid("reason must be null or non-empty"));
                }
            }
            Ok(())
        }
    }
}

pub fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

// ---------------------------------------------------------------------------
// Typed data payloads (closed shapes)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunStartedData {
    pub state: String,
    pub policy_revision: u64,
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssistantDeltaData {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolProposedData {
    pub tool_call_id: ToolCallId,
    pub tool_name: String,
    pub intent_hash: String,
    pub decision: Effect,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResultData {
    pub tool_call_id: ToolCallId,
    pub status: ToolStatus,
    pub artifact_id: Option<ArtifactId>,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRequestedData {
    pub approval_id: ApprovalId,
    pub tool_call_id: ToolCallId,
    pub intent_hash: String,
    pub policy_revision: u64,
    pub expires_at: String,
    pub summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalOutcome {
    Approve,
    Deny,
    Expired,
    Invalidated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalResolvedData {
    pub approval_id: ApprovalId,
    pub decision: ApprovalOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyChangedData {
    pub policy_revision: u64,
    pub profile: Profile,
    pub sandbox: SandboxMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationResultData {
    pub status: VerificationStatus,
    pub command: Option<Vec<String>>,
    pub exit_code: Option<i64>,
    pub artifact_id: Option<ArtifactId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageUpdatedData {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_microusd: Option<u64>,
    pub cost_known: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunFinishedData {
    pub state: TerminalState,
    pub reason: Option<String>,
    pub verification: VerificationStatus,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> SessionId {
        SessionId::generate()
    }

    fn run() -> RunId {
        RunId::generate()
    }

    #[test]
    fn constructs_valid_run_started() {
        let session = session();
        let run = run();
        let event = EventEnvelope::new(
            &session,
            Some(&run),
            1,
            EventType::RunStarted,
            &RunStartedData {
                state: "running".into(),
                policy_revision: 1,
                provider: "anthropic".into(),
                model: "claude-sonnet-4-5".into(),
            },
        )
        .unwrap();
        assert_eq!(event.event_type, EventType::RunStarted);
        assert_eq!(event.data["state"], "running");
    }

    #[test]
    fn rejects_null_run_for_non_policy_events() {
        let session = session();
        let event = EventEnvelope::new(
            &session,
            None,
            1,
            EventType::AssistantDelta,
            &AssistantDeltaData { text: "x".into() },
        );
        assert!(event.is_err());
    }

    #[test]
    fn policy_changed_may_have_null_run() {
        let session = session();
        let event = EventEnvelope::new(
            &session,
            None,
            2,
            EventType::PolicyChanged,
            &PolicyChangedData {
                policy_revision: 3,
                profile: Profile::Balanced,
                sandbox: SandboxMode::Workspace,
            },
        )
        .unwrap();
        assert!(event.run_id.is_none());
    }

    #[test]
    fn usage_requires_cost_consistency() {
        let session = session();
        let run = run();
        let bad = EventEnvelope::new(
            &session,
            Some(&run),
            3,
            EventType::UsageUpdated,
            &UsageUpdatedData {
                input_tokens: Some(10),
                output_tokens: Some(5),
                cost_microusd: Some(42),
                cost_known: false,
            },
        );
        assert!(bad.is_err());
        let good = EventEnvelope::new(
            &session,
            Some(&run),
            4,
            EventType::UsageUpdated,
            &UsageUpdatedData {
                input_tokens: None,
                output_tokens: None,
                cost_microusd: None,
                cost_known: false,
            },
        );
        assert!(good.is_ok());
    }

    #[test]
    fn required_nullable_fields_must_be_present() {
        let session = session();
        let run = run();
        let mut event = EventEnvelope::new(
            &session,
            Some(&run),
            5,
            EventType::RunFinished,
            &RunFinishedData {
                state: TerminalState::Completed,
                reason: None,
                verification: VerificationStatus::Skipped,
            },
        )
        .unwrap();
        // Removing the key (as opposed to nulling it) is a contract violation.
        event.data.as_object_mut().unwrap().remove("reason");
        assert!(event.validate().is_err());
    }

    #[test]
    fn unknown_data_fields_are_rejected() {
        let session = session();
        let run = run();
        let mut event = EventEnvelope::new(
            &session,
            Some(&run),
            6,
            EventType::AssistantDelta,
            &AssistantDeltaData { text: "ok".into() },
        )
        .unwrap();
        event
            .data
            .as_object_mut()
            .unwrap()
            .insert("extra".into(), Value::Bool(true));
        assert!(event.validate().is_err());
    }

    #[test]
    fn tool_proposed_requires_real_sha256() {
        let session = session();
        let run = run();
        let bad = EventEnvelope::new(
            &session,
            Some(&run),
            7,
            EventType::ToolProposed,
            &ToolProposedData {
                tool_call_id: ToolCallId::generate(),
                tool_name: "read_file".into(),
                intent_hash: "NOT-A-HASH".into(),
                decision: Effect::Allow,
                reason: "allowed by profile".into(),
            },
        );
        assert!(bad.is_err());
        let good = EventEnvelope::new(
            &session,
            Some(&run),
            8,
            EventType::ToolProposed,
            &ToolProposedData {
                tool_call_id: ToolCallId::generate(),
                tool_name: "read_file".into(),
                intent_hash: "a".repeat(64),
                decision: Effect::Allow,
                reason: "allowed by profile".into(),
            },
        );
        assert!(good.is_ok());
    }

    #[test]
    fn roundtrip_through_json_keeps_envelope() {
        let session = session();
        let run = run();
        let event = EventEnvelope::new(
            &session,
            Some(&run),
            9,
            EventType::VerificationResult,
            &VerificationResultData {
                status: VerificationStatus::Failed,
                command: Some(vec!["cargo".into(), "test".into()]),
                exit_code: Some(101),
                artifact_id: None,
            },
        )
        .unwrap();
        let json = serde_json::to_string(&event).unwrap();
        let back: EventEnvelope = serde_json::from_str(&json).unwrap();
        back.validate().unwrap();
        assert_eq!(back, event);
    }
}
