//! Scoped approval receipts: exact intent, single use, expiring, compare-and-set.
//!
//! A receipt can never be reused, can never authorize a changed argument,
//! policy revision, workspace or target preimage, and a second response to a
//! resolved receipt is a conflict even when it repeats the original decision.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use bollo_protocol::ids::{ApprovalId, RunId, SessionId, ToolCallId};
use bollo_protocol::timeutil;

use crate::normalize::{intent_hash, NormalizedIntent};

/// Default approval lifetime in seconds (docs: 300 default).
pub const DEFAULT_APPROVAL_TTL_SECONDS: i64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalState {
    Pending,
    Approved,
    Denied,
    Expired,
    Invalidated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalReceipt {
    pub approval_id: ApprovalId,
    pub session_id: SessionId,
    pub run_id: RunId,
    pub tool_call_id: ToolCallId,
    pub tool_name: String,
    pub intent_hash: String,
    pub workspace_identity: String,
    pub policy_revision: u64,
    pub target_preimage: Option<String>,
    pub state: ApprovalState,
    pub expires_at: String,
    pub consumed_at: Option<String>,
    pub note: Option<String>,
    pub summary: String,
}

/// What the dispatch site must still match at consumption time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalExpectation {
    pub intent_hash: String,
    pub policy_revision: u64,
    pub workspace_identity: String,
    pub target_preimage: Option<String>,
}

impl ApprovalExpectation {
    pub fn for_intent(
        intent: &NormalizedIntent,
        policy_revision: u64,
        workspace_identity: &str,
        target_preimage: Option<String>,
    ) -> Self {
        Self {
            intent_hash: intent_hash(intent),
            policy_revision,
            workspace_identity: workspace_identity.to_string(),
            target_preimage,
        }
    }
}

/// Result of consuming a receipt (dispatch site).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalOutcome {
    Authorized,
    Pending,
    Denied,
    Expired,
    NotFound,
    Conflict,
    Stale(String),
}

/// Result of a user decision (interactive site).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveOutcome {
    Approved,
    Denied,
    Expired,
    Conflict,
    NotFound,
}

pub trait ApprovalStore {
    fn insert(&mut self, receipt: ApprovalReceipt) -> Result<(), String>;
    fn get(&self, approval_id: &ApprovalId) -> Option<&ApprovalReceipt>;
    /// Record a user decision. Only a pending, unexpired receipt can resolve;
    /// anything else is a conflict.
    fn resolve(
        &mut self,
        approval_id: &ApprovalId,
        decision: ApprovalState,
        note: Option<String>,
        now: OffsetDateTime,
    ) -> ResolveOutcome;
    /// Atomically consume an approved receipt for an exact expectation.
    fn consume(
        &mut self,
        approval_id: &ApprovalId,
        expect: &ApprovalExpectation,
        now: OffsetDateTime,
    ) -> ApprovalOutcome;
    /// Invalidate all pending approvals for a run (policy change, failure,
    /// cancellation). Returns how many were invalidated.
    fn invalidate_for_run(&mut self, run_id: &RunId) -> usize;
}

/// Build a pending receipt for an ask decision. The caller must persist it,
/// publish `approval.requested`, and wait for a user decision.
pub fn pending_receipt(
    intent: &NormalizedIntent,
    session_id: SessionId,
    run_id: RunId,
    tool_call_id: ToolCallId,
    workspace_identity: String,
    policy_revision: u64,
    target_preimage: Option<String>,
    now: OffsetDateTime,
    ttl_seconds: i64,
) -> ApprovalReceipt {
    ApprovalReceipt {
        approval_id: ApprovalId::generate(),
        session_id,
        run_id,
        tool_call_id,
        tool_name: intent.tool.clone(),
        intent_hash: intent_hash(intent),
        workspace_identity,
        policy_revision,
        target_preimage,
        state: ApprovalState::Pending,
        expires_at: timeutil::add_seconds(now, ttl_seconds).format(&time::format_description::well_known::Rfc3339).expect("RFC3339"),
        consumed_at: None,
        note: None,
        summary: intent.summary.clone(),
    }
}

#[derive(Debug, Default)]
pub struct MemoryApprovalStore {
    receipts: BTreeMap<String, ApprovalReceipt>,
}

impl MemoryApprovalStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.receipts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.receipts.is_empty()
    }
}

fn is_expired(receipt: &ApprovalReceipt, now: OffsetDateTime) -> bool {
    match timeutil::parse_rfc3339(&receipt.expires_at) {
        Ok(expires) => now >= expires,
        Err(_) => true, // unparseable expiry is not a valid authorization
    }
}

impl ApprovalStore for MemoryApprovalStore {
    fn insert(&mut self, receipt: ApprovalReceipt) -> Result<(), String> {
        self.receipts
            .insert(receipt.approval_id.as_str().to_string(), receipt);
        Ok(())
    }

    fn get(&self, approval_id: &ApprovalId) -> Option<&ApprovalReceipt> {
        self.receipts.get(approval_id.as_str())
    }

    fn resolve(
        &mut self,
        approval_id: &ApprovalId,
        decision: ApprovalState,
        note: Option<String>,
        now: OffsetDateTime,
    ) -> ResolveOutcome {
        let Some(receipt) = self.receipts.get_mut(approval_id.as_str()) else {
            return ResolveOutcome::NotFound;
        };
        if receipt.consumed_at.is_some() {
            return ResolveOutcome::Conflict;
        }
        if receipt.state != ApprovalState::Pending {
            // Second response to a resolved receipt is a conflict even if it
            // repeats the original decision.
            return ResolveOutcome::Conflict;
        }
        if is_expired(receipt, now) {
            receipt.state = ApprovalState::Expired;
            return ResolveOutcome::Expired;
        }
        receipt.state = decision;
        receipt.note = note;
        match decision {
            ApprovalState::Approved => ResolveOutcome::Approved,
            ApprovalState::Denied => ResolveOutcome::Denied,
            _ => ResolveOutcome::Conflict,
        }
    }

    fn consume(
        &mut self,
        approval_id: &ApprovalId,
        expect: &ApprovalExpectation,
        now: OffsetDateTime,
    ) -> ApprovalOutcome {
        let Some(receipt) = self.receipts.get_mut(approval_id.as_str()) else {
            return ApprovalOutcome::NotFound;
        };
        if receipt.consumed_at.is_some() {
            return ApprovalOutcome::Conflict;
        }
        match receipt.state {
            ApprovalState::Pending => return ApprovalOutcome::Pending,
            ApprovalState::Denied => return ApprovalOutcome::Denied,
            ApprovalState::Expired => return ApprovalOutcome::Expired,
            ApprovalState::Invalidated => {
                return ApprovalOutcome::Stale("approval invalidated by a policy change".into())
            }
            ApprovalState::Approved => {}
        }
        if receipt.intent_hash != expect.intent_hash {
            return ApprovalOutcome::Stale("intent hash changed".into());
        }
        if receipt.policy_revision != expect.policy_revision {
            return ApprovalOutcome::Stale(format!(
                "policy revision changed ({} -> {})",
                receipt.policy_revision, expect.policy_revision
            ));
        }
        if receipt.workspace_identity != expect.workspace_identity {
            return ApprovalOutcome::Stale("workspace identity changed".into());
        }
        if receipt.target_preimage != expect.target_preimage {
            return ApprovalOutcome::Stale("target preimage changed".into());
        }
        if is_expired(receipt, now) {
            receipt.state = ApprovalState::Expired;
            return ApprovalOutcome::Expired;
        }
        receipt.consumed_at = Some(timeutil::now_rfc3339());
        ApprovalOutcome::Authorized
    }

    fn invalidate_for_run(&mut self, run_id: &RunId) -> usize {
        let mut count = 0;
        for receipt in self.receipts.values_mut() {
            if &receipt.run_id == run_id && receipt.state == ApprovalState::Pending {
                receipt.state = ApprovalState::Invalidated;
                count += 1;
            }
        }
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalize::scoped_path;
    use bollo_protocol::vocab::{EffectClass, ToolClass};

    fn intent() -> NormalizedIntent {
        NormalizedIntent {
            tool: "write_file".into(),
            class: ToolClass::Write,
            effect: EffectClass::WorkspaceMutation,
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

    fn receipt(now: OffsetDateTime) -> ApprovalReceipt {
        pending_receipt(
            &intent(),
            SessionId::generate(),
            RunId::generate(),
            ToolCallId::generate(),
            "ws-identity".into(),
            7,
            Some("pre-hash".into()),
            now,
            300,
        )
    }

    #[test]
    fn approved_receipt_is_single_use() {
        let now = OffsetDateTime::now_utc();
        let r = receipt(now);
        let id = r.approval_id.clone();
        let expect = ApprovalExpectation::for_intent(&intent(), 7, "ws-identity", Some("pre-hash".into()));
        let mut store = MemoryApprovalStore::new();
        store.insert(r).unwrap();

        assert_eq!(store.consume(&id, &expect, now), ApprovalOutcome::Pending);
        assert_eq!(
            store.resolve(&id, ApprovalState::Approved, None, now),
            ResolveOutcome::Approved
        );
        assert_eq!(
            store.consume(&id, &expect, now),
            ApprovalOutcome::Authorized
        );
        // Second consumption is a conflict: no double execution.
        assert_eq!(store.consume(&id, &expect, now), ApprovalOutcome::Conflict);
    }

    #[test]
    fn second_user_response_conflicts_even_when_repeating() {
        let now = OffsetDateTime::now_utc();
        let r = receipt(now);
        let id = r.approval_id.clone();
        let mut store = MemoryApprovalStore::new();
        store.insert(r).unwrap();
        assert_eq!(
            store.resolve(&id, ApprovalState::Approved, None, now),
            ResolveOutcome::Approved
        );
        // Repeating the original decision is still a conflict.
        assert_eq!(
            store.resolve(&id, ApprovalState::Approved, None, now),
            ResolveOutcome::Conflict
        );
    }

    #[test]
    fn expired_receipt_cannot_authorize() {
        let now = OffsetDateTime::now_utc();
        let r = receipt(now);
        let id = r.approval_id.clone();
        let expect = ApprovalExpectation::for_intent(&intent(), 7, "ws-identity", Some("pre-hash".into()));
        let mut store = MemoryApprovalStore::new();
        store.insert(r).unwrap();
        let later = now + time::Duration::seconds(301);
        assert_eq!(
            store.resolve(&id, ApprovalState::Approved, None, later),
            ResolveOutcome::Expired
        );
        assert_eq!(store.consume(&id, &expect, later), ApprovalOutcome::Expired);
    }

    #[test]
    fn stale_revision_or_preimage_is_refused() {
        let now = OffsetDateTime::now_utc();
        let r = receipt(now);
        let id = r.approval_id.clone();
        let mut store = MemoryApprovalStore::new();
        store.insert(r).unwrap();
        store.resolve(&id, ApprovalState::Approved, None, now);

        let stale_revision =
            ApprovalExpectation::for_intent(&intent(), 8, "ws-identity", Some("pre-hash".into()));
        assert!(matches!(
            store.consume(&id, &stale_revision, now),
            ApprovalOutcome::Stale(_)
        ));

        let stale_preimage =
            ApprovalExpectation::for_intent(&intent(), 7, "ws-identity", Some("other".into()));
        assert!(matches!(
            store.consume(&id, &stale_preimage, now),
            ApprovalOutcome::Stale(_)
        ));
    }

    #[test]
    fn denied_receipt_never_authorizes() {
        let now = OffsetDateTime::now_utc();
        let r = receipt(now);
        let id = r.approval_id.clone();
        let expect = ApprovalExpectation::for_intent(&intent(), 7, "ws-identity", Some("pre-hash".into()));
        let mut store = MemoryApprovalStore::new();
        store.insert(r).unwrap();
        assert_eq!(
            store.resolve(&id, ApprovalState::Denied, None, now),
            ResolveOutcome::Denied
        );
        assert_eq!(store.consume(&id, &expect, now), ApprovalOutcome::Denied);
    }

    #[test]
    fn policy_change_invalidates_pending_approvals() {
        let now = OffsetDateTime::now_utc();
        let r = receipt(now);
        let id = r.approval_id.clone();
        let run = r.run_id.clone();
        let expect = ApprovalExpectation::for_intent(&intent(), 7, "ws-identity", Some("pre-hash".into()));
        let mut store = MemoryApprovalStore::new();
        store.insert(r).unwrap();
        assert_eq!(store.invalidate_for_run(&run), 1);
        assert!(matches!(
            store.consume(&id, &expect, now),
            ApprovalOutcome::Stale(_)
        ));
    }
}
