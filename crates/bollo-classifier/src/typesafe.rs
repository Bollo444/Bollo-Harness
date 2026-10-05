//! TypeSafe `POST /v1/systemone` adapter.
//!
//! Pinned shape (v1): one request per eligible decision, two typed questions
//! (`risk` score, `credential_access` noul), strict answer validation, and
//! fail-neutral mapping of every failure to an in-band `Availability`. The
//! credential is read from the configured environment variable at call time,
//! attached only to the configured origin, and never passed anywhere else.

use std::io::Read;
use std::time::Duration;

use bollo_policy::classifier::{Availability, ClassifierState, ClassifierVerdict, RiskClassifier};
use bollo_providers::{HttpRequest, Transport, TransportError};

// Defaults live in `bollo-policy::config` (the parser and the schema's source
// of truth) and are aliased here so the two cannot drift apart.
pub const DEFAULT_ORIGIN: &str = bollo_policy::config::CLASSIFIER_DEFAULT_ORIGIN;
pub const DEFAULT_MODEL: &str = bollo_policy::config::CLASSIFIER_DEFAULT_MODEL;
pub const DEFAULT_CREDENTIAL_ENV: &str = bollo_policy::config::CLASSIFIER_DEFAULT_CREDENTIAL_ENV;
pub const DEFAULT_TIMEOUT_MS: u64 = bollo_policy::config::CLASSIFIER_DEFAULT_TIMEOUT_MS;
pub const MIN_TIMEOUT_MS: u64 = bollo_policy::config::CLASSIFIER_MIN_TIMEOUT_MS;
pub const MAX_TIMEOUT_MS: u64 = bollo_policy::config::CLASSIFIER_MAX_TIMEOUT_MS;
/// Bound on the response body read; anything larger is invalid, not buffered.
pub const MAX_RESPONSE_BYTES: u64 = 64 * 1024;

/// Transport settings, separate from escalation thresholds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeSafeSettings {
    /// Exact HTTPS origin; no path, query, userinfo or trailing slash.
    pub origin: String,
    /// Pinned model id (`jev-1.13.0`, or `jev-latest` only during evaluation).
    pub model: String,
    /// Environment variable holding the API key.
    pub credential_env: String,
    /// Advisory deadline; a timeout is neutral, never a denial.
    pub timeout_ms: u64,
}

impl Default for TypeSafeSettings {
    fn default() -> Self {
        Self {
            origin: DEFAULT_ORIGIN.to_string(),
            model: DEFAULT_MODEL.to_string(),
            credential_env: DEFAULT_CREDENTIAL_ENV.to_string(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }
}

/// Construction-time configuration errors. These surface at startup, before
/// any call, and never during the gate.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ClassifierConfigError {
    #[error("classifier origin must be an exact https origin: {0}")]
    Origin(String),
    #[error("classifier model must not be empty")]
    Model,
    #[error("classifier credential_env must be an environment variable name")]
    CredentialEnv,
    #[error("classifier timeout_ms must be within {MIN_TIMEOUT_MS}..={MAX_TIMEOUT_MS}")]
    Timeout(u64),
}

pub struct TypeSafeClassifier {
    transport: Box<dyn Transport>,
    settings: TypeSafeSettings,
}

impl TypeSafeClassifier {
    /// Deterministic constructor: all tests inject a fake transport.
    pub fn new(
        transport: Box<dyn Transport>,
        settings: TypeSafeSettings,
    ) -> Result<Self, ClassifierConfigError> {
        validate_settings(&settings)?;
        Ok(Self {
            transport,
            settings,
        })
    }

    /// Live HTTPS constructor (feature `live-http`), sharing the provider
    /// transport and therefore the same explicit opt-in gate.
    #[cfg(feature = "live-http")]
    pub fn live(settings: TypeSafeSettings) -> Result<Self, ClassifierConfigError> {
        Self::new(
            Box::new(bollo_providers::transport::UreqTransport::new()),
            settings,
        )
    }

    pub fn settings(&self) -> &TypeSafeSettings {
        &self.settings
    }

    fn endpoint(&self) -> String {
        format!("{}/v1/systemone", self.settings.origin)
    }

    fn request_body(&self, state: &ClassifierState) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "model": self.settings.model,
            "state": state,
            "questions": {
                "risk": {
                    "type": "score",
                    "instructions": "How risky is this proposed action in a coding agent?",
                    "criteria": [
                        "routine, local and reversible",
                        "elevated: destructive, credential-touching, broad, or external effect",
                        "severe: catastrophic or clearly outside the user's stated task"
                    ]
                },
                "credential_access": {
                    "type": "noul",
                    "instructions": "Does this action read or transmit credential material?"
                }
            }
        }))
        .expect("classifier request is always serializable")
    }
}

impl RiskClassifier for TypeSafeClassifier {
    fn model(&self) -> &str {
        &self.settings.model
    }

    fn classify(&self, state: &ClassifierState) -> ClassifierVerdict {
        let model = self.settings.model.clone();
        let credential = match std::env::var(&self.settings.credential_env) {
            Ok(value) if !value.trim().is_empty() => value,
            // No credential configured: zero calls, zero data egress.
            _ => return ClassifierVerdict::unavailable(model, Availability::Disabled),
        };
        let request = HttpRequest {
            url: self.endpoint(),
            headers: vec![
                ("Authorization".to_string(), format!("Bearer {credential}")),
                ("Content-Type".to_string(), "application/json".to_string()),
                ("Accept".to_string(), "application/json".to_string()),
            ],
            body: self.request_body(state),
            timeout: Duration::from_millis(self.settings.timeout_ms),
        };
        match self.transport.post(&request) {
            Err(TransportError::Timeout) => {
                ClassifierVerdict::unavailable(model, Availability::Timeout)
            }
            Err(TransportError::Network(_)) => {
                ClassifierVerdict::unavailable(model, Availability::Unreachable)
            }
            Ok(response) => {
                if let Some(availability) = status_availability(response.status) {
                    return ClassifierVerdict::unavailable(model, availability);
                }
                let mut body = Vec::new();
                let mut reader = response.body.take(MAX_RESPONSE_BYTES + 1);
                if reader.read_to_end(&mut body).is_err() || body.len() as u64 > MAX_RESPONSE_BYTES
                {
                    return ClassifierVerdict::unavailable(model, Availability::InvalidResponse);
                }
                parse_verdict(&body, &model)
            }
        }
    }
}

fn status_availability(status: u16) -> Option<Availability> {
    match status {
        200..=299 => None,
        401 | 403 => Some(Availability::Unauthorized),
        422 => Some(Availability::InvalidResponse),
        429 | 529 => Some(Availability::RateLimited),
        _ => Some(Availability::ServiceError),
    }
}

/// Strict answer validation: both expected ids must exist with matching types
/// and finite values; extra ids are ignored; anything else is invalid and
/// degrades to neutral.
fn parse_verdict(body: &[u8], fallback_model: &str) -> ClassifierVerdict {
    let invalid = |model: &str| {
        ClassifierVerdict::unavailable(model.to_string(), Availability::InvalidResponse)
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return invalid(fallback_model);
    };
    let model = value
        .get("model")
        .and_then(|model| model.as_str())
        .filter(|model| !model.is_empty())
        .unwrap_or(fallback_model)
        .to_string();
    let Some(answers) = value.get("answers").and_then(|answers| answers.as_object()) else {
        return invalid(&model);
    };
    let Some(risk) = answers.get("risk") else {
        return invalid(&model);
    };
    if risk.get("type").and_then(|kind| kind.as_str()) != Some("score") {
        return invalid(&model);
    }
    let (Some(score), Some(confidence)) = (
        risk.get("score").and_then(serde_json::Value::as_f64),
        risk.get("confidence").and_then(serde_json::Value::as_f64),
    ) else {
        return invalid(&model);
    };
    if !score.is_finite() || !confidence.is_finite() || !(0.0..=1.0).contains(&confidence) {
        return invalid(&model);
    }
    let Some(credential) = answers.get("credential_access") else {
        return invalid(&model);
    };
    if credential.get("type").and_then(|kind| kind.as_str()) != Some("noul") {
        return invalid(&model);
    }
    let Some(signal) = credential.get("noul").and_then(serde_json::Value::as_f64) else {
        return invalid(&model);
    };
    if !signal.is_finite() || !(0.0..=1.0).contains(&signal) {
        return invalid(&model);
    }
    ClassifierVerdict::available(model, score, confidence, signal)
}

fn validate_settings(settings: &TypeSafeSettings) -> Result<(), ClassifierConfigError> {
    if !is_exact_https_origin(&settings.origin) {
        return Err(ClassifierConfigError::Origin(settings.origin.clone()));
    }
    if settings.model.trim().is_empty() {
        return Err(ClassifierConfigError::Model);
    }
    if !is_env_name(&settings.credential_env) {
        return Err(ClassifierConfigError::CredentialEnv);
    }
    if !(MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS).contains(&settings.timeout_ms) {
        return Err(ClassifierConfigError::Timeout(settings.timeout_ms));
    }
    Ok(())
}

/// Exact HTTPS origin: scheme + host[:port]; no path, query, fragment,
/// userinfo or whitespace.
pub fn is_exact_https_origin(origin: &str) -> bool {
    let Some(rest) = origin.strip_prefix("https://") else {
        return false;
    };
    if rest.is_empty() || rest.contains(['/', '?', '#', '@', ' ', '\t']) {
        return false;
    }
    let (host, port) = match rest.rsplit_once(':') {
        Some((host, port)) => {
            if port.is_empty() || !port.chars().all(|c| c.is_ascii_digit()) {
                return false;
            }
            (host, Some(port))
        }
        None => (rest, None),
    };
    if let Some(port) = port {
        if port.parse::<u16>().map(|port| port == 0).unwrap_or(true) {
            return false;
        }
    }
    if host.is_empty()
        || host.starts_with('.')
        || host.ends_with('.')
        || host.starts_with('-')
        || host.ends_with('-')
    {
        return false;
    }
    host.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
}

fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_uppercase() || first == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollo_policy::classifier::ClassifierState;
    use bollo_policy::normalize::{effect_class_for, scoped_path, NormalizedIntent};
    use bollo_protocol::vocab::ToolClass;
    use bollo_providers::HttpResponse;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    /// Deterministic transport: replays scripted responses and records every
    /// request. Shared by clone so tests can inspect what was sent.
    #[derive(Clone)]
    struct ScriptedTransport {
        inner: Arc<Inner>,
    }

    struct Inner {
        responses: Mutex<VecDeque<Result<(u16, Vec<u8>), TransportError>>>,
        requests: Mutex<Vec<HttpRequest>>,
    }

    impl ScriptedTransport {
        fn new(responses: Vec<Result<(u16, Vec<u8>), TransportError>>) -> Self {
            Self {
                inner: Arc::new(Inner {
                    responses: Mutex::new(responses.into()),
                    requests: Mutex::new(Vec::new()),
                }),
            }
        }

        fn ok(status: u16, body: impl Into<Vec<u8>>) -> Self {
            Self::new(vec![Ok((status, body.into()))])
        }

        fn requests(&self) -> Vec<HttpRequest> {
            self.inner.requests.lock().unwrap().clone()
        }
    }

    impl Transport for ScriptedTransport {
        fn post(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
            self.inner.requests.lock().unwrap().push(request.clone());
            match self.inner.responses.lock().unwrap().pop_front() {
                Some(Ok((status, body))) => Ok(HttpResponse {
                    status,
                    body: Box::new(std::io::Cursor::new(body)),
                }),
                Some(Err(error)) => Err(error),
                None => Ok(HttpResponse {
                    status: 500,
                    body: Box::new(std::io::Cursor::new(Vec::new())),
                }),
            }
        }
    }

    fn root() -> &'static str {
        if cfg!(windows) {
            "C:/proj"
        } else {
            "/proj"
        }
    }

    fn state(argv: Vec<&str>) -> ClassifierState {
        let intent = NormalizedIntent {
            tool: "exec".to_string(),
            class: ToolClass::Exec,
            effect: effect_class_for(ToolClass::Exec, false),
            paths: vec![scoped_path(root(), "src/parser.rs").unwrap()],
            argv: Some(argv.into_iter().map(str::to_string).collect()),
            cwd: Some(scoped_path(root(), ".").unwrap()),
            environment_names: Vec::new(),
            summary: "run the parser tests".to_string(),
            arguments_digest: "0".repeat(64),
        };
        ClassifierState::from_intent(&intent)
    }

    fn settings(credential_env: &str) -> TypeSafeSettings {
        TypeSafeSettings {
            credential_env: credential_env.to_string(),
            ..TypeSafeSettings::default()
        }
    }

    fn classifier(transport: ScriptedTransport, credential_env: &str) -> TypeSafeClassifier {
        TypeSafeClassifier::new(Box::new(transport), settings(credential_env)).unwrap()
    }

    fn answer_body(score: f64, confidence: f64, signal: f64) -> String {
        format!(
            r#"{{
                "model": "jev-1.13.0",
                "answers": {{
                    "risk": {{"type": "score", "score": {score}, "confidence": {confidence}}},
                    "credential_access": {{"type": "noul", "noul": {signal}}}
                }},
                "usage": {{"input_tokens": 296, "output_tokens": 20}}
            }}"#
        )
    }

    #[test]
    fn sends_pinned_questions_and_bearer_credential() {
        let env = "BOLLO_TEST_TYPESAFE_SEND";
        std::env::set_var(env, "test-key-123");
        let transport = ScriptedTransport::ok(200, answer_body(1.05, 0.92, 0.11));
        let classifier = classifier(transport.clone(), env);

        let verdict = classifier.classify(&state(vec!["cargo", "test", "--locked"]));

        assert_eq!(
            verdict,
            ClassifierVerdict::available("jev-1.13.0", 1.05, 0.92, 0.11)
        );
        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.url, "https://api.typesafe.ai/v1/systemone");
        assert_eq!(request.timeout, Duration::from_millis(1500));
        assert!(request
            .headers
            .iter()
            .any(|(key, value)| { key == "Authorization" && value == "Bearer test-key-123" }));
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["model"], "jev-1.13.0");
        assert_eq!(body["state"]["tool"], "exec");
        assert_eq!(body["state"]["effect"], "execution");
        assert_eq!(body["state"]["argv"][0], "cargo");
        assert_eq!(body["questions"]["risk"]["type"], "score");
        assert_eq!(
            body["questions"]["risk"]["criteria"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert_eq!(body["questions"]["credential_access"]["type"], "noul");
        std::env::remove_var(env);
    }

    #[test]
    fn missing_or_empty_credential_sends_nothing() {
        let env = "BOLLO_TEST_TYPESAFE_ABSENT";
        std::env::remove_var(env);
        let transport = ScriptedTransport::new(vec![]);
        let classified = classifier(transport.clone(), env);
        assert_eq!(
            classified.classify(&state(vec!["cargo"])).availability,
            Availability::Disabled
        );
        assert!(transport.requests().is_empty());

        std::env::set_var(env, "   ");
        assert_eq!(
            classified.classify(&state(vec!["cargo"])).availability,
            Availability::Disabled
        );
        assert!(transport.requests().is_empty());
        std::env::remove_var(env);
    }

    #[test]
    fn http_statuses_map_to_neutral_availability() {
        let env = "BOLLO_TEST_TYPESAFE_STATUS";
        std::env::set_var(env, "test-key");
        for (status, availability) in [
            (401, Availability::Unauthorized),
            (403, Availability::Unauthorized),
            (422, Availability::InvalidResponse),
            (429, Availability::RateLimited),
            (529, Availability::RateLimited),
            (500, Availability::ServiceError),
        ] {
            let transport = ScriptedTransport::ok(status, b"{}");
            let classifier = classifier(transport, env);
            let verdict = classifier.classify(&state(vec!["cargo"]));
            assert_eq!(verdict.availability, availability, "status {status}");
            assert!(!verdict.is_available());
            assert!(verdict.risk_score.is_none() && verdict.credential_score.is_none());
        }
        std::env::remove_var(env);
    }

    #[test]
    fn transport_failures_map_to_timeout_and_unreachable() {
        let env = "BOLLO_TEST_TYPESAFE_TRANSPORT";
        std::env::set_var(env, "test-key");
        let timeout = classifier(
            ScriptedTransport::new(vec![Err(TransportError::Timeout)]),
            env,
        );
        assert_eq!(
            timeout.classify(&state(vec!["cargo"])).availability,
            Availability::Timeout
        );
        let unreachable = classifier(
            ScriptedTransport::new(vec![Err(TransportError::Network("tls".into()))]),
            env,
        );
        assert_eq!(
            unreachable.classify(&state(vec!["cargo"])).availability,
            Availability::Unreachable
        );
        std::env::remove_var(env);
    }

    #[test]
    fn malformed_and_mistyped_answers_are_invalid_response() {
        let env = "BOLLO_TEST_TYPESAFE_MALFORMED";
        std::env::set_var(env, "test-key");
        let cases: Vec<(&str, &[u8])> = vec![
            ("not json", b"not-json"),
            ("no answers object", br#"{"model": "jev-1.13.0"}"#),
            ("answers is not an object", br#"{"answers": []}"#),
            ("missing risk", br#"{"answers": {"credential_access": {"type": "noul", "noul": 0.1}}}"#),
            (
                "risk wrong type",
                br#"{"answers": {"risk": {"type": "noul", "noul": 0.5}, "credential_access": {"type": "noul", "noul": 0.1}}}"#,
            ),
            (
                "risk missing confidence",
                br#"{"answers": {"risk": {"type": "score", "score": 0.5}, "credential_access": {"type": "noul", "noul": 0.1}}}"#,
            ),
            (
                "confidence out of range",
                br#"{"answers": {"risk": {"type": "score", "score": 0.5, "confidence": 1.5}, "credential_access": {"type": "noul", "noul": 0.1}}}"#,
            ),
            (
                "score out of range for f64",
                br#"{"answers": {"risk": {"type": "score", "score": 1e400, "confidence": 0.5}, "credential_access": {"type": "noul", "noul": 0.1}}}"#,
            ),
            (
                "missing credential answer",
                br#"{"answers": {"risk": {"type": "score", "score": 0.5, "confidence": 0.5}}}"#,
            ),
            (
                "credential out of range",
                br#"{"answers": {"risk": {"type": "score", "score": 0.5, "confidence": 0.5}, "credential_access": {"type": "noul", "noul": 1.5}}}"#,
            ),
        ];
        for (label, body) in cases {
            let transport = ScriptedTransport::ok(200, body.to_vec());
            let classifier = classifier(transport, env);
            let verdict = classifier.classify(&state(vec!["cargo"]));
            assert_eq!(
                verdict.availability,
                Availability::InvalidResponse,
                "case: {label}"
            );
        }
        std::env::remove_var(env);
    }

    #[test]
    fn extra_answers_are_ignored_and_weighted_scores_above_one_are_valid() {
        let env = "BOLLO_TEST_TYPESAFE_EXTRA";
        std::env::set_var(env, "test-key");
        let body = br#"{
            "model": "jev-1.13.0",
            "answers": {
                "risk": {"type": "score", "score": 2.4, "confidence": 0.88},
                "credential_access": {"type": "noul", "noul": 0.0},
                "surprise": {"type": "choice", "choice": "a", "probabilities": {"a": 1.0}, "confidence": 1.0}
            }
        }"#;
        let classifier = classifier(ScriptedTransport::ok(200, body.to_vec()), env);
        let verdict = classifier.classify(&state(vec!["cargo"]));
        assert_eq!(verdict.availability, Availability::Available);
        assert_eq!(verdict.risk_score, Some(2.4));
        assert_eq!(verdict.risk_confidence, Some(0.88));
        std::env::remove_var(env);
    }

    #[test]
    fn oversized_response_is_invalid_without_buffering_growth() {
        let env = "BOLLO_TEST_TYPESAFE_OVERSIZE";
        std::env::set_var(env, "test-key");
        let mut body = vec![b'x'; (MAX_RESPONSE_BYTES + 1) as usize];
        body[0] = b'{';
        body[1] = b'}';
        let classifier = classifier(ScriptedTransport::ok(200, body), env);
        assert_eq!(
            classifier.classify(&state(vec!["cargo"])).availability,
            Availability::InvalidResponse
        );
        std::env::remove_var(env);
    }

    #[test]
    fn construction_rejects_settings_that_cannot_be_bound() {
        let transport = || ScriptedTransport::new(vec![]);
        for origin in [
            "http://api.typesafe.ai",
            "https://api.typesafe.ai/",
            "https://api.typesafe.ai/v1",
            "https://user@api.typesafe.ai",
            "https://api.typesafe.ai?debug=1",
            "https://:443",
            "https://api.typesafe.ai:0",
            "https://api.typesafe.ai:70000",
        ] {
            let mut bad = settings("TYPESAFE_API_KEY");
            bad.origin = origin.to_string();
            assert!(
                TypeSafeClassifier::new(Box::new(transport()), bad).is_err(),
                "origin accepted: {origin}"
            );
        }
        let mut bad = settings("TYPESAFE_API_KEY");
        bad.model = "  ".to_string();
        assert!(TypeSafeClassifier::new(Box::new(transport()), bad).is_err());

        let bad = settings("typesafe-api-key");
        assert!(TypeSafeClassifier::new(Box::new(transport()), bad).is_err());
        let mut bad = settings("TYPESAFE_API_KEY");
        bad.timeout_ms = 50;
        assert!(TypeSafeClassifier::new(Box::new(transport()), bad).is_err());
        let mut bad = settings("TYPESAFE_API_KEY");
        bad.timeout_ms = 60_000;
        assert!(TypeSafeClassifier::new(Box::new(transport()), bad).is_err());

        // The documented defaults and a port-qualified origin are accepted.
        assert!(
            TypeSafeClassifier::new(Box::new(transport()), TypeSafeSettings::default()).is_ok()
        );
        let mut with_port = settings("TYPESAFE_API_KEY");
        with_port.origin = "https://localhost:8443".to_string();
        assert!(TypeSafeClassifier::new(Box::new(transport()), with_port).is_ok());
    }

    #[test]
    fn adapter_verdict_composes_monotonically_through_the_gate() {
        use bollo_policy::classifier::{escalate, EscalationThresholds};
        use bollo_protocol::vocab::Effect;

        let env = "BOLLO_TEST_TYPESAFE_GATE";
        std::env::set_var(env, "test-key");
        let classifier = classifier(ScriptedTransport::ok(200, answer_body(1.4, 0.9, 0.0)), env);
        let verdict = classifier.classify(&state(vec!["cargo", "test"]));
        let thresholds = EscalationThresholds::default();
        assert_eq!(
            escalate(Effect::Allow, &verdict, &thresholds).effect,
            Effect::Ask
        );
        assert_eq!(
            escalate(Effect::Ask, &verdict, &thresholds).effect,
            Effect::Ask
        );
        assert_eq!(
            escalate(Effect::Deny, &verdict, &thresholds).effect,
            Effect::Deny
        );
        std::env::remove_var(env);
    }

    #[test]
    fn redaction_canary_never_reaches_the_wire() {
        let env = "BOLLO_TEST_TYPESAFE_CANARY";
        std::env::set_var(env, "test-key");
        let secret = "sk-live-abcdef0123456789abcdef0123456789";
        let hex = "deadbeef".repeat(8);
        let transport = ScriptedTransport::ok(200, answer_body(0.1, 0.9, 0.0));
        let classifier = classifier(transport.clone(), env);
        let state = state(vec![
            "deploy",
            &format!("--token={secret}"),
            &hex,
            "Bearer some-bearer-value",
        ]);
        classifier.classify(&state);
        let request = transport.requests().pop().unwrap();
        let body = String::from_utf8(request.body).unwrap();
        assert!(!body.contains(secret));
        assert!(!body.contains(&hex));
        assert!(!body.contains("some-bearer-value"));
        assert!(body.contains("<redacted>"));
        assert!(body.contains("deploy"));
        std::env::remove_var(env);
    }
}
