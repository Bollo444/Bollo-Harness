//! Cross-crate integration fixtures shared by the golden scenario, recovery and
//! negative suites. Everything here is a first-class test double (fake provider,
//! scripted approvals) driving the real loop, not a mock of the code under test.

use std::path::PathBuf;

use bollo_core::runtime::ApprovalChannel;
use bollo_core::{RunOutcome, Runtime, ScriptedChannel};
use bollo_extensions::TrustStore;
use bollo_policy::layers::{build_snapshot, validate_startup, CliOverrides, PolicySnapshot};
use bollo_policy::MemoryApprovalStore;
use bollo_providers::fake::{FakeProvider, ScriptedResponse};
use bollo_providers::{FinishReason, ToolIntent, Usage};
use bollo_protocol::cancel::CancellationToken;
use bollo_protocol::events::EventEnvelope;
use bollo_protocol::ids::SessionId;
use bollo_protocol::vocab::{ModeKind, Profile, SandboxMode};
use bollo_store::SqliteStore;
use bollo_tools::{McpDispatch, McpToolSpec};
use bollo_workspace::{CheckpointLog, WorkspaceFs, WorkspaceRoot};

pub struct Harness {
    pub workspace: tempfile::TempDir,
    pub root: WorkspaceRoot,
    pub fs: WorkspaceFs,
    pub snapshot: PolicySnapshot,
    pub store: SqliteStore,
    pub approvals: MemoryApprovalStore,
    pub checkpoints: CheckpointLog,
    pub trust: TrustStore,
    pub provider: FakeProvider,
    pub session: SessionId,
    pub cancel: CancellationToken,
    /// Discovered MCP tools added to the model-facing tool list.
    pub mcp_tools: Vec<McpToolSpec>,
    /// Optional MCP dispatch port attached to the next run.
    pub mcp: Option<Box<dyn McpDispatch>>,
}

impl Harness {
    pub fn new(profile: Profile, max_tool_calls: u32, script: Vec<ScriptedResponse>) -> Self {
        let workspace = tempfile::tempdir().unwrap();
        let root = WorkspaceRoot::discover(workspace.path()).unwrap();
        let fs = WorkspaceFs::new(&root);
        let snapshot = snapshot(profile, max_tool_calls);
        validate_startup(&snapshot).unwrap();
        let store = SqliteStore::open_in_memory().unwrap();
        let mut store = store;
        let session = store.create_session(root.identity_hash(), Some("integration")).unwrap();
        Self {
            workspace,
            root,
            fs,
            snapshot,
            store,
            approvals: MemoryApprovalStore::new(),
            checkpoints: CheckpointLog::new(),
            trust: TrustStore::new(),
            provider: FakeProvider::anthropic(script),
            session,
            cancel: CancellationToken::new(),
            mcp_tools: Vec::new(),
            mcp: None,
        }
    }

    /// Install (or replace) the provider script; useful when the script needs
    /// hashes of files that only exist after seeding.
    pub fn set_script(&mut self, script: Vec<ScriptedResponse>) {
        self.provider = FakeProvider::anthropic(script);
    }

    /// Write a file into the workspace before the run starts.
    pub fn seed(&self, path: &str, content: &str) {
        let absolute = self.workspace.path().join(path);
        if let Some(parent) = absolute.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(absolute, content).unwrap();
    }

    pub fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.workspace.path().join(path)).unwrap()
    }

    pub fn exists(&self, path: &str) -> bool {
        self.workspace.path().join(path).exists()
    }

    pub fn path(&self, path: &str) -> PathBuf {
        self.workspace.path().join(path)
    }

    pub fn run(&mut self, prompt: &str, decisions: Vec<bool>) -> (RunOutcome, Vec<EventEnvelope>) {
        let mut channel = ScriptedChannel::new(decisions);
        self.run_with_channel(prompt, &mut channel)
    }

    pub fn run_with_channel(
        &mut self,
        prompt: &str,
        channel: &mut dyn ApprovalChannel,
    ) -> (RunOutcome, Vec<EventEnvelope>) {
        let mut events = Vec::new();
        let outcome = {
            let mut sink = |event: &EventEnvelope| events.push(event.clone());
            let mut runtime = Runtime::new(
                &self.provider,
                &self.snapshot,
                self.root.identity_hash().to_string(),
                &self.fs,
                &mut self.store,
                &mut self.approvals,
                &mut self.checkpoints,
                &self.trust,
                channel,
                self.session.clone(),
                "test-model".into(),
                ModeKind::Build,
            );
            runtime = runtime.with_cancel(self.cancel.clone());
            if let Some(dispatch) = self.mcp.as_deref_mut() {
                runtime = runtime
                    .with_mcp_tools(self.mcp_tools.clone())
                    .with_mcp(dispatch);
            }
            runtime.run(prompt, &mut sink)
        };
        (outcome, events)
    }

    /// A broken-fixture Cargo project used for edit → test → restore.
    pub fn seed_broken_cargo_project(&self) {
        self.seed(
            "Cargo.toml",
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        self.seed(
            "src/lib.rs",
            "pub fn add(a: i32, b: i32) -> i32 { a - b }\n\n#[cfg(test)]\nmod tests {\n    \
             use super::*;\n    #[test]\n    fn adds() { assert_eq!(add(2, 2), 4); }\n}\n",
        );
    }
}

/// Balanced-or-other profile with host-mode sandbox, trusted prices and a
/// bounded tool-call ceiling. Mirrors what the CLI composes from config.
pub fn snapshot(profile: Profile, max_tool_calls: u32) -> PolicySnapshot {
    let json = format!(
        r#"{{
            "schema_version": "0.1",
            "provider": {{
                "kind": "anthropic",
                "base_url": "https://api.anthropic.com",
                "model": "test-model",
                "credential_env": "ANTHROPIC_API_KEY",
                "context_tokens": 20000,
                "input_microusd_per_token": 3,
                "output_microusd_per_token": 15
            }},
            "permissions": {{ "profile": "{}", "sandbox": "off" }},
            "limits": {{
                "max_tool_calls": {},
                "max_run_seconds": 120,
                "max_output_tokens": 1024,
                "max_spend_cents": 500,
                "tool_output_bytes": 65536
            }},
            "privacy": {{ "telemetry": false, "content_retention_days": 7 }},
            "mcp_servers": [],
            "hooks": []
        }}"#,
        profile.as_str(),
        max_tool_calls
    );
    let user = bollo_policy::parse_user_config(&json).unwrap();
    build_snapshot(
        &user,
        None,
        &CliOverrides {
            sandbox: Some(SandboxMode::Off),
            ..Default::default()
        },
        bollo_workspace::probe(),
        1,
    )
    .unwrap()
}

pub fn tool_intent(call_id: &str, name: &str, arguments: serde_json::Value) -> ToolIntent {
    ToolIntent {
        call_id: call_id.to_string(),
        name: name.to_string(),
        arguments,
    }
}

/// A response that proposes tool calls.
pub fn tool_response(intents: Vec<ToolIntent>) -> ScriptedResponse {
    ScriptedResponse {
        deltas: Vec::new(),
        tool_intents: intents,
        usage: Usage {
            input_tokens: Some(200),
            output_tokens: Some(20),
        },
        finish: FinishReason::ToolUse,
        error: None,
    }
}
