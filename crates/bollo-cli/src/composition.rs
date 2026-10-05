//! Composition root. This is the only place where concrete adapters meet:
//! trusted configuration is layered into a policy snapshot, the store is
//! opened, hook trust is bound to exact executable identities, and the provider
//! adapter is selected. Nothing here decides policy; it only assembles inputs
//! for `bollo-core`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::Value;

use bollo_core::runtime::{ApprovalChannel, ApprovalRequest as CoreApprovalRequest};
use bollo_core::{RunOutcome, Runtime};
use bollo_extensions::{ExtensionError, HookSpec, McpClient, McpServerSpec, McpTool, TrustStore};
use bollo_policy::classifier::{ClassifierGate, EscalationThresholds, RiskClassifier};
use bollo_policy::config::{
    BolloConfig, ClassifierConfig, HookConfig, McpServerConfig, ProviderConfig, ProviderKind,
};
use bollo_policy::layers::{build_snapshot, validate_startup, CliOverrides, PolicySnapshot};
use bollo_policy::MemoryApprovalStore;
use bollo_providers::fake::{FakeProvider, ScriptedResponse};
use bollo_providers::{FinishReason, Provider, ToolIntent, Usage};
use bollo_protocol::cancel::CancellationToken;
use bollo_protocol::errors::ErrorCode;
use bollo_protocol::events::EventEnvelope;
use bollo_protocol::ids::SessionId;
use bollo_protocol::vocab::{ModeKind, Profile, SandboxCapabilities, SandboxMode};
use bollo_store::SqliteStore;
use bollo_tools::{McpDispatch, McpDispatchOutcome, McpToolSpec};
use bollo_workspace::{
    create_workspace_sandbox, probe, verified_probe, CheckpointLog, WorkspaceFs, WorkspaceRoot,
};

use crate::args::{Cli, ProviderArg};
use crate::CliError;

pub const STATE_DB_FILE: &str = "state.sqlite3";
pub const ARTIFACT_DIR: &str = "artifacts";
pub const MODEL_PLACEHOLDER: &str = "REPLACE_WITH_AVAILABLE_MODEL_ID";

#[derive(Debug, Clone)]
pub struct ProviderChoice {
    pub vendor: Option<ProviderArg>,
    pub script: Option<PathBuf>,
}

/// Live connections to enabled MCP servers, built lazily by [`Composition::ensure_mcp`].
///
/// The trusted user configuration is the host action that grants executable
/// trust (exact resolved bytes, argv, cwd and env names); project configuration
/// cannot add servers at all. Discovery failures are warnings, never fatal:
/// the rest of the session still works, and the failed server's tools simply do
/// not appear in the model-facing list. A disconnect quarantines every tool of
/// that server for the rest of the process; nothing is replayed.
#[derive(Default)]
pub struct McpRegistry {
    clients: Vec<McpClient>,
    /// Discovered tools; `canonical_id` is the identity policy and the journal
    /// bind, `provider_name` is the id-safe spelling models see.
    pub tools: Vec<McpToolSpec>,
    quarantined: BTreeSet<String>,
    /// Generation of each live client's last `tools/list`, so an announced
    /// `notifications/tools/list_changed` is re-listed exactly once.
    listed_generation: BTreeMap<String, u64>,
}

impl McpRegistry {
    /// Start, handshake and discover every enabled server. Failures of one
    /// server do not abort the others.
    pub fn connect(
        servers: &[McpServerConfig],
        cwd: &Path,
        trust: &mut TrustStore,
    ) -> (Self, Vec<String>) {
        let mut registry = Self::default();
        let mut warnings = Vec::new();
        for server in servers.iter().filter(|server| server.enabled) {
            let spec = McpServerSpec {
                id: server.id.clone(),
                command: server.command.clone(),
                args: server.args.clone(),
                env_allowlist: server.env_allowlist.clone(),
                enabled: server.enabled,
            };
            let mut argv = vec![spec.command.clone()];
            argv.extend(spec.args.clone());
            if let Err(err) = trust.grant(&argv, cwd, &spec.env_allowlist) {
                warnings.push(format!(
                    "mcp server {} is enabled but could not be trusted ({err}); it stays off",
                    spec.id
                ));
                continue;
            }
            let mut client = match McpClient::start(&spec, cwd, trust) {
                Ok(client) => client,
                Err(err) => {
                    warnings.push(format!(
                        "mcp server {} could not start ({err}); it stays off",
                        spec.id
                    ));
                    continue;
                }
            };
            if let Err(err) = client.initialize() {
                warnings.push(format!(
                    "mcp server {} handshake failed ({err}); it stays off",
                    spec.id
                ));
                let _ = client.shutdown();
                continue;
            }
            let discovered = match client.list_tools() {
                Ok(tools) => tools,
                Err(err) => {
                    warnings.push(format!(
                        "mcp server {} tool discovery failed ({err}); it stays off",
                        spec.id
                    ));
                    let _ = client.shutdown();
                    continue;
                }
            };
            let generation = client.tools_changed();
            for tool in discovered {
                registry.tools.push(mcp_tool_spec(&spec.id, tool));
            }
            registry.clients.push(client);
            registry
                .listed_generation
                .insert(spec.id.clone(), generation);
        }
        (registry, warnings)
    }

    /// Close every live server. Called when the registry is dropped so a
    /// process exit never leaves an orphaned stdio child behind.
    fn close_all(&mut self) {
        for client in std::mem::take(&mut self.clients) {
            let _ = client.shutdown();
        }
    }

    fn quarantine(&mut self, server_id: &str) {
        if let Some(position) = self
            .clients
            .iter()
            .position(|client| client.server_id() == server_id)
        {
            let client = self.clients.remove(position);
            client.quarantine();
        }
        self.quarantined.insert(server_id.to_string());
    }

    /// Re-list tools for every server that announced
    /// `notifications/tools/list_changed` since the catalog was last built.
    /// Returns true when the model-facing catalog changed. A server that fails
    /// to re-list is quarantined exactly like a disconnect: uncertainty shrinks
    /// the catalog, it never widens it.
    fn refresh(&mut self) -> bool {
        let mut changed = false;
        let mut index = 0;
        while index < self.clients.len() {
            let server_id = self.clients[index].server_id().to_string();
            self.clients[index].poll_notifications();
            let generation = self.clients[index].tools_changed();
            let listed = self
                .listed_generation
                .get(&server_id)
                .copied()
                .unwrap_or(0);
            if generation <= listed {
                index += 1;
                continue;
            }
            match self.clients[index].list_tools() {
                Ok(discovered) => {
                    let generation = self.clients[index].tools_changed();
                    self.listed_generation.insert(server_id.clone(), generation);
                    self.tools.retain(|spec| spec.server_id != server_id);
                    for tool in discovered {
                        self.tools.push(mcp_tool_spec(&server_id, tool));
                    }
                    changed = true;
                    index += 1;
                }
                Err(_) => {
                    let client = self.clients.remove(index);
                    client.quarantine();
                    self.quarantined.insert(server_id.clone());
                    self.tools.retain(|spec| spec.server_id != server_id);
                    changed = true;
                }
            }
        }
        changed
    }
}

impl Drop for McpRegistry {
    fn drop(&mut self) {
        self.close_all();
    }
}

/// Map a discovered extension tool onto the runtime's catalog entry.
fn mcp_tool_spec(server_id: &str, tool: McpTool) -> McpToolSpec {
    let provider_name = bollo_tools::provider_safe_tool_name(&tool.canonical_id);
    McpToolSpec {
        canonical_id: tool.canonical_id,
        provider_name,
        server_id: server_id.to_string(),
        tool_name: tool.name.clone(),
        description: tool
            .description
            .unwrap_or_else(|| format!("MCP tool {} on server {}", tool.name, server_id)),
        input_schema: tool.input_schema,
    }
}

impl McpDispatch for McpRegistry {
    fn refresh_catalog(&mut self) -> Option<Vec<McpToolSpec>> {
        if self.refresh() {
            Some(self.tools.clone())
        } else {
            None
        }
    }

    fn dispatch(&mut self, canonical_id: &str, arguments: &Value) -> McpDispatchOutcome {
        let Some(spec) = self
            .tools
            .iter()
            .find(|tool| tool.canonical_id == canonical_id)
        else {
            return McpDispatchOutcome::failed(
                ErrorCode::UnknownTool,
                format!("unknown MCP tool {canonical_id}"),
            );
        };
        let server_id = spec.server_id.clone();
        let tool_name = spec.tool_name.clone();
        if self.quarantined.contains(&server_id) {
            return McpDispatchOutcome::unknown(format!(
                "mcp server {server_id} is quarantined after a disconnect; its tools need a fresh \
                 handshake"
            ));
        }
        let call = match self
            .clients
            .iter_mut()
            .find(|client| client.server_id() == server_id)
        {
            Some(client) => client.call_tool(&tool_name, arguments.clone()),
            None => Err(bollo_extensions::ExtensionError::Disconnected),
        };
        match call {
            Ok(result) => {
                let mut outcome = McpDispatchOutcome::from_result(
                    result.content,
                    result.is_error,
                    result.truncated,
                );
                outcome.summary = mcp_summary(&server_id, &tool_name, &outcome.content);
                outcome
            }
            Err(ExtensionError::ServerError(message)) => McpDispatchOutcome::failed(
                ErrorCode::McpError,
                format!("mcp server {server_id} rejected the call: {message}"),
            ),
            Err(err) => {
                self.quarantine(&server_id);
                McpDispatchOutcome::unknown(format!(
                    "mcp server {server_id} failed during the call ({err}); the effect is unknown and \
                     its tools are quarantined"
                ))
            }
        }
    }
}

/// Bounded, labeled preview of server content for journals and the TUI. Server
/// content is untrusted; it is shown, never interpreted as a permission.
fn mcp_summary(server_id: &str, tool_name: &str, content: &[Value]) -> String {
    let mut preview = String::new();
    for block in content {
        let part = block
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| block.to_string());
        if !preview.is_empty() {
            preview.push(' ');
        }
        preview.push_str(&part);
        if preview.chars().count() >= 256 {
            break;
        }
    }
    let preview: String = preview.chars().take(256).collect();
    if preview.is_empty() {
        format!("mcp {server_id} / {tool_name} returned no content")
    } else {
        format!("mcp {server_id} / {tool_name}: {preview}")
    }
}

/// The attached advisory classifier: a concrete [`RiskClassifier`] plus the
/// escalation policy it is bound to. `Composition.classifier == None` means the
/// runtime makes zero classifier calls, whatever the configuration says.
pub struct ClassifierRuntime {
    classifier: Box<dyn RiskClassifier>,
    profiles: Vec<Profile>,
    thresholds: EscalationThresholds,
}

impl ClassifierRuntime {
    // Constructed only on the live path (and by the unit test below); the
    // default build deliberately has no way to create one.
    #[cfg(any(test, feature = "live-http"))]
    fn new(classifier: Box<dyn RiskClassifier>, config: &ClassifierConfig) -> Self {
        Self {
            classifier,
            profiles: config.profiles.clone(),
            thresholds: EscalationThresholds {
                escalate_at: config.escalate_at,
                min_confidence: config.min_confidence,
                credential_threshold: config.credential_threshold,
            },
        }
    }

    pub fn model(&self) -> &str {
        self.classifier.model()
    }

    pub fn profiles(&self) -> &[Profile] {
        &self.profiles
    }

    /// Bind the gate for one turn: same adapter and thresholds, and only the
    /// configured profiles are eligible for escalation.
    pub fn gate(&self) -> ClassifierGate<'_> {
        ClassifierGate::new(self.classifier.as_ref())
            .with_profiles(self.profiles.clone())
            .with_thresholds(self.thresholds)
    }
}

fn profile_list(profiles: &[Profile]) -> String {
    profiles
        .iter()
        .map(|profile| profile.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

pub struct Composition {
    pub root: WorkspaceRoot,
    pub fs: WorkspaceFs,
    pub snapshot: PolicySnapshot,
    pub user_config: BolloConfig,
    pub store: SqliteStore,
    pub trust: TrustStore,
    pub hooks: Vec<HookSpec>,
    provider: Option<Box<dyn Provider>>,
    provider_choice: ProviderChoice,
    pub model: String,
    pub state_dir: PathBuf,
    pub user_config_path: Option<PathBuf>,
    pub project_config_path: Option<PathBuf>,
    pub capabilities: SandboxCapabilities,
    /// True when the effective posture executes with the user's own authority.
    pub risk_required: bool,
    pub risk_acknowledged: bool,
    pub warnings: Vec<String>,
    /// Stale `prepared`/`started` operations turned `unknown` at startup.
    pub recovered_operations: usize,
    /// Connected MCP servers. `None` until a turn is about to run, so
    /// inspection commands never launch a server.
    pub mcp: Option<McpRegistry>,
    /// Advisory classifier (BH-021). `None` means zero calls are possible: the
    /// default build has no live transport, and an enabled block without the
    /// `live-http` feature intentionally stays detached.
    pub classifier: Option<ClassifierRuntime>,
    /// Run sandbox. Present when workspace mode was verified *and* the host
    /// container was created; brokered children then launch inside it. A
    /// creation failure refuses the run instead of falling back to the host.
    pub sandbox: Option<Box<dyn bollo_workspace::ChildSandbox>>,
}

impl Composition {
    pub fn build(cli: &Cli) -> Result<Self, CliError> {
        let mut warnings = Vec::new();
        let workspace = cli.workspace.clone().unwrap_or_else(|| PathBuf::from("."));
        let root = WorkspaceRoot::discover(&workspace)
            .map_err(|err| CliError::usage(format!("workspace discovery failed: {err}")))?;
        let fs = WorkspaceFs::new(&root);

        let state_dir = state_directory(cli);
        std::fs::create_dir_all(&state_dir).map_err(|err| {
            CliError::usage(format!(
                "cannot create state directory {}: {err}",
                state_dir.display()
            ))
        })?;
        std::fs::create_dir_all(state_dir.join(ARTIFACT_DIR)).map_err(|err| {
            CliError::usage(format!("cannot create artifact directory: {err}"))
        })?;

        // --- trusted user configuration ---
        let user_config_path = user_config_path();
        let mut user_config = match &user_config_path {
            Some(path) if path.is_file() => {
                let json = std::fs::read_to_string(path).map_err(|err| {
                    CliError::usage(format!("cannot read {}: {err}", path.display()))
                })?;
                bollo_policy::parse_user_config(&json).map_err(|err| {
                    CliError::usage(format!("invalid configuration {}: {err}", path.display()))
                })?
            }
            _ => {
                warnings.push(
                    "no trusted user configuration found; using built-in balanced defaults"
                        .to_string(),
                );
                default_user_config()
            }
        };

        // --- provider selection is a session binding, layered before the snapshot ---
        if let Some(provider) = cli.provider {
            apply_provider_binding(&mut user_config, provider);
        }
        if let Some(model) = &cli.model {
            user_config.provider.model = model.clone();
        }

        // --- project configuration: untrusted, restrict-only ---
        let project_config_path = root.canonical().join(".bollo").join("config.json");
        let project_config = if project_config_path.is_file() {
            let json = std::fs::read_to_string(&project_config_path).map_err(|err| {
                CliError::usage(format!(
                    "cannot read {}: {err}",
                    project_config_path.display()
                ))
            })?;
            let project = bollo_policy::parse_project_config(&json).map_err(|err| {
                CliError::usage(format!(
                    "invalid project configuration {}: {err}",
                    project_config_path.display()
                ))
            })?;
            warnings.push(
                "project configuration applied as restrictions only (it can never widen)".into(),
            );
            Some(project)
        } else {
            None
        };

        let mut capabilities = probe();
        let cli_overrides = CliOverrides {
            profile: cli.profile.map(Into::into),
            sandbox: cli.sandbox.map(Into::into),
            model: cli.model.clone(),
            max_tool_calls: cli.max_tool_calls,
            max_run_seconds: cli.max_run_seconds,
            max_spend_cents: cli.max_spend_cents.map(Some),
            tool_output_bytes: None,
        };
        let mut snapshot = build_snapshot(
            &user_config,
            project_config.as_ref(),
            &cli_overrides,
            capabilities.clone(),
            1,
        )
        .map_err(|err| CliError::usage(format!("policy: {err}")))?;
        if snapshot.sandbox == SandboxMode::Workspace {
            // An enforcement claim must be backed by an executed check in this
            // process: the cheap probe above makes no claim. When the check
            // fails the flags stay false and startup refuses below, and the
            // broker never falls back to host execution for a workspace run.
            capabilities = verified_probe();
            snapshot.capabilities = capabilities.clone();
        }
        validate_startup(&snapshot).map_err(|err| CliError::usage(format!("{err}")))?;

        // --- advisory classifier: trusted opt-in, disabled by default ---
        // Attaching the gate needs `classifier.enabled` in the trusted user
        // configuration *and* a build with the live transport. Otherwise the
        // gate stays detached and no classifier traffic is possible, even for
        // an enabled block. Project configuration cannot carry this block at
        // all, so it can never enable escalation.
        let classifier = if user_config.classifier.enabled {
            match build_classifier(&user_config.classifier) {
                Ok(runtime) => {
                    warnings.push(format!(
                        "advisory classifier attached (model {}; profiles {}); it can only \
                         escalate allow to ask, and every failure is neutral",
                        runtime.model(),
                        profile_list(runtime.profiles())
                    ));
                    Some(runtime)
                }
                Err(reason) => {
                    warnings.push(reason);
                    None
                }
            }
        } else {
            None
        };

        // Risk acknowledgement gates *execution*, not inspection: read-only
        // commands stay usable without it.
        let risk_required = snapshot.profile == Profile::Unrestricted
            || snapshot.sandbox == SandboxMode::Off;
        warnings.extend(snapshot.warnings.iter().cloned());

        // --- durable store + startup recovery ---
        let mut store = SqliteStore::open(&state_dir.join(STATE_DB_FILE))
            .map_err(|err| CliError::recovery(format!("cannot open store: {err}")))?;
        let recovered_operations = store
            .mark_stale_operations_unknown()
            .map_err(|err| CliError::recovery(format!("recovery failed: {err}")))?;
        if recovered_operations > 0 {
            warnings.push(format!(
                "{recovered_operations} operation(s) left in flight by a previous process were \
                 marked unknown; unknown effects are never replayed"
            ));
        }

        // --- hooks: trusted user config is the host action; identity is bound ---
        let mut trust = TrustStore::new();
        let mut hooks = Vec::new();
        for hook in &user_config.hooks {
            let spec = hook_spec(hook)?;
            if hook.enabled {
                match trust.grant(&spec.argv, root.canonical(), &spec.env_names) {
                    Ok(_record) => {}
                    Err(err) => warnings.push(format!(
                        "hook {} enabled but could not be trusted ({err}); it will deny until \
                         the trust record can be created",
                        hook.id
                    )),
                }
            }
            hooks.push(spec);
        }

        let provider_choice = ProviderChoice {
            vendor: cli.provider,
            script: cli.script.clone(),
        };
        let model = if user_config.provider.model.trim().is_empty()
            || user_config.provider.model == MODEL_PLACEHOLDER
        {
            warnings.push(format!(
                "provider model is unset; pass --model or set provider.model in {}",
                user_config_path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "the trusted user configuration".into())
            ));
            String::new()
        } else {
            user_config.provider.model.clone()
        };

        let project_config_display = project_config
            .is_some()
            .then(|| root.canonical().join(".bollo").join("config.json"));
        Ok(Self {
            root,
            fs,
            snapshot,
            user_config,
            store,
            trust,
            hooks,
            provider: None,
            provider_choice,
            model,
            state_dir,
            user_config_path,
            project_config_path: project_config_display,
            capabilities,
            risk_required,
            risk_acknowledged: cli.acknowledge_risk,
            warnings,
            recovered_operations,
            sandbox: None,
            mcp: None,
            classifier,
        })
    }

    pub fn artifact_dir(&self) -> PathBuf {
        self.state_dir.join(ARTIFACT_DIR)
    }

    /// Commands that can execute project code must acknowledge host-mode risk
    /// explicitly. Never bypassed by configuration.
    pub fn require_risk_acknowledgement(&self) -> Result<(), CliError> {
        if !self.risk_required || self.risk_acknowledged {
            return Ok(());
        }
        let reason = match (self.snapshot.profile, self.snapshot.sandbox) {
            (Profile::Unrestricted, SandboxMode::Off) => {
                "profile unrestricted and sandbox off run with your account's full authority \
                 outside any isolation"
            }
            (Profile::Unrestricted, _) => {
                "profile unrestricted runs commands with your account's full authority"
            }
            (_, SandboxMode::Off) => "sandbox off is host mode, not isolation",
            _ => "this configuration requires an explicit risk acknowledgement",
        };
        Err(CliError::usage(format!("--acknowledge-risk is required: {reason}")))
    }

    /// Start, handshake and discover configured MCP servers, exactly once per
    /// process. Called only on paths that are about to execute a turn; failures
    /// are warnings appended to [`Composition::warnings`].
    pub fn ensure_mcp(&mut self) {
        if self.mcp.is_some() {
            return;
        }
        let servers = self.user_config.mcp_servers.clone();
        let (registry, warnings) = McpRegistry::connect(
            &servers,
            self.root.canonical(),
            &mut self.trust,
        );
        if !registry.tools.is_empty() {
            self.warnings.push(format!(
                "{} MCP tool(s) discovered and added to the model-facing tool list; every call \
                 still passes policy, mode, approval and the operation journal",
                registry.tools.len()
            ));
        }
        self.warnings.extend(warnings);
        self.mcp = Some(registry);
    }

    /// Build the provider adapter on demand, so read-only commands like
    /// `doctor` and `config validate` never require a credential.
    pub fn ensure_provider(&mut self) -> Result<(), CliError> {
        if self.provider.is_none() {
            self.provider = Some(build_provider(&self.provider_choice, &self.snapshot)?);
        }
        Ok(())
    }

    pub fn provider_label(&self) -> String {
        self.provider
            .as_ref()
            .map(|provider| provider.id().to_string())
            .unwrap_or_else(|| {
                self.provider_choice
                    .vendor
                    .map(|vendor| vendor.as_str().to_string())
                    .unwrap_or_else(|| match self.snapshot.provider.kind {
                        ProviderKind::Anthropic => "anthropic".into(),
                        ProviderKind::Xai => "xai".into(),
                    })
            })
    }

    pub fn credential_env(&self) -> String {
        required_credential_env(&self.snapshot)
    }

    /// Whether the credential environment reference is currently present.
    /// Values are never read into diagnostics; only presence is reported.
    pub fn credential_present(&self) -> bool {
        std::env::var(self.credential_env())
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
    }

    /// One-line advisory-classifier posture for `doctor` and
    /// `config validate`. Never includes credential values.
    pub fn classifier_status(&self) -> String {
        match &self.classifier {
            Some(runtime) => format!(
                "attached (model {}; profiles {}; advisory allow→ask only)",
                runtime.model(),
                profile_list(runtime.profiles())
            ),
            None if self.user_config.classifier.enabled => {
                "enabled in config but detached; zero calls in this build".to_string()
            }
            None => "disabled (zero calls)".to_string(),
        }
    }

    pub fn provider_script_label(&self) -> Option<String> {
        self.provider_choice
            .script
            .as_ref()
            .map(|path| path.display().to_string())
    }

    /// Create a fresh session bound to this workspace identity.
    pub fn new_session(&mut self, label: Option<&str>) -> Result<SessionId, CliError> {
        self.store
            .create_session(self.root.identity_hash(), label)
            .map_err(|err| CliError::recovery(format!("cannot create session: {err}")))
    }

    /// Facts recovery needs before a stored session is resumed.
    /// Create the run's enforcement sandbox on first use when workspace mode
    /// was verified for this process. Creation failure refuses the run; there
    /// is no silent fallback to host execution.
    pub fn ensure_sandbox(&mut self) -> Result<(), CliError> {
        if self.sandbox.is_some() || self.snapshot.sandbox != SandboxMode::Workspace {
            return Ok(());
        }
        match create_workspace_sandbox(self.root.canonical()) {
            Ok(sandbox) => {
                self.sandbox = sandbox;
                Ok(())
            }
            Err(err) => Err(CliError::usage(format!(
                "workspace sandbox requested but the container could not be created: {err}"
            ))),
        }
    }

    pub fn recovery_report(&self, session: &SessionId) -> Result<(usize, usize), CliError> {
        let interrupted = self
            .store
            .interrupted_runs(session)
            .map_err(|err| CliError::recovery(format!("cannot read runs: {err}")))?
            .len();
        let unknown = self
            .store
            .operations_for_session(session)
            .map_err(|err| CliError::recovery(format!("cannot read operations: {err}")))?
            .iter()
            .filter(|operation| operation.state == bollo_store::OperationState::Unknown)
            .count();
        Ok((interrupted, unknown))
    }
}

#[derive(Debug, Clone)]
pub struct TurnSettings {
    pub session: SessionId,
    pub mode: ModeKind,
    pub include_claude_md: bool,
}

/// Run one turn through the bounded runtime. The caller owns the sink and the
/// approval channel, so headless and interactive clients share the loop.
#[allow(clippy::too_many_arguments)]
pub fn execute_turn(
    composition: &mut Composition,
    settings: &TurnSettings,
    prompt: &str,
    sink: &mut dyn FnMut(&EventEnvelope),
    channel: &mut dyn ApprovalChannel,
    cancel: CancellationToken,
) -> RunOutcome {
    // Destructuring gives disjoint field borrows, so the provider handle, the
    // immutable policy snapshot and the mutable store can coexist.
    let Composition {
        root,
        fs,
        snapshot,
        store,
        trust,
        hooks,
        provider,
        model,
        mcp,
        classifier,
        sandbox,
        ..
    } = composition;
    let provider: &dyn Provider = provider
        .as_ref()
        .map(|boxed| boxed.as_ref())
        .expect("provider is built before a turn starts");
    let mut approvals = MemoryApprovalStore::new();
    let mut checkpoints = CheckpointLog::new();
    let mut runtime = Runtime::new(
        provider,
        snapshot,
        root.identity_hash().to_string(),
        fs,
        store,
        &mut approvals,
        &mut checkpoints,
        trust,
        channel,
        settings.session.clone(),
        model.clone(),
        settings.mode,
    );
    runtime = runtime
        .with_hooks(hooks.clone())
        .with_cancel(cancel)
        .with_claude_md(settings.include_claude_md)
        .with_sandbox(sandbox.as_deref());
    // Advisory classifier (BH-021): attached only when the trusted config
    // enabled it and this build has a live transport. The gate itself decides
    // eligibility, so deny/ask, read-only effects and unselected profiles
    // still make zero calls.
    if let Some(classifier) = classifier.as_ref() {
        runtime = runtime.with_classifier(classifier.gate());
    }
    if let Some(registry) = mcp.as_mut() {
        // Borrow order matters: clone the discovered tools first, then hand the
        // registry to the runtime as the dispatch port.
        let tools = registry.tools.clone();
        runtime = runtime.with_mcp_tools(tools).with_mcp(registry);
    }
    runtime.run(prompt, sink)
}

/// Adapter from the interactive client's approver to the runtime channel.
pub struct TuiChannel<'a> {
    inner: &'a mut dyn bollo_tui::Approver,
}

impl<'a> TuiChannel<'a> {
    pub fn new(inner: &'a mut dyn bollo_tui::Approver) -> Self {
        Self { inner }
    }
}

impl ApprovalChannel for TuiChannel<'_> {
    fn request(&mut self, request: CoreApprovalRequest) -> Option<bool> {
        self.inner.decide(&bollo_tui::ApprovalPrompt {
            approval_id: request.approval_id.to_string(),
            tool_name: request.tool_name,
            summary: request.summary,
            intent_hash: request.intent_hash,
            policy_revision: request.policy_revision,
            expires_at: request.expires_at,
        })
    }
}

/// Interactive turn runner handed to the TUI. The composition lives in a
/// `RefCell` so the same object can be borrowed for a turn and for a slash
/// command, never at the same time.
pub struct InteractiveRunner<'a> {
    pub composition: &'a std::cell::RefCell<Composition>,
    pub settings: TurnSettings,
    pub cancel: CancellationToken,
}

impl bollo_tui::TurnRunner for InteractiveRunner<'_> {
    fn run_turn(
        &mut self,
        prompt: &str,
        sink: &mut dyn FnMut(&EventEnvelope),
        approvals: &mut dyn bollo_tui::Approver,
    ) -> bollo_tui::TurnOutcome {
        let composition = self.composition.borrow_mut();
        let mut composition = composition;
        if let Err(err) = composition.ensure_provider() {
            eprintln!("bollo: {}", err.message);
            return bollo_tui::TurnOutcome {
                exit_code: err.code,
                summary: err.message,
            };
        }
        composition.ensure_mcp();
        if let Err(err) = composition.ensure_sandbox() {
            eprintln!("bollo: {}", err.message);
            return bollo_tui::TurnOutcome {
                exit_code: err.code,
                summary: err.message,
            };
        }
        let mut channel = TuiChannel::new(approvals);
        let outcome = execute_turn(
            &mut composition,
            &self.settings,
            prompt,
            sink,
            &mut channel,
            self.cancel.clone(),
        );
        bollo_tui::TurnOutcome {
            exit_code: outcome.exit_code,
            summary: summarize(&outcome),
        }
    }
}

pub fn summarize(outcome: &RunOutcome) -> String {
    let state = format!("{:?}", outcome.state).to_lowercase();
    let verification = format!("{:?}", outcome.verification).to_lowercase();
    let reason = outcome
        .reason
        .as_ref()
        .map(|reason| format!(", reason {reason}"))
        .unwrap_or_default();
    // Classifier activity rides the turn summary, never the event stream.
    let classifier = if outcome.classifier.attached {
        format!(" · classifier {}", outcome.classifier.compact())
    } else {
        String::new()
    };
    format!(
        "{state}{reason} · {verification} · {} tool call(s){classifier}",
        outcome.tool_calls
    )
}

// --- configuration discovery -------------------------------------------------

pub fn state_directory(cli: &Cli) -> PathBuf {
    if let Some(dir) = &cli.state_dir {
        return dir.clone();
    }
    if let Some(dir) = std::env::var_os("BOLLO_STATE_DIR") {
        return PathBuf::from(dir);
    }
    platform_state_root().join("bollo")
}

/// User state never lives in the project. Windows uses `%APPDATA%`; other
/// platforms follow the XDG state directory.
fn platform_state_root() -> PathBuf {
    if cfg!(windows) {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata).join("state");
        }
    }
    if let Some(state) = std::env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(state);
    }
    home_dir().join(".local").join("state")
}

fn platform_config_root() -> PathBuf {
    if cfg!(windows) {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata);
        }
    }
    if let Some(config) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(config);
    }
    home_dir().join(".config")
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Trusted user configuration path, overridable for tests via `BOLLO_CONFIG`.
pub fn user_config_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("BOLLO_CONFIG") {
        return Some(PathBuf::from(path));
    }
    Some(platform_config_root().join("bollo").join("config.json"))
}

pub fn default_user_config() -> BolloConfig {
    BolloConfig {
        schema_version: bollo_policy::config::CONFIG_SCHEMA_VERSION.to_string(),
        provider: ProviderConfig {
            kind: ProviderKind::Anthropic,
            base_url: "https://api.anthropic.com".into(),
            model: String::new(),
            credential_env: "ANTHROPIC_API_KEY".into(),
            context_tokens: 200_000,
            input_microusd_per_token: None,
            output_microusd_per_token: None,
        },
        permissions: Default::default(),
        limits: Default::default(),
        privacy: Default::default(),
        mcp_servers: Vec::new(),
        hooks: Vec::new(),
        classifier: ClassifierConfig::default(),
    }
}

fn apply_provider_binding(config: &mut BolloConfig, provider: ProviderArg) {
    let (kind, base_url, credential_env) = match provider {
        ProviderArg::Anthropic => (
            ProviderKind::Anthropic,
            "https://api.anthropic.com",
            "ANTHROPIC_API_KEY",
        ),
        ProviderArg::Xai => (ProviderKind::Xai, "https://api.x.ai", "XAI_API_KEY"),
        // The replay provider never reaches the network; keep the configured
        // vendor binding but let the adapter stand in for it.
        ProviderArg::Replay => (config.provider.kind, "", ""),
    };
    if matches!(kind, ProviderKind::Anthropic | ProviderKind::Xai) {
        config.provider.kind = kind;
        config.provider.base_url = base_url.to_string();
        config.provider.credential_env = credential_env.to_string();
    }
}

fn hook_spec(hook: &HookConfig) -> Result<HookSpec, CliError> {
    let event = match hook.event {
        bollo_policy::config::HookEvent::BeforeTool => bollo_extensions::HookEvent::BeforeTool,
        bollo_policy::config::HookEvent::AfterTool => bollo_extensions::HookEvent::AfterTool,
    };
    Ok(HookSpec {
        id: hook.id.clone(),
        event,
        enabled: hook.enabled,
        argv: hook.argv.clone(),
        timeout_seconds: hook.timeout_seconds,
        env_names: Vec::new(),
    })
}

// --- classifier construction --------------------------------------------------

/// Attach the advisory classifier. The default (non-`live-http`) build returns
/// a diagnostic instead: the gate stays detached and no classifier traffic is
/// possible, which keeps "disabled by default" true even for an enabled block.
#[cfg(feature = "live-http")]
fn build_classifier(config: &ClassifierConfig) -> Result<ClassifierRuntime, String> {
    let settings = bollo_classifier::TypeSafeSettings {
        origin: config.origin.clone(),
        model: config.model.clone(),
        credential_env: config.credential_env.clone(),
        timeout_ms: config.timeout_ms,
    };
    match bollo_classifier::TypeSafeClassifier::live(settings) {
        Ok(classifier) => Ok(ClassifierRuntime::new(Box::new(classifier), config)),
        Err(err) => Err(format!(
            "classifier.enabled is set but the adapter rejected the configuration ({err}); the \
             gate stays detached and no classifier traffic is possible"
        )),
    }
}

#[cfg(not(feature = "live-http"))]
fn build_classifier(_config: &ClassifierConfig) -> Result<ClassifierRuntime, String> {
    Err(
        "classifier.enabled is set but this build has no live HTTPS transport; rebuild with \
         --features live-http. The gate stays detached and no classifier traffic is possible"
            .to_string(),
    )
}

// --- provider construction ---------------------------------------------------

fn build_provider(
    choice: &ProviderChoice,
    snapshot: &PolicySnapshot,
) -> Result<Box<dyn Provider>, CliError> {
    let selected = choice.vendor.unwrap_or(match snapshot.provider.kind {
        ProviderKind::Anthropic => ProviderArg::Anthropic,
        ProviderKind::Xai => ProviderArg::Xai,
    });
    match selected {
        ProviderArg::Replay => {
            let path = choice.script.as_ref().ok_or_else(|| {
                CliError::usage("--provider replay requires --script FILE (deterministic test driver)")
            })?;
            let json = std::fs::read_to_string(path).map_err(|err| {
                CliError::usage(format!("cannot read script {}: {err}", path.display()))
            })?;
            let script = parse_replay_script(&json)?;
            Ok(Box::new(FakeProvider::new("replay", script)))
        }
        vendor => {
            let credential_env = required_credential_env(snapshot);
            let credential = credential(&credential_env)?;
            build_live_provider(vendor, snapshot, &credential)
        }
    }
}

fn required_credential_env(snapshot: &PolicySnapshot) -> String {
    if snapshot.provider.credential_env.trim().is_empty() {
        match snapshot.provider.kind {
            ProviderKind::Anthropic => "ANTHROPIC_API_KEY".to_string(),
            ProviderKind::Xai => "XAI_API_KEY".to_string(),
        }
    } else {
        snapshot.provider.credential_env.clone()
    }
}

/// Presence check only; the value is never printed and never passed to children.
fn credential(env_name: &str) -> Result<String, CliError> {
    match std::env::var(env_name) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(CliError::usage(format!(
            "credential {env_name} is not set in the environment; no --api-key flag exists by design"
        ))),
    }
}

#[cfg(feature = "live-http")]
fn build_live_provider(
    vendor: ProviderArg,
    snapshot: &PolicySnapshot,
    credential: &str,
) -> Result<Box<dyn Provider>, CliError> {
    use std::sync::Arc;

    use bollo_providers::{anthropic::AnthropicAdapter, transport::UreqTransport, xai::XaiAdapter};

    let transport = Arc::new(UreqTransport::new());
    let context_tokens = snapshot.provider.context_tokens;
    let max_output_tokens = snapshot.limits.max_output_tokens;
    let base_url = if snapshot.provider.base_url.trim().is_empty() {
        match vendor {
            ProviderArg::Anthropic => "https://api.anthropic.com".to_string(),
            ProviderArg::Xai => "https://api.x.ai".to_string(),
            ProviderArg::Replay => unreachable!("replay is handled before transport selection"),
        }
    } else {
        snapshot.provider.base_url.clone()
    };
    Ok(match vendor {
        ProviderArg::Anthropic => Box::new(AnthropicAdapter::new(
            transport,
            credential,
            base_url,
            context_tokens,
            max_output_tokens,
        )),
        ProviderArg::Xai => Box::new(XaiAdapter::new(
            transport,
            credential,
            base_url,
            context_tokens,
            max_output_tokens,
        )),
        ProviderArg::Replay => unreachable!("replay is handled before transport selection"),
    })
}

#[cfg(not(feature = "live-http"))]
fn build_live_provider(
    _vendor: ProviderArg,
    _snapshot: &PolicySnapshot,
    _credential: &str,
) -> Result<Box<dyn Provider>, CliError> {
    Err(CliError::usage(
        "this build has no live HTTPS transport; rebuild with --features live-http or drive the \
         runtime with --provider replay --script FILE",
    ))
}

#[derive(Debug, serde::Deserialize)]
struct ReplayTurn {
    #[serde(default)]
    deltas: Vec<String>,
    #[serde(default)]
    tool_calls: Vec<ReplayToolCall>,
    #[serde(default)]
    finish: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
}

#[derive(Debug, serde::Deserialize)]
struct ReplayToolCall {
    call_id: String,
    name: String,
    #[serde(default)]
    arguments: serde_json::Value,
}

/// Scripted responses for the deterministic replay provider. This is a test
/// driver, not a network provider: it cannot be selected accidentally because
/// it requires an explicit `--script` file.
pub fn parse_replay_script(json: &str) -> Result<Vec<ScriptedResponse>, CliError> {
    let turns: Vec<ReplayTurn> = serde_json::from_str(json)
        .map_err(|err| CliError::usage(format!("invalid replay script: {err}")))?;
    if turns.is_empty() {
        return Err(CliError::usage("replay script is empty"));
    }
    let mut responses = Vec::new();
    for turn in turns {
        let tool_intents: Vec<ToolIntent> = turn
            .tool_calls
            .into_iter()
            .map(|call| ToolIntent {
                call_id: call.call_id,
                name: call.name,
                arguments: call.arguments,
            })
            .collect();
        let finish = match turn.finish.as_deref() {
            None => {
                if tool_intents.is_empty() {
                    FinishReason::EndTurn
                } else {
                    FinishReason::ToolUse
                }
            }
            Some("end_turn") => FinishReason::EndTurn,
            Some("tool_use") => FinishReason::ToolUse,
            Some("max_output") => FinishReason::MaxOutput,
            Some("refusal") => FinishReason::Refusal,
            Some(other) => {
                return Err(CliError::usage(format!("unknown finish reason {other:?}")));
            }
        };
        let error = turn.error.map(|message| {
            bollo_providers::ProviderError::new(bollo_protocol::ErrorCode::ProviderError, message)
        });
        responses.push(ScriptedResponse {
            deltas: turn.deltas,
            tool_intents,
            usage: Usage {
                input_tokens: Some(turn.input_tokens.unwrap_or(100)),
                output_tokens: Some(turn.output_tokens.unwrap_or(10)),
            },
            finish,
            error,
        });
    }
    Ok(responses)
}

/// Helper used by `doctor` and tests: is a path inside the state directory?
pub fn is_inside_state(state_dir: &Path, candidate: &Path) -> bool {
    candidate.starts_with(state_dir)
}

#[cfg(test)]
mod classifier_tests {
    use super::*;

    fn outcome_with(classifier: bollo_core::ClassifierAudit) -> RunOutcome {
        RunOutcome {
            run_id: bollo_protocol::ids::RunId::generate(),
            state: bollo_protocol::vocab::TerminalState::Completed,
            reason: None,
            verification: bollo_protocol::vocab::VerificationStatus::Skipped,
            tool_calls: 1,
            usage: Default::default(),
            classifier,
            exit_code: 0,
        }
    }

    /// The turn summary surfaces classifier activity only for instrumented
    /// runs; default runs are unchanged.
    #[test]
    fn summarize_surfaces_classifier_activity_only_when_attached() {
        let plain = summarize(&outcome_with(Default::default()));
        assert_eq!(plain, "completed · skipped · 1 tool call(s)", "{plain}");

        let mut audit = bollo_core::ClassifierAudit {
            attached: true,
            ..Default::default()
        };
        audit.record(
            &bollo_policy::classifier::ClassifierVerdict::available("jev-test", 1.2, 0.9, 0.0),
            true,
        );
        let instrumented = summarize(&outcome_with(audit));
        assert_eq!(
            instrumented, "completed · skipped · 1 tool call(s) · classifier 1 call, 1 escalated",
            "{instrumented}"
        );
    }

    /// The config block maps onto the gate verbatim: profiles, thresholds and
    /// the pinned model, with no widening anywhere.
    #[test]
    fn classifier_runtime_maps_config_onto_the_gate() {
        let config = ClassifierConfig {
            enabled: true,
            origin: ClassifierConfig::default().origin,
            model: "jev-test".into(),
            credential_env: "TYPESAFE_API_KEY".into(),
            timeout_ms: 1500,
            escalate_at: 0.8,
            min_confidence: 0.4,
            credential_threshold: 0.95,
            profiles: vec![Profile::Unrestricted],
        };
        let runtime = ClassifierRuntime::new(
            Box::new(bollo_policy::ScriptedClassifier::new("jev-test", vec![])),
            &config,
        );
        assert_eq!(runtime.model(), "jev-test");
        let gate = runtime.gate();
        assert_eq!(gate.profiles(), &[Profile::Unrestricted]);
        assert_eq!(gate.thresholds().escalate_at, 0.8);
        assert_eq!(gate.thresholds().min_confidence, 0.4);
        assert_eq!(gate.thresholds().credential_threshold, 0.95);
    }
}
