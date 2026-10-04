//! Client → runtime DTOs. The API (P2) and TUI reuse these; they never carry
//! secrets and never bypass the tool gate (there is no "execute arbitrary tool"
//! command).

use serde::{Deserialize, Serialize};

use crate::ids::{ApprovalId, SessionId};
use crate::vocab::ModeKind;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "command")]
pub enum ClientCommand {
    StartRun {
        session_id: SessionId,
        prompt: String,
        mode: ModeKind,
    },
    ResolveApproval {
        session_id: SessionId,
        approval_id: ApprovalId,
        decision: ApprovalDecision,
        note: Option<String>,
    },
    Cancel {
        session_id: SessionId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalDecision {
    Approve,
    Deny,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_roundtrip() {
        let command = ClientCommand::StartRun {
            session_id: SessionId::generate(),
            prompt: "fix the parser".into(),
            mode: ModeKind::Build,
        };
        let json = serde_json::to_string(&command).unwrap();
        let back: ClientCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(command, back);
        assert!(json.contains("\"command\":\"start_run\""));
    }

    #[test]
    fn approval_resolution_roundtrips() {
        let command = ClientCommand::ResolveApproval {
            session_id: SessionId::generate(),
            approval_id: ApprovalId::generate(),
            decision: ApprovalDecision::Deny,
            note: Some("targets a path outside the workspace".into()),
        };
        let json = serde_json::to_string(&command).unwrap();
        let back: ClientCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(command, back);
    }
}
