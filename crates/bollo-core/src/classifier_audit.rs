//! Per-run advisory-classifier audit (BH-021).
//!
//! The classifier is explicitly enabled model spend, but no trusted price
//! source exists yet (OD-07), so its cost is reported as unknown (`None`),
//! never as zero. Counts and availability travel with the run itself: the
//! NDJSON event schema stays frozen, so this audit never becomes a new event
//! type — it is persisted on the run record and printed in the run summary.

use std::collections::BTreeMap;

use bollo_policy::classifier::{Availability, ClassifierVerdict};
use serde::{Deserialize, Serialize};

/// Advisory-classifier activity for one run. `Default` means no gate was ever
/// attached, which is exactly zero calls and zero cost.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassifierAudit {
    /// True when a gate was attached to the run, so "0 calls" is a fact rather
    /// than an absence of instrumentation.
    #[serde(default)]
    pub attached: bool,
    /// Eligible decisions that invoked the classifier. A `disabled` verdict
    /// means no credential was configured, so no data left the process even
    /// though the decision is counted here.
    #[serde(default)]
    pub calls: u32,
    /// Verdicts by availability, e.g. `{"available": 1, "timeout": 1}`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub availability: BTreeMap<Availability, u32>,
    /// Applied allow→ask escalations (always a subset of `calls`).
    #[serde(default)]
    pub escalations: u32,
    /// False until a trusted classifier price source exists (OD-07).
    #[serde(default)]
    pub cost_known: bool,
    /// `None` while `cost_known` is false: unknown is never zero.
    #[serde(default)]
    pub cost_microusd: Option<u64>,
}

impl ClassifierAudit {
    /// Record one verdict and whether it escalated the allow to an ask.
    pub fn record(&mut self, verdict: &ClassifierVerdict, escalated: bool) {
        self.calls = self.calls.saturating_add(1);
        let count = self.availability.entry(verdict.availability).or_insert(0);
        *count = count.saturating_add(1);
        if escalated {
            self.escalations = self.escalations.saturating_add(1);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.calls == 0
    }

    fn calls_label(&self) -> String {
        if self.calls == 1 {
            "1 call".to_string()
        } else {
            format!("{} calls", self.calls)
        }
    }

    /// Full one-line usage/audit report:
    /// `2 calls · available 1, timeout 1 · 1 escalated · cost unknown`.
    pub fn summary_line(&self) -> String {
        let mut parts = vec![self.calls_label()];
        if !self.availability.is_empty() {
            let breakdown = self
                .availability
                .iter()
                .map(|(availability, count)| format!("{} {count}", availability.as_str()))
                .collect::<Vec<_>>()
                .join(", ");
            parts.push(breakdown);
        }
        if self.escalations > 0 {
            parts.push(format!("{} escalated", self.escalations));
        }
        match self.cost_microusd {
            Some(micro) => parts.push(format!("cost {micro}µ$")),
            None => parts.push("cost unknown".to_string()),
        }
        parts.join(" · ")
    }

    /// Compact form for the turn summary: `2 calls, 1 escalated`.
    pub fn compact(&self) -> String {
        let mut text = self.calls_label();
        if self.escalations > 0 {
            text.push_str(&format!(", {} escalated", self.escalations));
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verdict(availability: Availability) -> ClassifierVerdict {
        if availability == Availability::Available {
            ClassifierVerdict::available("jev-test", 1.2, 0.9, 0.0)
        } else {
            ClassifierVerdict::unavailable("jev-test", availability)
        }
    }

    #[test]
    fn records_counts_availability_and_escalations() {
        let mut audit = ClassifierAudit {
            attached: true,
            ..Default::default()
        };
        audit.record(&verdict(Availability::Available), true);
        audit.record(&verdict(Availability::Timeout), false);
        audit.record(&verdict(Availability::Timeout), false);
        assert_eq!(audit.calls, 3);
        assert_eq!(audit.escalations, 1);
        assert_eq!(audit.availability.get(&Availability::Available), Some(&1));
        assert_eq!(audit.availability.get(&Availability::Timeout), Some(&2));
        assert!(audit.cost_microusd.is_none(), "unknown is never zero");
        assert!(!audit.cost_known);
        assert!(!audit.is_empty());
    }

    #[test]
    fn availability_names_match_the_serialized_form() {
        for availability in [
            Availability::Available,
            Availability::Disabled,
            Availability::Unreachable,
            Availability::Timeout,
            Availability::Unauthorized,
            Availability::RateLimited,
            Availability::InvalidResponse,
            Availability::ServiceError,
        ] {
            let serialized = serde_json::to_value(availability).unwrap();
            assert_eq!(serialized.as_str(), Some(availability.as_str()));
        }
    }

    #[test]
    fn audit_round_trips_through_json_with_unknown_cost() {
        let mut audit = ClassifierAudit {
            attached: true,
            ..Default::default()
        };
        audit.record(&verdict(Availability::Available), true);
        audit.record(&verdict(Availability::Unreachable), false);
        let json = serde_json::to_string(&audit).unwrap();
        assert!(json.contains("\"cost_known\":false"), "{json}");
        assert!(
            json.contains("\"cost_microusd\":null"),
            "unknown cost is null, never zero: {json}"
        );
        let restored: ClassifierAudit = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, audit);
    }

    #[test]
    fn summary_line_reports_calls_availability_and_unknown_cost() {
        let mut audit = ClassifierAudit {
            attached: true,
            ..Default::default()
        };
        audit.record(&verdict(Availability::Available), true);
        audit.record(&verdict(Availability::Timeout), false);
        let line = audit.summary_line();
        assert_eq!(
            line,
            "2 calls · available 1, timeout 1 · 1 escalated · cost unknown"
        );
        assert_eq!(audit.compact(), "2 calls, 1 escalated");

        let attached_no_calls = ClassifierAudit {
            attached: true,
            ..Default::default()
        };
        assert_eq!(attached_no_calls.summary_line(), "0 calls · cost unknown");
        assert_eq!(attached_no_calls.compact(), "0 calls");
    }

    #[test]
    fn disabled_verdict_counts_as_a_call_without_egress() {
        let mut audit = ClassifierAudit {
            attached: true,
            ..Default::default()
        };
        audit.record(&verdict(Availability::Disabled), false);
        assert_eq!(audit.calls, 1);
        assert_eq!(audit.availability.get(&Availability::Disabled), Some(&1));
        assert_eq!(audit.escalations, 0);
    }
}
