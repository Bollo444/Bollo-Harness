//! The execution gate. [`Authorization`] has a private field and is
//! constructed only by [`authorize`] in this module, so no crate can execute a
//! prepared action without a policy decision and, where required, a consumed
//! approval receipt.

use time::OffsetDateTime;

use bollo_protocol::errors::{BolloError, ErrorCode};
use bollo_protocol::ids::ApprovalId;
use bollo_protocol::vocab::Effect;

use crate::approval::{ApprovalExpectation, ApprovalOutcome, ApprovalStore};
use crate::evaluate::PolicyDecision;
use crate::normalize::{intent_hash, NormalizedIntent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    /// The policy allowed the action directly.
    PolicyAllow,
    /// An exact approval receipt was consumed for this dispatch.
    Approved,
}

/// A capability to execute one prepared action exactly as normalized.
#[derive(Debug, Clone)]
pub struct Authorization {
    intent_hash: String,
    policy_revision: u64,
    mode: AuthMode,
    approval_id: Option<ApprovalId>,
}

impl Authorization {
    pub fn mode(&self) -> AuthMode {
        self.mode
    }

    pub fn intent_hash(&self) -> &str {
        &self.intent_hash
    }

    pub fn policy_revision(&self) -> u64 {
        self.policy_revision
    }

    pub fn approval_id(&self) -> Option<&ApprovalId> {
        self.approval_id.as_ref()
    }
}

/// Authorize dispatch for a normalized intent.
///
/// - `Deny` → `policy_denied`.
/// - `Allow` → authorization token.
/// - `Ask` → consume the exact approved receipt; anything pending, expired,
///   stale or already consumed is refused (`approval_required` /
///   `approval_stale`), never silently allowed.
pub fn authorize(
    decision: &PolicyDecision,
    intent: &NormalizedIntent,
    approval_id: Option<&ApprovalId>,
    expect: &ApprovalExpectation,
    store: &mut dyn ApprovalStore,
    now: OffsetDateTime,
) -> Result<Authorization, BolloError> {
    let actual_hash = intent_hash(intent);
    if expect.intent_hash != actual_hash {
        return Err(BolloError::new(
            ErrorCode::ApprovalStale,
            "expectation does not match the normalized intent",
        ));
    }
    match decision.effect {
        Effect::Deny => Err(BolloError::new(ErrorCode::PolicyDenied, decision.explain())),
        Effect::Allow => Ok(Authorization {
            intent_hash: actual_hash,
            policy_revision: expect.policy_revision,
            mode: AuthMode::PolicyAllow,
            approval_id: None,
        }),
        Effect::Ask => {
            let Some(approval_id) = approval_id else {
                return Err(BolloError::new(
                    ErrorCode::ApprovalRequired,
                    decision.explain(),
                ));
            };
            match store.consume(approval_id, expect, now) {
                ApprovalOutcome::Authorized => Ok(Authorization {
                    intent_hash: actual_hash,
                    policy_revision: expect.policy_revision,
                    mode: AuthMode::Approved,
                    approval_id: Some(approval_id.clone()),
                }),
                ApprovalOutcome::Pending | ApprovalOutcome::NotFound => Err(BolloError::new(
                    ErrorCode::ApprovalRequired,
                    decision.explain(),
                )),
                ApprovalOutcome::Denied => Err(BolloError::new(
                    ErrorCode::PolicyDenied,
                    "approval was denied",
                )),
                ApprovalOutcome::Expired => Err(BolloError::new(
                    ErrorCode::ApprovalRequired,
                    "approval expired; a new request is required",
                )),
                ApprovalOutcome::Conflict => Err(BolloError::new(
                    ErrorCode::ApprovalStale,
                    "approval was already consumed",
                )),
                ApprovalOutcome::Stale(reason) => Err(BolloError::new(
                    ErrorCode::ApprovalStale,
                    format!("approval no longer matches: {reason}"),
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::{pending_receipt, ApprovalState, MemoryApprovalStore};
    use crate::evaluate::PolicyDecision;
    use crate::normalize::{effect_class_for, scoped_path};
    use bollo_protocol::ids::{RunId, SessionId, ToolCallId};
    use bollo_protocol::vocab::ToolClass;

    fn intent() -> NormalizedIntent {
        NormalizedIntent {
            tool: "write_file".into(),
            class: ToolClass::Write,
            effect: effect_class_for(ToolClass::Write, false),
            paths: vec![scoped_path(
                if cfg!(windows) { "C:/proj" } else { "/proj" },
                "src/main.rs",
            )
            .unwrap()],
            argv: None,
            cwd: None,
            environment_names: Vec::new(),
            summary: "write src/main.rs".into(),
            arguments_digest: "0".repeat(64),
        }
    }

    fn decision(effect: Effect) -> PolicyDecision {
        PolicyDecision {
            effect,
            rule_id: None,
            provenance: Vec::new(),
            reason: "test decision".into(),
        }
    }

    fn expectation() -> ApprovalExpectation {
        ApprovalExpectation::for_intent(&intent(), 1, "ws", None)
    }

    fn approved_store(now: OffsetDateTime) -> (MemoryApprovalStore, ApprovalId) {
        let receipt = pending_receipt(
            &intent(),
            SessionId::generate(),
            RunId::generate(),
            ToolCallId::generate(),
            "ws".into(),
            1,
            None,
            now,
            300,
        );
        let id = receipt.approval_id.clone();
        let mut store = MemoryApprovalStore::new();
        store.insert(receipt).unwrap();
        store.resolve(&id, ApprovalState::Approved, None, now);
        (store, id)
    }

    #[test]
    fn deny_never_returns_a_token() {
        let now = OffsetDateTime::now_utc();
        let mut store = MemoryApprovalStore::new();
        let err = authorize(
            &decision(Effect::Deny),
            &intent(),
            None,
            &expectation(),
            &mut store,
            now,
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::PolicyDenied);
    }

    #[test]
    fn allow_returns_a_token_without_approval() {
        let now = OffsetDateTime::now_utc();
        let mut store = MemoryApprovalStore::new();
        let auth = authorize(
            &decision(Effect::Allow),
            &intent(),
            None,
            &expectation(),
            &mut store,
            now,
        )
        .unwrap();
        assert_eq!(auth.mode(), AuthMode::PolicyAllow);
        assert_eq!(auth.policy_revision(), 1);
    }

    #[test]
    fn ask_without_receipt_requires_approval() {
        let now = OffsetDateTime::now_utc();
        let mut store = MemoryApprovalStore::new();
        let err = authorize(
            &decision(Effect::Ask),
            &intent(),
            None,
            &expectation(),
            &mut store,
            now,
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::ApprovalRequired);
    }

    #[test]
    fn pending_receipt_still_requires_approval() {
        let now = OffsetDateTime::now_utc();
        let receipt = pending_receipt(
            &intent(),
            SessionId::generate(),
            RunId::generate(),
            ToolCallId::generate(),
            "ws".into(),
            1,
            None,
            now,
            300,
        );
        let id = receipt.approval_id.clone();
        let mut store = MemoryApprovalStore::new();
        store.insert(receipt).unwrap();
        let err = authorize(
            &decision(Effect::Ask),
            &intent(),
            Some(&id),
            &expectation(),
            &mut store,
            now,
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::ApprovalRequired);
    }

    #[test]
    fn approved_receipt_authorizes_once() {
        let now = OffsetDateTime::now_utc();
        let (mut store, id) = approved_store(now);
        let auth = authorize(
            &decision(Effect::Ask),
            &intent(),
            Some(&id),
            &expectation(),
            &mut store,
            now,
        )
        .unwrap();
        assert_eq!(auth.mode(), AuthMode::Approved);
        assert_eq!(auth.approval_id(), Some(&id));

        // Second dispatch is refused: no double execution.
        let err = authorize(
            &decision(Effect::Ask),
            &intent(),
            Some(&id),
            &expectation(),
            &mut store,
            now,
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::ApprovalStale);
    }

    #[test]
    fn mismatched_expectation_is_stale() {
        let now = OffsetDateTime::now_utc();
        let (mut store, id) = approved_store(now);
        let mut expect = expectation();
        expect.policy_revision = 99;
        let err = authorize(
            &decision(Effect::Ask),
            &intent(),
            Some(&id),
            &expect,
            &mut store,
            now,
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::ApprovalStale);
    }
}
