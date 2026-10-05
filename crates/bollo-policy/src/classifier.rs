//! Advisory risk classifier (BH-021): the port, the bounded intent projection
//! and the escalation-only composition rule.
//!
//! The classifier is advisory by construction: [`RiskClassifier`] returns
//! calibrated scores, never an authorization, and [`escalate`] can only turn
//! `allow` into `ask`. Deny/ask decisions pass through unchanged, so no
//! verdict — however malformed or hostile — can grant, widen or deny an
//! action. Failures are represented in-band (`availability != Available`) and
//! are neutral, never a denial of service.
//!
//! This module performs no I/O. The out-of-process call is an implementation
//! of [`RiskClassifier`] supplied by the composition root; [`ClassifierGate`]
//! guarantees that `deny`/`ask` decisions, read-only effects and unselected
//! profiles never trigger one.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Mutex;

use bollo_protocol::vocab::{Effect, EffectClass, Profile};

use crate::normalize::{NormalizedIntent, ScopedPath};

/// Hard cap on the serialized projection. Over-cap tears down the optional
/// fields (argv, paths) instead of sending partial data.
pub const PROJECTION_MAX_BYTES: usize = 8 * 1024;
pub const PROJECTION_MAX_PATHS: usize = 8;
pub const PROJECTION_MAX_PATH_CHARS: usize = 256;
/// Executable basename plus at most eight arguments.
pub const PROJECTION_MAX_ARGV: usize = 8;
pub const PROJECTION_MAX_ARG_CHARS: usize = 128;
pub const PROJECTION_MAX_SUMMARY_CHARS: usize = 512;

/// Whether a classifier call produced a usable verdict. Everything except
/// `Available` is neutral: the policy decision is left unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    /// A structurally valid answer was received.
    Available,
    /// No credential is configured, so nothing was sent (zero calls).
    Disabled,
    /// DNS/TLS/connect failure or another transport-level error.
    Unreachable,
    /// The call exceeded the configured deadline.
    Timeout,
    /// 401/403: missing or invalid credential; likely misconfiguration.
    Unauthorized,
    /// 429/529: rate limited or overloaded; no automatic retry pre-gate.
    RateLimited,
    /// Malformed body, missing/mistyped/non-finite answers.
    InvalidResponse,
    /// Any other non-success HTTP status.
    ServiceError,
}

impl Availability {
    /// Stable snake_case name, used by audit records and reports. It matches
    /// the serialization exactly, so reports and stored JSON never drift.
    pub fn as_str(self) -> &'static str {
        match self {
            Availability::Available => "available",
            Availability::Disabled => "disabled",
            Availability::Unreachable => "unreachable",
            Availability::Timeout => "timeout",
            Availability::Unauthorized => "unauthorized",
            Availability::RateLimited => "rate_limited",
            Availability::InvalidResponse => "invalid_response",
            Availability::ServiceError => "service_error",
        }
    }
}

/// One advisory verdict. There is deliberately no allow/deny field: the only
/// decisions a verdict can influence are expressed through scores and the
/// escalation thresholds.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassifierVerdict {
    /// Model id reported by the service, falling back to the configured pin.
    pub model: String,
    pub availability: Availability,
    /// `risk` score answer. Probability-weighted; may exceed 1.0 by design.
    pub risk_score: Option<f64>,
    /// `risk` answer confidence in `0.0..=1.0`.
    pub risk_confidence: Option<f64>,
    /// `credential_access` answer in `0.0..=1.0`.
    pub credential_score: Option<f64>,
}

impl ClassifierVerdict {
    pub fn available(
        model: impl Into<String>,
        risk_score: f64,
        risk_confidence: f64,
        credential_score: f64,
    ) -> Self {
        Self {
            model: model.into(),
            availability: Availability::Available,
            risk_score: Some(risk_score),
            risk_confidence: Some(risk_confidence),
            credential_score: Some(credential_score),
        }
    }

    pub fn unavailable(model: impl Into<String>, availability: Availability) -> Self {
        Self {
            model: model.into(),
            availability,
            risk_score: None,
            risk_confidence: None,
            credential_score: None,
        }
    }

    pub fn is_available(&self) -> bool {
        self.availability == Availability::Available
    }
}

/// The advisory boundary. Implementations must never panic on malformed
/// service behavior and must never perform an effect: the only output is a
/// [`ClassifierVerdict`].
pub trait RiskClassifier: Send + Sync {
    /// Pinned model id, used for audit when the service omits one.
    fn model(&self) -> &str;

    /// Evaluate a bounded, already-redacted projection. Called only for
    /// eligible decisions, at most once per decision.
    fn classify(&self, state: &ClassifierState) -> ClassifierVerdict;
}

/// Escalation thresholds, separate from transport settings so the decision
/// rule is testable without a classifier.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EscalationThresholds {
    /// Risk score at or above which an eligible allow escalates.
    pub escalate_at: f64,
    /// Minimum risk-answer confidence for the score branch.
    pub min_confidence: f64,
    /// Credential-access signal at or above which an allow escalates.
    pub credential_threshold: f64,
}

impl Default for EscalationThresholds {
    fn default() -> Self {
        Self {
            escalate_at: 1.0,
            min_confidence: 0.5,
            credential_threshold: 0.9,
        }
    }
}

impl EscalationThresholds {
    /// Reject thresholds that are not finite or outside `0.0..=1.0`.
    pub fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            ("escalate_at", self.escalate_at),
            ("min_confidence", self.min_confidence),
            ("credential_threshold", self.credential_threshold),
        ] {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(format!("{name} must be a finite number in 0.0..=1.0"));
            }
        }
        Ok(())
    }
}

/// The outcome of composing a verdict with a decision: monotone, with a
/// human-readable note when an escalation happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Escalation {
    pub effect: Effect,
    pub escalated: bool,
    pub note: Option<String>,
}

/// Apply an advisory verdict. Only `Allow` can change, and only to `Ask`;
/// everything else is returned untouched.
pub fn escalate(
    effect: Effect,
    verdict: &ClassifierVerdict,
    thresholds: &EscalationThresholds,
) -> Escalation {
    if effect != Effect::Allow || !verdict.is_available() {
        return Escalation {
            effect,
            escalated: false,
            note: None,
        };
    }
    let risk = match (verdict.risk_score, verdict.risk_confidence) {
        (Some(score), Some(confidence)) => {
            score.is_finite()
                && confidence.is_finite()
                && score >= thresholds.escalate_at
                && confidence >= thresholds.min_confidence
        }
        _ => false,
    };
    let credential = verdict
        .credential_score
        .filter(|signal| signal.is_finite())
        .is_some_and(|signal| signal >= thresholds.credential_threshold);
    if !risk && !credential {
        return Escalation {
            effect,
            escalated: false,
            note: None,
        };
    }
    let mut reasons = Vec::new();
    if risk {
        reasons.push(format!(
            "risk score {:.2} at or above {:.2} (confidence {:.2})",
            verdict.risk_score.unwrap_or_default(),
            thresholds.escalate_at,
            verdict.risk_confidence.unwrap_or_default()
        ));
    }
    if credential {
        reasons.push(format!(
            "credential-access signal {:.2} at or above {:.2}",
            verdict.credential_score.unwrap_or_default(),
            thresholds.credential_threshold
        ));
    }
    Escalation {
        effect: Effect::Ask,
        escalated: true,
        note: Some(format!(
            "advisory classifier: {} [{}]",
            reasons.join("; "),
            verdict.model
        )),
    }
}

/// The bounded, redacted projection of a normalized intent: the only state a
/// classifier may ever see. Never contains file contents, prompts, tool
/// results, environment names or values, credentials, journal rows, artifacts
/// or protected-state contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassifierState {
    /// Canonical tool name (`mcp:<server>:<tool>` for MCP).
    pub tool: String,
    pub class: String,
    pub effect: String,
    pub summary: String,
    pub paths: Vec<String>,
    pub argv: Vec<String>,
    pub cwd: Option<String>,
}

impl ClassifierState {
    /// Project a normalized intent with per-field caps and secret-shaped value
    /// replacement. Over-cap drops argv then paths; it never sends partial
    /// file content (there is none) and never grows past [`PROJECTION_MAX_BYTES`].
    ///
    /// MCP argument key names and byte totals are not yet projected because
    /// `NormalizedIntent` does not carry them; values are never sent either way.
    pub fn from_intent(intent: &NormalizedIntent) -> Self {
        let paths: Vec<String> = intent
            .paths
            .iter()
            .take(PROJECTION_MAX_PATHS)
            .map(|path| match path {
                ScopedPath::Outside { .. } => "<outside-workspace>".to_string(),
                other => redact_secret_shaped(&truncate_chars(
                    &other.display(),
                    PROJECTION_MAX_PATH_CHARS,
                )),
            })
            .collect();
        let argv = match &intent.argv {
            Some(argv) if !argv.is_empty() => {
                let mut projected = vec![redact_secret_shaped(&truncate_chars(
                    &executable_basename(&argv[0]),
                    PROJECTION_MAX_ARG_CHARS,
                ))];
                projected.extend(argv.iter().skip(1).take(PROJECTION_MAX_ARGV).map(|arg| {
                    redact_secret_shaped(&truncate_chars(arg, PROJECTION_MAX_ARG_CHARS))
                }));
                projected
            }
            _ => Vec::new(),
        };
        let cwd = intent.cwd.as_ref().map(|cwd| match cwd {
            ScopedPath::Outside { .. } => "<outside-workspace>".to_string(),
            other => other.display(),
        });
        let mut state = Self {
            tool: intent.tool.clone(),
            class: intent.class.as_str().to_string(),
            effect: effect_class_name(intent.effect).to_string(),
            summary: redact_secret_shaped(&truncate_chars(
                &intent.summary,
                PROJECTION_MAX_SUMMARY_CHARS,
            )),
            paths,
            argv,
            cwd,
        };
        let over_cap = match serde_json::to_vec(&state) {
            Ok(bytes) => bytes.len() > PROJECTION_MAX_BYTES,
            Err(_) => true,
        };
        if over_cap {
            state.argv.clear();
            state.paths.clear();
        }
        state
    }
}

/// The runtime-side gate: holds the classifier, the profiles where escalation
/// is active, and the thresholds. `deny`/`ask` decisions, read-only effects
/// and unselected profiles produce **zero calls**.
pub struct ClassifierGate<'a> {
    classifier: &'a dyn RiskClassifier,
    profiles: Vec<Profile>,
    thresholds: EscalationThresholds,
}

impl<'a> ClassifierGate<'a> {
    /// Balanced and workspace_auto escalate by default; `read_only` cannot
    /// allow mutation anyway, and `unrestricted` requires an explicit opt-in.
    pub fn new(classifier: &'a dyn RiskClassifier) -> Self {
        Self {
            classifier,
            profiles: vec![Profile::Balanced, Profile::WorkspaceAuto],
            thresholds: EscalationThresholds::default(),
        }
    }

    pub fn with_profiles(mut self, profiles: Vec<Profile>) -> Self {
        self.profiles = profiles;
        self
    }

    pub fn with_thresholds(mut self, thresholds: EscalationThresholds) -> Self {
        self.thresholds = thresholds;
        self
    }

    pub fn profiles(&self) -> &[Profile] {
        &self.profiles
    }

    pub fn thresholds(&self) -> &EscalationThresholds {
        &self.thresholds
    }

    pub fn classifier(&self) -> &dyn RiskClassifier {
        self.classifier
    }

    /// Classify an eligible effective decision, or return `None` without any
    /// classifier call. Eligible means: the effective decision is `allow`,
    /// the normalized effect is not read-only, and the active profile is
    /// selected for escalation.
    pub fn hint(
        &self,
        intent: &NormalizedIntent,
        effect: Effect,
        profile: Profile,
    ) -> Option<ClassifierVerdict> {
        if effect != Effect::Allow || intent.effect == EffectClass::Read {
            return None;
        }
        if !self.profiles.contains(&profile) {
            return None;
        }
        Some(
            self.classifier
                .classify(&ClassifierState::from_intent(intent)),
        )
    }

    /// Monotone composition of the hint with the decision.
    pub fn apply(&self, effect: Effect, verdict: &ClassifierVerdict) -> Escalation {
        escalate(effect, verdict, &self.thresholds)
    }
}

/// A first-class deterministic test double for the port, mirroring the
/// scripted provider: replays verdicts in order and records every projection
/// it was asked about. Exhaustion fails neutral, never panics.
pub struct ScriptedClassifier {
    model: String,
    verdicts: Mutex<VecDeque<ClassifierVerdict>>,
    states: Mutex<Vec<ClassifierState>>,
}

impl ScriptedClassifier {
    pub fn new(model: impl Into<String>, verdicts: Vec<ClassifierVerdict>) -> Self {
        Self {
            model: model.into(),
            verdicts: Mutex::new(verdicts.into()),
            states: Mutex::new(Vec::new()),
        }
    }

    pub fn calls(&self) -> usize {
        self.states.lock().unwrap().len()
    }

    pub fn remaining(&self) -> usize {
        self.verdicts.lock().unwrap().len()
    }

    pub fn states(&self) -> Vec<ClassifierState> {
        self.states.lock().unwrap().clone()
    }
}

impl RiskClassifier for ScriptedClassifier {
    fn model(&self) -> &str {
        &self.model
    }

    fn classify(&self, state: &ClassifierState) -> ClassifierVerdict {
        self.states.lock().unwrap().push(state.clone());
        self.verdicts
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| {
                ClassifierVerdict::unavailable(&self.model, Availability::InvalidResponse)
            })
    }
}

fn effect_class_name(class: EffectClass) -> &'static str {
    match class {
        EffectClass::Read => "read",
        EffectClass::WorkspaceMutation => "workspace_mutation",
        EffectClass::ExternalMutation => "external_mutation",
        EffectClass::OutsideWorkspace => "outside_workspace",
        EffectClass::Execution => "execution",
    }
}

fn executable_basename(argv0: &str) -> String {
    argv0
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(argv0)
        .to_string()
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

const SECRET_MARKERS: &[&str] = &[
    "token",
    "secret",
    "password",
    "passwd",
    "api_key",
    "apikey",
    "api-key",
    "credential",
    "bearer",
];

/// Replace secret-shaped values before serialization: `name=value` whose name
/// looks secret-shaped, bearer strings, and long hex/base64 runs. Conservative
/// by design — false positives only remove data from the outbound projection.
pub fn redact_secret_shaped(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    if lower.starts_with("bearer ") || lower.contains(" bearer ") {
        return "<redacted>".to_string();
    }
    for separator in ['=', ':'] {
        if let Some((name, _)) = value.split_once(separator) {
            let name = name.to_ascii_lowercase();
            if SECRET_MARKERS.iter().any(|marker| name.contains(marker)) {
                return "<redacted>".to_string();
            }
        }
    }
    redact_long_runs(value)
}

fn redact_long_runs(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    let mut result = String::with_capacity(value.len());
    let mut index = 0;
    while index < chars.len() {
        let is_run_char = |c: char| c.is_ascii_alphanumeric() || c == '+' || c == '/';
        if is_run_char(chars[index]) {
            let start = index;
            while index < chars.len() && is_run_char(chars[index]) {
                index += 1;
            }
            let run: String = chars[start..index].iter().collect();
            let is_hex = run.chars().all(|c| c.is_ascii_hexdigit());
            if (is_hex && run.len() >= 32) || run.len() >= 40 {
                result.push_str("<redacted>");
            } else {
                result.push_str(&run);
            }
        } else {
            result.push(chars[index]);
            index += 1;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalize::{effect_class_for, scoped_path};
    use bollo_protocol::vocab::ToolClass;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Strictness ordering: deny > ask > allow.
    fn strictness(effect: Effect) -> u8 {
        match effect {
            Effect::Allow => 0,
            Effect::Ask => 1,
            Effect::Deny => 2,
        }
    }

    fn root() -> &'static str {
        if cfg!(windows) {
            "C:/proj"
        } else {
            "/proj"
        }
    }

    fn intent(class: ToolClass, argv: Option<Vec<&str>>) -> NormalizedIntent {
        intent_at(class, vec!["src/main.rs"], argv)
    }

    fn intent_at(class: ToolClass, paths: Vec<&str>, argv: Option<Vec<&str>>) -> NormalizedIntent {
        let scoped: Vec<_> = paths
            .iter()
            .map(|p| scoped_path(root(), p).unwrap())
            .collect();
        let any_outside = scoped.iter().any(|p| p.is_outside());
        NormalizedIntent {
            tool: class.as_str().to_string(),
            class,
            effect: effect_class_for(class, any_outside),
            paths: scoped,
            argv: argv.map(|v| v.into_iter().map(str::to_string).collect()),
            cwd: Some(scoped_path(root(), ".").unwrap()),
            environment_names: Vec::new(),
            summary: "test intent".into(),
            arguments_digest: "0".repeat(64),
        }
    }

    struct CountingClassifier {
        calls: AtomicUsize,
    }

    impl CountingClassifier {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
            }
        }

        fn count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl RiskClassifier for CountingClassifier {
        fn model(&self) -> &str {
            "counting"
        }

        fn classify(&self, _state: &ClassifierState) -> ClassifierVerdict {
            self.calls.fetch_add(1, Ordering::SeqCst);
            ClassifierVerdict::available("counting", 0.0, 1.0, 0.0)
        }
    }

    fn escalating(model: &str) -> ClassifierVerdict {
        ClassifierVerdict::available(model, 1.2, 0.9, 0.0)
    }

    #[test]
    fn escalation_is_monotone_and_only_allow_changes() {
        let thresholds = EscalationThresholds::default();
        let verdicts = vec![
            ClassifierVerdict::available("m", 0.0, 1.0, 0.0),
            escalating("m"),
            ClassifierVerdict::available("m", 0.0, 1.0, 0.99),
            ClassifierVerdict::available("m", 0.99, 0.9, 0.0),
            ClassifierVerdict::available("m", 1.2, 0.4, 0.0),
            ClassifierVerdict::unavailable("m", Availability::Unreachable),
            ClassifierVerdict::unavailable("m", Availability::Timeout),
            ClassifierVerdict::unavailable("m", Availability::InvalidResponse),
            ClassifierVerdict::unavailable("m", Availability::RateLimited),
            ClassifierVerdict {
                model: "m".into(),
                availability: Availability::Available,
                risk_score: Some(f64::NAN),
                risk_confidence: Some(f64::NAN),
                credential_score: Some(f64::NAN),
            },
        ];
        for effect in [Effect::Allow, Effect::Ask, Effect::Deny] {
            for verdict in &verdicts {
                let result = escalate(effect, verdict, &thresholds);
                assert!(
                    strictness(result.effect) >= strictness(effect),
                    "verdict weakened {effect:?}: {verdict:?} -> {:?}",
                    result.effect
                );
                if effect != Effect::Allow {
                    assert_eq!(result.effect, effect, "only allow may change");
                    assert!(!result.escalated);
                    assert!(result.note.is_none());
                } else {
                    assert!(matches!(result.effect, Effect::Allow | Effect::Ask));
                }
            }
        }
    }

    #[test]
    fn thresholds_decide_the_risk_and_credential_branches() {
        let thresholds = EscalationThresholds::default();
        // At the threshold escalates; just below does not.
        assert!(
            escalate(
                Effect::Allow,
                &ClassifierVerdict::available("m", 1.0, 0.5, 0.0),
                &thresholds
            )
            .escalated
        );
        assert!(
            !escalate(
                Effect::Allow,
                &ClassifierVerdict::available("m", 0.99, 0.9, 0.0),
                &thresholds
            )
            .escalated
        );
        assert!(
            !escalate(
                Effect::Allow,
                &ClassifierVerdict::available("m", 1.2, 0.49, 0.0),
                &thresholds
            )
            .escalated
        );
        // The credential branch escalates independently of the risk branch.
        assert!(
            escalate(
                Effect::Allow,
                &ClassifierVerdict::available("m", 0.0, 0.0, 0.9),
                &thresholds
            )
            .escalated
        );
        // An unavailable verdict never escalates even with stale scores.
        let unavailable = ClassifierVerdict {
            model: "m".into(),
            availability: Availability::Timeout,
            risk_score: Some(1.5),
            risk_confidence: Some(0.9),
            credential_score: Some(1.0),
        };
        assert!(!escalate(Effect::Allow, &unavailable, &thresholds).escalated);
    }

    #[test]
    fn threshold_validation_rejects_non_finite_and_out_of_range() {
        assert!(EscalationThresholds::default().validate().is_ok());
        for bad in [f64::NAN, f64::INFINITY, -0.1, 1.1] {
            let mut thresholds = EscalationThresholds::default();
            thresholds.escalate_at = bad;
            assert!(thresholds.validate().is_err(), "{bad} accepted");
        }
    }

    #[test]
    fn ineligible_decisions_make_zero_calls() {
        let classifier = CountingClassifier::new();
        let gate = ClassifierGate::new(&classifier);
        // Ask and deny: no call even when the effect class is mutating.
        assert!(gate
            .hint(
                &intent(ToolClass::Write, None),
                Effect::Ask,
                Profile::Balanced
            )
            .is_none());
        assert!(gate
            .hint(
                &intent(ToolClass::Write, None),
                Effect::Deny,
                Profile::Balanced
            )
            .is_none());
        // Read-only effects: no call.
        assert!(gate
            .hint(
                &intent(ToolClass::Read, None),
                Effect::Allow,
                Profile::Balanced
            )
            .is_none());
        // Unselected profile: no call.
        assert!(gate
            .hint(
                &intent(ToolClass::Write, None),
                Effect::Allow,
                Profile::Unrestricted
            )
            .is_none());
        // Read-only profile is unreachable for mutations anyway.
        assert!(gate
            .hint(
                &intent(ToolClass::Write, None),
                Effect::Allow,
                Profile::ReadOnly
            )
            .is_none());
        assert_eq!(classifier.count(), 0);

        // Exactly one call for an eligible allow.
        assert!(gate
            .hint(
                &intent(ToolClass::Exec, Some(vec!["cargo", "test"])),
                Effect::Allow,
                Profile::WorkspaceAuto
            )
            .is_some());
        assert_eq!(classifier.count(), 1);
    }

    #[test]
    fn gate_defaults_and_overrides() {
        let classifier = CountingClassifier::new();
        let gate = ClassifierGate::new(&classifier);
        assert_eq!(
            gate.profiles(),
            &[Profile::Balanced, Profile::WorkspaceAuto]
        );
        assert!(!gate
            .hint(
                &intent(ToolClass::Write, None),
                Effect::Allow,
                Profile::Unrestricted
            )
            .is_some());

        let gate = ClassifierGate::new(&classifier)
            .with_profiles(vec![Profile::Unrestricted])
            .with_thresholds(EscalationThresholds {
                escalate_at: 0.0,
                min_confidence: 0.0,
                credential_threshold: 1.0,
            });
        let verdict = classifier.classify(&ClassifierState::from_intent(&intent(
            ToolClass::Write,
            None,
        )));
        let escalation = gate.apply(Effect::Allow, &verdict);
        assert!(escalation.escalated);
        assert!(escalation.note.unwrap().contains("counting"));
    }

    #[test]
    fn projection_is_bounded_and_redacts_secret_shaped_values() {
        let long_summary = "s".repeat(1_000);
        let hex = "a".repeat(64);
        let base64 = "QWxsb2dhdG9y".repeat(4);
        let mut prepared = intent(
            ToolClass::Exec,
            Some(vec![
                "/usr/bin/bash",
                "--token=sk-live-secret-value",
                &hex,
                &base64,
                "Bearer abcdef0123456789",
            ]),
        );
        prepared.summary = long_summary;
        for index in 0..12 {
            prepared
                .paths
                .push(scoped_path(root(), &format!("src/file{index}.rs")).unwrap());
        }
        let state = ClassifierState::from_intent(&prepared);
        assert_eq!(state.argv[0], "bash", "executable basename only");
        assert!(state.argv.iter().any(|a| a == "<redacted>"));
        assert!(state.paths.len() <= PROJECTION_MAX_PATHS);
        assert!(state.summary.chars().count() <= PROJECTION_MAX_SUMMARY_CHARS);
        assert!(state
            .argv
            .iter()
            .all(|a| a.chars().count() <= PROJECTION_MAX_ARG_CHARS));
        assert!(state
            .paths
            .iter()
            .all(|p| p.chars().count() <= PROJECTION_MAX_PATH_CHARS));
        let serialized = serde_json::to_string(&state).unwrap();
        assert!(!serialized.contains(&hex));
        assert!(!serialized.contains(&base64));
        assert!(!serialized.contains("sk-live-secret-value"));
        assert!(!serialized.contains("abcdef0123456789"));
        assert!(serialized.len() <= PROJECTION_MAX_BYTES);
    }

    #[test]
    fn projection_never_carries_environment_or_outside_paths() {
        let mut prepared = intent_at(
            ToolClass::Write,
            vec!["src/main.rs", "../secrets/.env"],
            None,
        );
        prepared.environment_names = vec!["ANTHROPIC_API_KEY".into()];
        prepared.summary = "write src/main.rs".into();
        let state = ClassifierState::from_intent(&prepared);
        assert_eq!(state.paths, vec!["src/main.rs", "<outside-workspace>"]);
        let serialized = serde_json::to_string(&state).unwrap();
        assert!(!serialized.contains("ANTHROPIC_API_KEY"));
        assert!(!serialized.contains("arguments_digest"));
    }

    #[test]
    fn scripted_classifier_replays_and_records() {
        let classifier = ScriptedClassifier::new(
            "jev-test",
            vec![
                escalating("jev-test"),
                ClassifierVerdict::unavailable("jev-test", Availability::Timeout),
            ],
        );
        let state = ClassifierState::from_intent(&intent(ToolClass::Write, None));
        assert!(classifier.classify(&state).is_available());
        assert_eq!(
            classifier.classify(&state).availability,
            Availability::Timeout
        );
        // Exhaustion fails neutral instead of panicking.
        let exhausted = classifier.classify(&state);
        assert!(!exhausted.is_available());
        assert_eq!(classifier.calls(), 3);
        assert_eq!(classifier.states().len(), 3);
    }
}
