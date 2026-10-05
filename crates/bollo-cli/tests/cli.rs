//! CLI contract tests. These run the real `bollo` binary and assert the
//! documented interface: stdout carries only the selected format, diagnostics
//! go to stderr, and the exit codes match `docs/reference/cli.md`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use bollo_protocol::events::EventEnvelope;
use bollo_protocol::ids::SessionId;
use bollo_store::SqliteStore;

const BINARY: &str = env!("CARGO_BIN_EXE_bollo");

struct Fixture {
    _root: tempfile::TempDir,
    workspace: PathBuf,
    state: PathBuf,
    config: PathBuf,
    script: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("ws");
        let state = root.path().join("state");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        let config = root.path().join("config.json");
        std::fs::write(
            &config,
            r#"{
                "schema_version": "0.1",
                "provider": {
                    "kind": "anthropic",
                    "base_url": "https://api.anthropic.com",
                    "model": "test-model",
                    "credential_env": "ANTHROPIC_API_KEY",
                    "context_tokens": 20000,
                    "input_microusd_per_token": 3,
                    "output_microusd_per_token": 15
                },
                "permissions": { "profile": "balanced", "sandbox": "off", "rules": [] },
                "limits": {
                    "max_tool_calls": 10,
                    "max_run_seconds": 120,
                    "max_output_tokens": 1024,
                    "max_spend_cents": 500,
                    "tool_output_bytes": 65536
                },
                "privacy": { "telemetry": false, "content_retention_days": 7 },
                "mcp_servers": [],
                "hooks": []
            }"#,
        )
        .unwrap();
        let script = root.path().join("script.json");
        std::fs::write(&script, "[]").unwrap();
        let fixture = Self {
            _root: root,
            workspace,
            state,
            config,
            script,
        };
        fixture.seed("hello.txt", "hello from the fixture\n");
        fixture
    }

    fn seed(&self, path: &str, content: &str) -> &Self {
        let target = self.workspace.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(target, content).unwrap();
        self
    }

    fn write_script(&self, json: &str) -> &Self {
        std::fs::write(&self.script, json).unwrap();
        self
    }

    /// Replace the classifier block in the trusted configuration, keeping the
    /// rest of the fixture config intact.
    fn set_classifier(&self, block: &str) -> &Self {
        let mut value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&self.config).unwrap()).unwrap();
        value["classifier"] = serde_json::from_str(block).unwrap();
        std::fs::write(&self.config, serde_json::to_string_pretty(&value).unwrap()).unwrap();
        self
    }

    fn base_args(&self) -> Vec<String> {
        vec![
            "--workspace".into(),
            self.workspace.display().to_string(),
            "--state-dir".into(),
            self.state.display().to_string(),
        ]
    }

    fn output(&self, args: &[&str]) -> std::process::Output {
        let mut command = Command::new(BINARY);
        command
            .args(self.base_args())
            .args(args)
            .env("BOLLO_CONFIG", &self.config)
            .stdin(Stdio::null());
        command.output().unwrap()
    }

    fn script_arg(&self) -> String {
        self.script.display().to_string()
    }

    /// Arguments for `run`/`resume` that always pass the absolute script path.
    fn replay_args(&self, extra: &[&str]) -> Vec<String> {
        let mut args: Vec<String> = [
            "--sandbox",
            "off",
            "--acknowledge-risk",
            "--provider",
            "replay",
            "--script",
            self.script_arg().as_str(),
        ]
        .iter()
        .map(|value| value.to_string())
        .collect();
        args.extend(extra.iter().map(|value| value.to_string()));
        args
    }

    fn run_replay(&self, profile: &[&str], output: &str) -> std::process::Output {
        let mut extra: Vec<&str> = vec!["run"];
        let replay = self.replay_args(&[]);
        let mut args: Vec<String> = vec!["run".to_string()];
        args.extend(replay);
        args.push("--output".into());
        args.push(output.into());
        args.push("--prompt".into());
        args.push("do the thing".into());
        args.extend(profile.iter().map(|value| value.to_string()));
        extra.clear();
        self.output(&args.iter().map(String::as_str).collect::<Vec<_>>())
    }
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn help_and_version_exit_zero() {
    let fixture = Fixture::new();
    let help = fixture.output(&["--help"]);
    assert_eq!(help.status.code(), Some(0));
    assert!(stdout(&help).contains("bollo"));
    let version = fixture.output(&["--version"]);
    assert_eq!(version.status.code(), Some(0));
}

#[test]
fn invalid_arguments_exit_2() {
    let fixture = Fixture::new();
    let unknown = fixture.output(&["--definitely-not-a-flag"]);
    assert_eq!(unknown.status.code(), Some(2));
    let script = fixture.script_arg();
    let missing_prompt = fixture.output(&[
        "run",
        "--sandbox",
        "off",
        "--acknowledge-risk",
        "--provider",
        "replay",
        "--script",
        &script,
    ]);
    assert_eq!(missing_prompt.status.code(), Some(2));
    assert!(stderr(&missing_prompt).contains("--prompt"));
}

#[test]
fn headless_ndjson_stdout_is_machine_only() {
    let fixture = Fixture::new();
    fixture.write_script(
        r#"[
            {"deltas": [], "tool_calls": [{"call_id": "c1", "name": "read_file", "arguments": {"path": "hello.txt"}}], "finish": "tool_use"},
            {"deltas": ["all done\n"], "finish": "end_turn"}
        ]"#,
    );
    let output = fixture.run_replay(&[], "ndjson");
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    assert!(!lines.is_empty());
    for line in &lines {
        let event: EventEnvelope =
            serde_json::from_str(line).unwrap_or_else(|err| panic!("not an event: {err}: {line}"));
        assert_eq!(event.schema_version, "0.1");
    }
    assert!(
        !stdout(&output).contains('\u{1b}'),
        "no ANSI escapes on stdout"
    );
    assert!(
        stderr(&output).contains("verification is reported separately"),
        "diagnostics belong on stderr"
    );
    assert!(stdout(&output).contains("\"type\":\"run_finished\""));
}

#[test]
fn headless_ask_blocks_with_exit_3() {
    let fixture = Fixture::new();
    fixture.write_script(
        r#"[
            {"deltas": [], "tool_calls": [{"call_id": "w1", "name": "write_file", "arguments": {"path": "new.txt", "content": "nope\n", "expected_sha256": null}}], "finish": "tool_use"}
        ]"#,
    );
    let output = fixture.run_replay(&[], "ndjson");
    assert_eq!(output.status.code(), Some(3));
    assert!(!fixture.workspace.join("new.txt").exists());
    assert!(stdout(&output).contains("\"reason\":\"approval_required\""));
}

#[test]
fn interactive_without_a_terminal_is_exit_2() {
    let fixture = Fixture::new();
    let output = fixture.output(&[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("needs a terminal"));
}

#[test]
fn policy_show_and_explain_are_read_only() {
    let fixture = Fixture::new();
    let show = fixture.output(&["policy", "show"]);
    assert_eq!(show.status.code(), Some(0), "stderr: {}", stderr(&show));
    let text = stdout(&show);
    assert!(text.contains("profile        balanced"));
    assert!(text.contains("sandbox        off"));
    assert!(text.contains("provenance:"));

    let intent = fixture._root.path().join("intent.json");
    std::fs::write(
        &intent,
        r#"{"tool": "write_file", "arguments": {"path": "x.txt", "content": "y", "expected_sha256": null}}"#,
    )
    .unwrap();
    let explain = fixture.output(&[
        "policy",
        "explain",
        "--intent",
        &intent.display().to_string(),
    ]);
    assert_eq!(
        explain.status.code(),
        Some(0),
        "stderr: {}",
        stderr(&explain)
    );
    let text = stdout(&explain);
    assert!(text.contains("policy_effect  ask"), "{text}");
    assert!(text.contains("mode           build → ask"), "{text}");
    assert!(text.contains("nothing was executed"));
    assert!(!fixture.workspace.join("x.txt").exists());
}

#[test]
fn sessions_list_export_and_delete() {
    let fixture = Fixture::new();
    fixture.write_script(r#"[{"deltas": ["hello\n"], "finish": "end_turn"}]"#);
    let run = fixture.run_replay(&[], "ndjson");
    assert_eq!(run.status.code(), Some(0));

    let list = fixture.output(&["sessions", "list"]);
    assert_eq!(list.status.code(), Some(0));
    let text = stdout(&list);
    let session = text
        .lines()
        .next()
        .and_then(|line| line.split('\t').next())
        .expect("a session row")
        .to_string();
    assert!(session.starts_with("sess_"));

    let export = fixture._root.path().join("export.ndjson");
    let export_out = fixture.output(&[
        "sessions",
        "export",
        &session,
        "--output",
        &export.display().to_string(),
    ]);
    assert_eq!(
        export_out.status.code(),
        Some(0),
        "stderr: {}",
        stderr(&export_out)
    );
    let exported = std::fs::read_to_string(&export).unwrap();
    assert!(exported.lines().count() >= 3);
    for line in exported.lines() {
        serde_json::from_str::<EventEnvelope>(line).unwrap();
    }

    let refused = fixture.output(&["sessions", "delete", &session]);
    assert_eq!(refused.status.code(), Some(2), "deletion needs --yes");
    let deleted = fixture.output(&["sessions", "delete", &session, "--yes"]);
    assert_eq!(deleted.status.code(), Some(0));
    let after = fixture.output(&["sessions", "list"]);
    assert!(stdout(&after).contains("no local sessions"));
}

#[test]
fn config_validate_and_doctor_report_facts() {
    let fixture = Fixture::new();
    let validate = fixture.output(&["config", "validate"]);
    assert_eq!(
        validate.status.code(),
        Some(0),
        "stderr: {}",
        stderr(&validate)
    );
    assert!(stdout(&validate).contains("snapshot       revision 1 (valid)"));

    let doctor = fixture.output(&["doctor"]);
    assert_eq!(doctor.status.code(), Some(0), "stderr: {}", stderr(&doctor));
    let text = stdout(&doctor);
    assert!(text.contains("workspace"));
    assert!(text.contains("sandbox"));
    assert!(text.contains("store          schema_version=4"));
    assert!(text.contains("credential_env=ANTHROPIC_API_KEY present="));
    assert!(text.contains("no project code was executed"));
}

#[test]
fn classifier_is_disabled_by_default_and_strict_when_present() {
    let fixture = Fixture::new();
    let validate = fixture.output(&["config", "validate"]);
    assert_eq!(
        validate.status.code(),
        Some(0),
        "stderr: {}",
        stderr(&validate)
    );
    assert!(stdout(&validate).contains("classifier     disabled (zero calls)"));

    // A minimal disabled block changes nothing: still zero calls.
    fixture.set_classifier(r#"{"enabled": false}"#);
    let validate = fixture.output(&["config", "validate"]);
    assert_eq!(
        validate.status.code(),
        Some(0),
        "stderr: {}",
        stderr(&validate)
    );
    assert!(stdout(&validate).contains("classifier     disabled (zero calls)"));

    // Unsafe shapes are rejected by the strict parser before anything runs.
    for block in [
        r#"{"enabled": true, "origin": "http://api.typesafe.ai"}"#,
        r#"{"enabled": true, "origin": "https://api.typesafe.ai/v1"}"#,
        r#"{"enabled": true, "credential_env": "sk-live-secret-value"}"#,
        r#"{"enabled": true, "timeout_ms": 50}"#,
        r#"{"enabled": true, "profiles": []}"#,
    ] {
        fixture.set_classifier(block);
        let output = fixture.output(&["config", "validate"]);
        assert_eq!(output.status.code(), Some(2), "block {block} was accepted");
        assert!(
            stderr(&output).contains("invalid configuration"),
            "{block}: {}",
            stderr(&output)
        );
    }
}

#[test]
fn classifier_enabled_reports_posture_and_never_blocks_a_turn() {
    let fixture = Fixture::new();
    fixture.set_classifier(
        r#"{"enabled": true, "origin": "https://api.typesafe.ai", "model": "jev-1.13.0",
            "credential_env": "TYPESAFE_API_KEY", "timeout_ms": 1500, "escalate_at": 1.0,
            "min_confidence": 0.5, "credential_threshold": 0.9, "profiles": ["balanced"]}"#,
    );
    let validate = fixture.output(&["config", "validate"]);
    assert_eq!(
        validate.status.code(),
        Some(0),
        "stderr: {}",
        stderr(&validate)
    );
    let text = stdout(&validate);
    if cfg!(feature = "live-http") {
        assert!(
            text.contains("classifier     attached (model jev-1.13.0; profiles balanced"),
            "{text}"
        );
    } else {
        assert!(
            text.contains("classifier     enabled in config but detached"),
            "{text}"
        );
    }

    // The advisory classifier is fail-neutral and cannot block a turn; without
    // the live transport no classifier call is possible at all.
    fixture.write_script(r#"[{"deltas": ["ok\n"], "finish": "end_turn"}]"#);
    let run = fixture.run_replay(&[], "ndjson");
    assert_eq!(run.status.code(), Some(0), "stderr: {}", stderr(&run));
    assert!(
        stderr(&run).contains("classifier"),
        "missing classifier posture: {}",
        stderr(&run)
    );
}

#[test]
fn runs_list_reports_usage_and_classifier_audit_without_the_api() {
    let fixture = Fixture::new();
    fixture.write_script(
        r#"[
            {"deltas": [], "tool_calls": [{"call_id": "c1", "name": "read_file", "arguments": {"path": "hello.txt"}}], "finish": "tool_use", "input_tokens": 100, "output_tokens": 10},
            {"deltas": ["done\n"], "finish": "end_turn", "input_tokens": 50, "output_tokens": 5}
        ]"#,
    );
    let run = fixture.run_replay(&[], "ndjson");
    assert_eq!(run.status.code(), Some(0), "stderr: {}", stderr(&run));

    let database = fixture.state.join("state.sqlite3");
    let (session, run_id) = {
        let store = SqliteStore::open(&database).unwrap();
        let summaries = store.list_run_summaries(None).unwrap();
        assert_eq!(summaries.len(), 1);
        (
            summaries[0].run.session_id.clone(),
            summaries[0].run.id.clone(),
        )
    };

    // Plant the audit exactly as a live classifier run persists it (the default
    // test build has no live transport, so no gate can attach on its own).
    {
        let mut store = SqliteStore::open(&database).unwrap();
        store
            .record_classifier_audit(
                &run_id,
                r#"{"attached":true,"calls":2,"availability":{"available":1,"timeout":1},"escalations":1,"cost_known":false,"cost_microusd":null}"#,
            )
            .unwrap();
    }

    let listed = fixture.output(&["runs", "list"]);
    assert_eq!(listed.status.code(), Some(0), "stderr: {}", stderr(&listed));
    let text = stdout(&listed);
    for expected in [
        run_id.as_str(),
        session.as_str(),
        "completed",
        "usage=in=150 out=15 cost=675µ$",
        "classifier=2 calls · available 1, timeout 1 · 1 escalated · cost unknown",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
    }

    let filtered = fixture.output(&["runs", "list", "--session", session.as_str()]);
    assert_eq!(
        filtered.status.code(),
        Some(0),
        "stderr: {}",
        stderr(&filtered)
    );
    assert!(stdout(&filtered).contains(run_id.as_str()));

    let absent = SessionId::generate();
    let empty = fixture.output(&["runs", "list", "--session", absent.as_str()]);
    assert_eq!(empty.status.code(), Some(0), "stderr: {}", stderr(&empty));
    assert!(stdout(&empty).contains("no runs recorded for session"));

    // Read-only: listing creates neither sessions nor runs.
    let store = SqliteStore::open(&database).unwrap();
    assert_eq!(store.list_sessions().unwrap().len(), 1);
    assert_eq!(store.list_run_summaries(None).unwrap().len(), 1);
}

#[test]
fn doctor_reports_mcp_servers_as_untrusted() {
    let fixture = Fixture::new();
    std::fs::write(
        &fixture.config,
        std::fs::read_to_string(&fixture.config)
            .unwrap()
            .replace(
                r#""mcp_servers": []"#,
                r#""mcp_servers": [{"id": "srv_1", "command": "definitely-not-a-program", "args": [], "env_allowlist": [], "enabled": true}]"#,
            ),
    )
    .unwrap();
    let output = fixture.output(&["mcp", "list"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("srv_1"));
    assert!(text.contains("enabled=true"));
    assert!(text.contains("trusted=false"));
    assert!(text.contains("no server is started"));
}

#[test]
fn checkpoints_preview_and_refuse_user_edits() {
    let fixture = Fixture::new();
    fixture.write_script(
        r#"[
            {"deltas": [], "tool_calls": [{"call_id": "w1", "name": "write_file", "arguments": {"path": "notes.txt", "content": "written by bollo\n", "expected_sha256": null}}], "finish": "tool_use"},
            {"deltas": ["done\n"], "finish": "end_turn"}
        ]"#,
    );
    let profile = ["--profile", "unrestricted"];
    let run = fixture.run_replay(&profile, "ndjson");
    assert_eq!(run.status.code(), Some(0), "stderr: {}", stderr(&run));
    assert!(fixture.workspace.join("notes.txt").exists());

    let list = fixture.output(&[
        "checkpoint",
        "list",
        "--profile",
        "unrestricted",
        "--acknowledge-risk",
    ]);
    assert_eq!(list.status.code(), Some(0), "stderr: {}", stderr(&list));
    let checkpoint = stdout(&list)
        .lines()
        .next()
        .and_then(|line| line.split('\t').next())
        .expect("a checkpoint id")
        .to_string();
    assert!(checkpoint.starts_with("ckpt_"));

    let preview = fixture.output(&[
        "checkpoint",
        "preview",
        &checkpoint,
        "--profile",
        "unrestricted",
        "--acknowledge-risk",
    ]);
    assert_eq!(preview.status.code(), Some(0));
    assert!(stdout(&preview).contains("delete notes.txt"));

    // A user edit after Bollo's change must make restore refuse.
    std::fs::write(fixture.workspace.join("notes.txt"), "user edit\n").unwrap();
    let refused = fixture.output(&[
        "checkpoint",
        "restore",
        &checkpoint,
        "--yes",
        "--profile",
        "unrestricted",
        "--acknowledge-risk",
    ]);
    assert_eq!(
        refused.status.code(),
        Some(5),
        "stderr: {}",
        stderr(&refused)
    );
    assert!(stderr(&refused).contains("refusing to delete user content"));
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("notes.txt")).unwrap(),
        "user edit\n"
    );

    // Restoring our exact postimage works and removes the created file.
    std::fs::write(fixture.workspace.join("notes.txt"), "written by bollo\n").unwrap();
    let restored = fixture.output(&[
        "checkpoint",
        "restore",
        &checkpoint,
        "--yes",
        "--profile",
        "unrestricted",
        "--acknowledge-risk",
    ]);
    assert_eq!(
        restored.status.code(),
        Some(0),
        "stderr: {}",
        stderr(&restored)
    );
    assert!(!fixture.workspace.join("notes.txt").exists());
}

#[cfg(windows)]
#[test]
fn workspace_sandbox_is_verified_or_refused_but_never_host_fallback() {
    let fixture = Fixture::new();
    // No --sandbox off: startup must either verify containment with an executed
    // check or refuse. It must never quietly run workspace mode on the host.
    let config = std::fs::read_to_string(&fixture.config)
        .unwrap()
        .replace(r#""sandbox": "off""#, r#""sandbox": "workspace""#);
    std::fs::write(&fixture.config, config).unwrap();
    let output = fixture.output(&["doctor"]);
    let text = stdout(&output);
    let diagnostics = stderr(&output);
    match output.status.code() {
        Some(0) => {
            assert!(
                text.contains(
                    "filesystem_containment=true network_denied=true workspace_auto=true"
                ),
                "stdout: {text}"
            );
            assert!(text.contains("Windows AppContainer"), "stdout: {text}");
        }
        Some(2) => {
            assert!(
                diagnostics.contains("filesystem containment is unavailable"),
                "stderr: {diagnostics}"
            );
        }
        other => panic!("unexpected exit {other:?}: stdout: {text}; stderr: {diagnostics}"),
    }
}

#[cfg(windows)]
#[test]
fn workspace_mode_runs_exec_children_inside_the_container() {
    let fixture = Fixture::new();
    let config = std::fs::read_to_string(&fixture.config)
        .unwrap()
        .replace(r#""sandbox": "off""#, r#""sandbox": "workspace""#);
    std::fs::write(&fixture.config, config).unwrap();
    // Only a host that verified containment can run this scenario; the doctor
    // test above pins the refusal contract when verification is unavailable.
    if fixture.output(&["doctor"]).status.code() != Some(0) {
        eprintln!("containment unavailable on this host; skipping the contained run");
        return;
    }
    // The child writes inside the workspace through the granted container
    // access; the file is the observable proof it ran under containment.
    fixture.write_script(
        r#"[
            {"deltas": [], "tool_calls": [{"call_id": "x1", "name": "exec", "arguments": {"argv": ["cmd", "/C", "echo contained-ok > contained.txt"], "cwd": ".", "timeout_seconds": 20}}], "finish": "tool_use"},
            {"deltas": ["done\n"], "finish": "end_turn"}
        ]"#,
    );
    let script = fixture.script_arg();
    let run = fixture.output(&[
        "run",
        "--profile",
        "unrestricted",
        "--acknowledge-risk",
        "--provider",
        "replay",
        "--script",
        &script,
        "--prompt",
        "run the child",
        "--output",
        "ndjson",
    ]);
    assert_eq!(run.status.code(), Some(0), "stderr: {}", stderr(&run));
    let written = std::fs::read_to_string(fixture.workspace.join("contained.txt"))
        .expect("the contained child wrote its output into the workspace");
    assert!(written.contains("contained-ok"), "file: {written}");
}

#[test]
fn resume_refuses_unknown_operations_without_acknowledgement() {
    let fixture = Fixture::new();
    // Simulate a crash: a run whose operation intent was journalled but never
    // finished. This is exactly what a restart must treat as `unknown`.
    let database = fixture.state.join("state.sqlite3");
    let session = {
        let mut store = SqliteStore::open(&database).unwrap();
        let identity = fixture.workspace.display().to_string();
        let session = store.create_session(&identity, Some("crash")).unwrap();
        let run = store
            .start_run(&session, "anthropic", "test-model", 1)
            .unwrap();
        store
            .record_operation_intent(&run, "call_crashed", "exec", &"a".repeat(64))
            .unwrap();
        session
    };
    fixture.write_script(r#"[{"deltas": ["resumed\n"], "finish": "end_turn"}]"#);

    let script = fixture.script_arg();
    let refused = fixture.output(&[
        "resume",
        session.as_str(),
        "--sandbox",
        "off",
        "--acknowledge-risk",
        "--provider",
        "replay",
        "--script",
        &script,
        "--prompt",
        "continue",
    ]);
    assert_eq!(
        refused.status.code(),
        Some(5),
        "stderr: {}",
        stderr(&refused)
    );
    assert!(stderr(&refused).contains("never replayed"));

    let accepted = fixture.output(&[
        "resume",
        session.as_str(),
        "--sandbox",
        "off",
        "--acknowledge-risk",
        "--acknowledge-unknown",
        "--provider",
        "replay",
        "--script",
        &script,
        "--prompt",
        "continue",
        "--output",
        "ndjson",
    ]);
    assert_eq!(
        accepted.status.code(),
        Some(0),
        "stderr: {}",
        stderr(&accepted)
    );

    // The unknown operation is untouched: still exactly one, still unknown.
    let store = SqliteStore::open(&database).unwrap();
    let operations = store.operations_for_session(&session).unwrap();
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].state, bollo_store::OperationState::Unknown);
}

#[test]
fn nonexistent_session_resume_is_exit_5() {
    let fixture = Fixture::new();
    fixture.write_script(r#"[{"deltas": ["hi\n"], "finish": "end_turn"}]"#);
    let missing = SessionId::generate();
    let script = fixture.script_arg();
    let output = fixture.output(&[
        "resume",
        missing.as_str(),
        "--sandbox",
        "off",
        "--acknowledge-risk",
        "--provider",
        "replay",
        "--script",
        &script,
        "--prompt",
        "continue",
    ]);
    assert_eq!(output.status.code(), Some(5));
    assert!(stderr(&output).contains("no stored session"));
}

/// Paths in a fixture stay inside the temporary root.
#[allow(dead_code)]
fn assert_inside(root: &Path, path: &Path) {
    assert!(
        path.starts_with(root),
        "{} escaped {}",
        path.display(),
        root.display()
    );
}

/// The interactive client is terminal-independent by design (input and output
/// are injected), so the whole interactive path can be driven here without a
/// TTY: composition → runtime → TUI transcript.
#[test]
fn interactive_session_drives_the_real_runtime() {
    use bollo_cli::composition::{Composition, InteractiveRunner, TurnSettings};
    use bollo_protocol::cancel::CancellationToken;
    use bollo_protocol::vocab::ModeKind;
    use bollo_tui::{Presenter, SlashCommands, Terminal, TuiSession};
    use clap::Parser;

    struct NoExternalCommands;
    impl SlashCommands for NoExternalCommands {
        fn handle(&mut self, _name: &str, _argument: &str) -> Option<Vec<String>> {
            None
        }
    }

    let fixture = Fixture::new();
    fixture.write_script(r#"[{"deltas": ["the repo is small\n"], "finish": "end_turn"}]"#);
    std::env::set_var("BOLLO_CONFIG", fixture.config.display().to_string());

    let cli = bollo_cli::args::Cli::try_parse_from([
        "bollo",
        "--workspace",
        &fixture.workspace.display().to_string(),
        "--state-dir",
        &fixture.state.display().to_string(),
        "--sandbox",
        "off",
        "--acknowledge-risk",
        "--provider",
        "replay",
        "--script",
        &fixture.script_arg(),
    ])
    .unwrap();

    let mut composition = Composition::build(&cli).unwrap();
    composition.require_risk_acknowledgement().unwrap();
    composition.ensure_provider().unwrap();
    let session = composition.new_session(Some("interactive-test")).unwrap();
    let settings = TurnSettings {
        session,
        mode: ModeKind::Build,
        include_claude_md: false,
    };

    let terminal = Terminal::new(
        Box::new(std::io::Cursor::new("explain the repo\n/quit\n")),
        Box::new(Vec::new()),
        Presenter::new(80, false),
    );
    let mut tui = TuiSession::new(terminal, 256);
    let composition = std::cell::RefCell::new(composition);
    let mut runner = InteractiveRunner {
        composition: &composition,
        settings,
        cancel: CancellationToken::new(),
    };
    let mut commands = NoExternalCommands;
    let code = tui.run(&mut runner, &mut commands, None);
    assert_eq!(code, 0);
    let transcript = tui.transcript().joined();
    assert!(transcript.contains("the repo is small"), "{transcript}");
    assert!(
        transcript.contains("run finished: completed"),
        "{transcript}"
    );

    // The interactive turn is durably journalled like any other run.
    let store = SqliteStore::open(&fixture.state.join("state.sqlite3")).unwrap();
    assert!(!store.list_sessions().unwrap().is_empty());
    std::env::remove_var("BOLLO_CONFIG");
}
