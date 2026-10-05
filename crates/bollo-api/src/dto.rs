//! Wire DTOs for the documented OpenAPI contract.
//!
//! Request types deny unknown fields so a typo fails loudly instead of being
//! ignored. Response types serialize exactly the schema's properties.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use bollo_policy::approval::{ApprovalReceipt, ApprovalState};
use bollo_policy::classifier::Availability;
use bollo_protocol::timeutil;
use bollo_protocol::vocab::{Effect, Profile, RunState, SandboxMode};
use bollo_store::{ArtifactRef, RunDetail, SessionRecord};

use crate::state::{PolicySnapshot, ProvenanceView, RuleView};

pub const API_VERSION: &str = "v1";
pub const SCHEMA_VERSION: &str = "0.1";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSessionRequest {
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRunRequest {
    pub prompt: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Approve,
    Deny,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalDecisionRequest {
    pub decision: Decision,
    pub intent_hash: String,
    pub policy_revision: u64,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct HealthDto {
    pub status: &'static str,
    pub api_version: &'static str,
}

impl HealthDto {
    pub fn ok() -> Self {
        Self {
            status: "ok",
            api_version: API_VERSION,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CapabilitiesDto {
    pub api_version: &'static str,
    pub schema_version: &'static str,
    pub features: Vec<&'static str>,
    pub sandbox_verified: bool,
}

impl CapabilitiesDto {
    pub fn current(sandbox_verified: bool) -> Self {
        Self {
            api_version: API_VERSION,
            schema_version: SCHEMA_VERSION,
            features: vec![
                "sessions",
                "runs",
                "approvals",
                "events",
                "artifacts",
                "policy",
            ],
            sandbox_verified,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct SessionDto {
    pub id: String,
    pub workspace_id: String,
    pub created_at: String,
    pub last_seq: u64,
}

impl From<&SessionRecord> for SessionDto {
    fn from(record: &SessionRecord) -> Self {
        Self {
            id: record.id.to_string(),
            workspace_id: record.workspace_identity.clone(),
            created_at: record.created_at.clone(),
            last_seq: record.last_seq,
        }
    }
}

/// Map a stored run state string onto the documented vocabulary. Unknown
/// values cannot be invented away, so they surface as `failed`.
pub fn run_state(value: &str) -> RunState {
    match value {
        "queued" => RunState::Queued,
        "running" => RunState::Running,
        "waiting_approval" => RunState::WaitingApproval,
        "completed" => RunState::Completed,
        "failed" => RunState::Failed,
        "cancelled" => RunState::Cancelled,
        "blocked" => RunState::Blocked,
        "interrupted" => RunState::Interrupted,
        _ => RunState::Failed,
    }
}

/// The persisted per-run advisory-classifier audit (BH-021) as exposed by run
/// reads. This mirrors what `bollo-core` writes into the store's opaque
/// `runs.classifier_json` column (the shape is locked by a round-trip test);
/// unknown fields are tolerated on decode and the wire object stays exactly
/// the documented six properties.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassifierAuditDto {
    /// True when a gate was attached, so "0 calls" is a fact rather than an
    /// absence of instrumentation.
    #[serde(default)]
    pub attached: bool,
    /// Eligible decisions that invoked the classifier (a `disabled` verdict
    /// counts, though no data left the process).
    #[serde(default)]
    pub calls: u32,
    /// Verdict counts keyed by availability (`available`, `timeout`, ...).
    #[serde(default)]
    pub availability: BTreeMap<Availability, u32>,
    /// Applied allow→ask escalations, always a subset of `calls`.
    #[serde(default)]
    pub escalations: u32,
    /// False until a trusted classifier price source exists (OD-07).
    #[serde(default)]
    pub cost_known: bool,
    /// `None` while `cost_known` is false: unknown is never zero.
    #[serde(default)]
    pub cost_microusd: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct RunDto {
    pub id: String,
    pub session_id: String,
    pub state: RunState,
    pub created_at: String,
    pub reason: Option<String>,
    /// `null` when no gate was attached to the run. A stored audit that no
    /// longer decodes also reads as `null`: advisory data never fails a run
    /// read, and the counters are additive to the durable record.
    pub classifier: Option<ClassifierAuditDto>,
}

impl From<&RunDetail> for RunDto {
    fn from(detail: &RunDetail) -> Self {
        Self {
            id: detail.id.to_string(),
            session_id: detail.session_id.to_string(),
            state: run_state(&detail.state),
            created_at: detail.started_at.clone(),
            reason: None,
            classifier: detail
                .classifier_json
                .as_deref()
                .and_then(|raw| serde_json::from_str(raw).ok()),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CancelResultDto {
    pub run_id: String,
    pub state: RunState,
}

#[derive(Debug, Serialize)]
pub struct ArtifactDto {
    pub id: String,
    pub run_id: String,
    pub media_type: String,
    pub bytes: u64,
    #[serde(rename = "sha256")]
    pub hash: String,
}

impl ArtifactDto {
    pub fn from_ref(run_id: &str, artifact: &ArtifactRef) -> Self {
        Self {
            id: artifact.id.to_string(),
            run_id: run_id.to_string(),
            media_type: artifact.media_type.clone(),
            bytes: artifact.bytes,
            hash: artifact.hash.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalStatusDto {
    Pending,
    Approved,
    Denied,
    Expired,
    Invalidated,
}

#[derive(Debug, Serialize)]
pub struct ApprovalDto {
    pub id: String,
    pub run_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub intent_hash: String,
    pub policy_revision: u64,
    pub expires_at: String,
    pub status: ApprovalStatusDto,
    pub display_text: String,
    pub display_complete: bool,
    pub workspace_scope: String,
    pub sandbox: SandboxMode,
    pub reason: String,
    pub rule_source: String,
}

impl ApprovalDto {
    pub fn from_receipt(
        receipt: &ApprovalReceipt,
        workspace_scope: &str,
        sandbox: SandboxMode,
        now: time::OffsetDateTime,
    ) -> Self {
        Self {
            id: receipt.approval_id.to_string(),
            run_id: receipt.run_id.to_string(),
            tool_call_id: receipt.tool_call_id.to_string(),
            tool_name: receipt.tool_name.clone(),
            intent_hash: receipt.intent_hash.clone(),
            policy_revision: receipt.policy_revision,
            expires_at: receipt.expires_at.clone(),
            status: approval_status(receipt, now),
            display_text: receipt.summary.clone(),
            display_complete: true,
            workspace_scope: workspace_scope.to_string(),
            sandbox,
            reason: receipt
                .note
                .clone()
                .unwrap_or_else(|| receipt.summary.clone()),
            rule_source: "policy".to_string(),
        }
    }
}

fn approval_status(receipt: &ApprovalReceipt, now: time::OffsetDateTime) -> ApprovalStatusDto {
    if receipt.state == ApprovalState::Pending {
        if let Ok(expires) = timeutil::parse_rfc3339(&receipt.expires_at) {
            if now >= expires {
                return ApprovalStatusDto::Expired;
            }
        }
    }
    match receipt.state {
        ApprovalState::Pending => ApprovalStatusDto::Pending,
        ApprovalState::Approved => ApprovalStatusDto::Approved,
        ApprovalState::Denied => ApprovalStatusDto::Denied,
        ApprovalState::Expired => ApprovalStatusDto::Expired,
        ApprovalState::Invalidated => ApprovalStatusDto::Invalidated,
    }
}

#[derive(Debug, Serialize)]
pub struct ApprovalResultDto {
    pub approval_id: String,
    pub decision: Decision,
}

#[derive(Debug, Serialize)]
pub struct RuleDto {
    pub id: String,
    pub effect: Effect,
    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_glob: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub argv_prefix: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct ProvenanceDto {
    pub rule_id: String,
    pub source: String,
    pub layer: String,
}

#[derive(Debug, Serialize)]
pub struct PolicyDto {
    pub revision: u64,
    pub profile: Profile,
    pub sandbox: SandboxMode,
    pub isolation_verified: bool,
    pub rules: Vec<RuleDto>,
    pub provenance: Vec<ProvenanceDto>,
}

impl From<&PolicySnapshot> for PolicyDto {
    fn from(snapshot: &PolicySnapshot) -> Self {
        Self {
            revision: snapshot.revision,
            profile: snapshot.profile,
            sandbox: snapshot.sandbox,
            isolation_verified: snapshot.isolation_verified,
            rules: snapshot
                .rules
                .iter()
                .map(|rule: &RuleView| RuleDto {
                    id: rule.id.clone(),
                    effect: rule.effect,
                    tool: rule.tool.clone(),
                    path_glob: rule.path_glob.clone(),
                    argv_prefix: rule.argv_prefix.clone(),
                })
                .collect(),
            provenance: snapshot
                .provenance
                .iter()
                .map(|origin: &ProvenanceView| ProvenanceDto {
                    rule_id: origin.rule_id.clone(),
                    source: origin.source.clone(),
                    layer: origin.layer.clone(),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollo_policy::classifier::ClassifierVerdict;
    use bollo_protocol::ids::{RunId, SessionId};
    use serde_json::json;

    fn run_detail(classifier_json: Option<&str>) -> RunDetail {
        RunDetail {
            id: RunId::generate(),
            session_id: SessionId::generate(),
            state: "completed".into(),
            policy_revision: 7,
            model_id: "replay-model".into(),
            provider: "replay".into(),
            started_at: "2026-10-04T10:00:00Z".into(),
            ended_at: Some("2026-10-04T10:00:05Z".into()),
            classifier_json: classifier_json.map(str::to_string),
        }
    }

    #[test]
    fn classifier_audit_dto_matches_the_core_serialization() {
        let mut audit = bollo_core::ClassifierAudit {
            attached: true,
            ..Default::default()
        };
        audit.record(
            &ClassifierVerdict::available("jev-test", 1.2, 0.9, 0.0),
            true,
        );
        audit.record(
            &ClassifierVerdict::unavailable("jev-test", Availability::Timeout),
            false,
        );
        let stored = serde_json::to_string(&audit).expect("the core audit serializes");
        let dto: ClassifierAuditDto =
            serde_json::from_str(&stored).expect("the API decodes the persisted audit");
        assert_eq!(
            serde_json::to_value(&dto).unwrap(),
            serde_json::to_value(&audit).unwrap(),
            "the API must expose exactly what bollo-core persisted"
        );
    }

    #[test]
    fn run_reads_always_carry_classifier_and_only_decode_valid_audits() {
        // No gate attached: the property is present as null, not absent.
        let plain = serde_json::to_value(RunDto::from(&run_detail(None))).unwrap();
        assert!(plain.as_object().unwrap().contains_key("classifier"));
        assert!(plain["classifier"].is_null());

        // A minimal-but-valid audit fills the documented defaults.
        let minimal =
            serde_json::to_value(RunDto::from(&run_detail(Some(r#"{"attached":true}"#)))).unwrap();
        assert_eq!(minimal["classifier"]["attached"], true);
        assert_eq!(minimal["classifier"]["calls"], 0);
        assert_eq!(minimal["classifier"]["availability"], json!({}));
        assert!(minimal["classifier"]["cost_microusd"].is_null());

        // Unreadable stored JSON reads as null; advisory data never fails the read.
        let broken = serde_json::to_value(RunDto::from(&run_detail(Some("{not json")))).unwrap();
        assert!(broken["classifier"].is_null());
    }
}
