//! Negative suite: the cases where the answer must be "no". These assert the
//! precedence and ceiling rules that the whole design rests on.

use bollo_modes::{constrain, descriptor};
use bollo_policy::layers::{build_snapshot, CliOverrides};
use bollo_policy::{evaluate, PolicySnapshot};
use bollo_protocol::vocab::{
    Effect, EffectClass, ModeKind, Platform, SandboxCapabilities, ToolClass,
};
use bollo_protocol::{EventEnvelope, EventType};
use bollo_tools::prepare;
use bollo_workspace::{WorkspaceFs, WorkspaceRoot};
use serde_json::json;

fn caps() -> SandboxCapabilities {
    SandboxCapabilities {
        platform: Platform::Linux,
        filesystem_containment: true,
        network_denied: true,
        backend: "test-probe".into(),
    }
}

fn snapshot(profile: &str, rules_json: &str) -> PolicySnapshot {
    let json = format!(
        r#"{{
            "schema_version": "0.1",
            "provider": {{
                "kind": "anthropic",
                "base_url": "https://api.anthropic.com",
                "model": "test-model",
                "credential_env": "ANTHROPIC_API_KEY",
                "context_tokens": 20000
            }},
            "permissions": {{ "profile": "{profile}", "sandbox": "off", "rules": {rules_json} }},
            "limits": {{ "max_spend_cents": null }},
            "privacy": {{ "telemetry": false, "content_retention_days": 7 }},
            "mcp_servers": [],
            "hooks": []
        }}"#
    );
    let user = bollo_policy::parse_user_config(&json).unwrap();
    build_snapshot(&user, None, &CliOverrides::default(), caps(), 1).unwrap()
}

fn fixture_fs() -> (tempfile::TempDir, WorkspaceFs, String) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
    let root = WorkspaceRoot::discover(dir.path()).unwrap();
    let fs = WorkspaceFs::new(&root);
    let policy_root = fs.policy_root();
    (dir, fs, policy_root)
}

#[test]
fn deny_beats_ask_and_allow_for_the_same_intent() {
    let (_dir, fs, root) = fixture_fs();
    let snap = snapshot(
        "balanced",
        r#"[
            {"id": "a-allow", "effect": "allow", "tool": "write_file"},
            {"id": "b-ask", "effect": "ask", "tool": "write_file"},
            {"id": "c-deny", "effect": "deny", "tool": "write_file"}
        ]"#,
    );
    let prepared = prepare(
        "write_file",
        &json!({"path": "a.txt", "content": "x", "expected_sha256": fs.current_sha256("a.txt").unwrap()}),
        &fs,
        &root,
    )
    .unwrap();
    let decision = evaluate(&prepared.intent, &snap);
    assert_eq!(decision.effect, Effect::Deny);
    assert_eq!(decision.rule_id.as_deref(), Some("c-deny"));
}

#[test]
fn read_only_is_an_inspection_ceiling_no_rule_can_unlock() {
    let (_dir, fs, root) = fixture_fs();
    let snap = snapshot(
        "read_only",
        r#"[{"id": "let-me-write", "effect": "allow", "tool": "write_file"}]"#,
    );
    let prepared = prepare(
        "write_file",
        &json!({"path": "new.txt", "content": "x", "expected_sha256": null}),
        &fs,
        &root,
    )
    .unwrap();
    let decision = evaluate(&prepared.intent, &snap);
    assert_eq!(decision.effect, Effect::Deny);
    assert!(decision.reason.contains("inspection ceiling"));
}

#[test]
fn an_explicit_ask_stays_an_ask_under_unrestricted() {
    let (_dir, fs, root) = fixture_fs();
    let snap = snapshot(
        "unrestricted",
        r#"[{"id": "ask-mutations", "effect": "ask", "tool": "write_file"}]"#,
    );
    let prepared = prepare(
        "write_file",
        &json!({"path": "new.txt", "content": "x", "expected_sha256": null}),
        &fs,
        &root,
    )
    .unwrap();
    let decision = evaluate(&prepared.intent, &snap);
    assert_eq!(decision.effect, Effect::Ask, "unrestricted is not 'auto-yes'");
}

#[test]
fn project_configuration_cannot_widen_the_profile() {
    let json = r#"{
        "schema_version": "0.1",
        "provider": {
            "kind": "anthropic",
            "base_url": "https://api.anthropic.com",
            "model": "test-model",
            "credential_env": "ANTHROPIC_API_KEY",
            "context_tokens": 20000
        },
        "permissions": { "profile": "read_only", "sandbox": "off" },
        "limits": { "max_spend_cents": null },
        "privacy": { "telemetry": false, "content_retention_days": 7 },
        "mcp_servers": [],
        "hooks": []
    }"#;
    let user = bollo_policy::parse_user_config(json).unwrap();
    let project = bollo_policy::parse_project_config(
        r#"{"schema_version":"0.1","permissions":{"profile":"unrestricted"}}"#,
    )
    .unwrap();
    let result = build_snapshot(
        &user,
        Some(&project),
        &CliOverrides::default(),
        caps(),
        1,
    );
    assert!(result.is_err(), "a project may restrict but never widen");
}

#[test]
fn outside_workspace_intents_are_namespaced_never_rewritten() {
    let (_dir, fs, root) = fixture_fs();
    // The path is namespaced as `outside:`, not silently rewritten to something
    // inside the workspace, and the effect class records that fact.
    let prepared = prepare(
        "read_file",
        &json!({"path": "../escape.txt"}),
        &fs,
        &root,
    )
    .unwrap();
    assert_eq!(prepared.intent.effect, EffectClass::OutsideWorkspace);
    assert!(prepared.intent.paths.iter().any(|path| path.is_outside()));

    // Balanced asks for explicit scope; workspace_auto denies it outright.
    let balanced = snapshot("balanced", "[]");
    assert_eq!(evaluate(&prepared.intent, &balanced).effect, Effect::Ask);
    let auto = snapshot("workspace_auto", "[]");
    assert_eq!(evaluate(&prepared.intent, &auto).effect, Effect::Deny);

    // Inside paths still normalize to an ordinary read that balanced allows.
    let inside = prepare("read_file", &json!({"path": "a.txt"}), &fs, &root).unwrap();
    assert_eq!(inside.intent.effect, EffectClass::Read);
    assert_eq!(evaluate(&inside.intent, &balanced).effect, Effect::Allow);
}

#[test]
fn modes_only_tighten_policy() {
    let (_dir, fs, root) = fixture_fs();
    let snap = snapshot("unrestricted", "[]");
    let prepared = prepare(
        "exec",
        &json!({"argv": ["git", "status"], "cwd": ".", "timeout_seconds": 10}),
        &fs,
        &root,
    )
    .unwrap();
    // Policy says allow under unrestricted…
    assert_eq!(evaluate(&prepared.intent, &snap).effect, Effect::Allow);
    // …but inspect still denies execution, and does so as a mode denial.
    let decision = constrain(
        &descriptor(ModeKind::Inspect),
        prepared.class,
        prepared.intent.effect,
        Effect::Allow,
    );
    assert_eq!(decision.effect, Effect::Deny);
    assert!(decision.mode_denied);
    // Build mode keeps the policy answer.
    let build = constrain(
        &descriptor(ModeKind::Build),
        ToolClass::Exec,
        EffectClass::Execution,
        Effect::Allow,
    );
    assert_eq!(build.effect, Effect::Allow);
    assert!(!build.mode_denied);
}

#[test]
fn event_shapes_are_closed() {
    // Unknown event types do not deserialize.
    let unknown = r#"{"schema_version":"0.1","event_id":"evt_1","session_id":"sess_1","seq":1,
        "timestamp":"2026-10-03T00:00:00Z","type":"tool.magic","data":{}}"#;
    assert!(serde_json::from_str::<EventEnvelope>(unknown).is_err());

    // A known type with the wrong data shape is rejected by validation.
    let session = bollo_protocol::ids::SessionId::generate();
    let event = EventEnvelope::new(
        &session,
        None,
        1,
        EventType::ApprovalRequested,
        &json!({"unexpected": true}),
    );
    assert!(event.is_err(), "approval.requested requires its documented fields");
}
