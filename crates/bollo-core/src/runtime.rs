//! The bounded agent loop, exactly the documented per-step order:
//! validate → assemble context → reserve → stream → validate arguments →
//! normalize → trusted pre-hook → policy → mode → approval → journal intent →
//! execute once → after-hook → repeat up to the run ceiling.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;
use time::OffsetDateTime;

use bollo_extensions::hooks::{
    run_after_hook, run_before_hook, HookEvent, HookOutcome, HookPayload, HookSpec,
};
use bollo_extensions::TrustStore;
use bollo_modes::{constrain, effective, ModeDescriptor};
use bollo_policy::approval::{
    pending_receipt, ApprovalState, DEFAULT_APPROVAL_TTL_SECONDS, MemoryApprovalStore,
};
use bollo_policy::evaluate::{evaluate, PolicyDecision};
use bollo_policy::gate::{authorize, Authorization};
use bollo_policy::layers::PolicySnapshot;
use bollo_policy::normalize::intent_hash;
use bollo_policy::{ApprovalExpectation, ApprovalStore};
use bollo_providers::{
    ChatMessage, ContentBlock, ModelRequest, Provider, ProviderEvent, Role, ToolDefinition,
};
use bollo_protocol::cancel::CancellationToken;
use bollo_protocol::errors::{BolloError, ErrorCode};
use bollo_protocol::events::{
    ApprovalOutcome as ApprovalOutcomeEvent, ApprovalRequestedData, ApprovalResolvedData,
    AssistantDeltaData, EventEnvelope, RunFinishedData, RunStartedData, ToolProposedData,
    ToolResultData, VerificationResultData,
};
use bollo_protocol::ids::{ApprovalId, RunId, SessionId, ToolCallId};
use bollo_protocol::vocab::{
    Effect, ModeKind, TerminalState, ToolStatus, VerificationStatus,
};
use bollo_protocol::EventType;
use bollo_store::{CheckpointRecord, OperationState, SqliteStore};
use bollo_tools::execute::{looks_like_verification, McpDispatch};
use bollo_tools::prepare::PreparedAction;
use bollo_tools::{execute_with_mcp, ExecuteContext, McpToolSpec, ToolOutcome};
use bollo_workspace::{Checkpoint, CheckpointLog, WorkspaceFs};

use crate::budget::Budget;
use crate::compaction;
use crate::context;
use crate::scheduler::RunScheduler;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRequest {
    pub approval_id: ApprovalId,
    pub tool_name: String,
    pub summary: String,
    pub intent_hash: String,
    pub policy_revision: u64,
    pub expires_at: String,
}

/// Interactive decision rights. `None` means no channel is available, which
/// blocks the run with `approval_required` — it never defaults to yes.
pub trait ApprovalChannel {
    fn request(&mut self, request: ApprovalRequest) -> Option<bool>;
}

#[derive(Debug, Default)]
pub struct NoChannel;

impl ApprovalChannel for NoChannel {
    fn request(&mut self, _request: ApprovalRequest) -> Option<bool> {
        None
    }
}

/// Deterministic channel for tests and scripted automation.
#[derive(Debug, Default)]
pub struct ScriptedChannel {
    decisions: VecDeque<bool>,
}

impl ScriptedChannel {
    pub fn new(decisions: Vec<bool>) -> Self {
        Self {
            decisions: decisions.into(),
        }
    }
}

impl ApprovalChannel for ScriptedChannel {
    fn request(&mut self, _request: ApprovalRequest) -> Option<bool> {
        self.decisions.pop_front()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfig {
    pub prompt: String,
    pub mode: ModeKind,
    pub include_claude_md: bool,
}

#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub run_id: RunId,
    pub state: TerminalState,
    pub reason: Option<String>,
    pub verification: VerificationStatus,
    pub tool_calls: u32,
    pub usage: crate::budget::UsageTotals,
    pub exit_code: i32,
}

pub struct Runtime<'a> {
    pub provider: &'a dyn Provider,
    pub snapshot: &'a PolicySnapshot,
    pub workspace_identity: String,
    pub fs: &'a WorkspaceFs,
    pub store: &'a mut SqliteStore,
    pub approvals: &'a mut MemoryApprovalStore,
    pub checkpoints: &'a mut CheckpointLog,
    pub trust: &'a TrustStore,
    pub hooks: Vec<HookSpec>,
    pub cancel: CancellationToken,
    pub session: SessionId,
    pub model: String,
    pub mode: ModeDescriptor,
    pub channel: &'a mut dyn ApprovalChannel,
    pub include_claude_md: bool,
    /// Tools discovered from connected MCP servers; they join the model-facing
    /// tool list and are prepared by canonical id like every built-in.
    pub mcp_tools: Vec<McpToolSpec>,
    /// Host-supplied MCP dispatch. Absent means MCP calls fail closed.
    pub mcp: Option<&'a mut dyn McpDispatch>,
}

impl<'a> Runtime<'a> {
    pub fn new(
        provider: &'a dyn Provider,
        snapshot: &'a PolicySnapshot,
        workspace_identity: String,
        fs: &'a WorkspaceFs,
        store: &'a mut SqliteStore,
        approvals: &'a mut MemoryApprovalStore,
        checkpoints: &'a mut CheckpointLog,
        trust: &'a TrustStore,
        channel: &'a mut dyn ApprovalChannel,
        session: SessionId,
        model: String,
        mode: ModeKind,
    ) -> Self {
        let mode = effective(mode, snapshot);
        Self {
            provider,
            snapshot,
            workspace_identity,
            fs,
            store,
            approvals,
            checkpoints,
            trust,
            hooks: Vec::new(),
            cancel: CancellationToken::new(),
            session,
            model,
            mode,
            channel,
            include_claude_md: false,
            mcp_tools: Vec::new(),
            mcp: None,
        }
    }

    /// Add discovered MCP tools to the model-facing tool list.
    pub fn with_mcp_tools(mut self, tools: Vec<McpToolSpec>) -> Self {
        self.mcp_tools = tools;
        self
    }

    /// Attach the host's MCP dispatcher. Calls still pass prepare → policy →
    /// mode → approval → journal before this port is reached.
    pub fn with_mcp(mut self, dispatch: &'a mut dyn McpDispatch) -> Self {
        self.mcp = Some(dispatch);
        self
    }

    pub fn with_hooks(mut self, hooks: Vec<HookSpec>) -> Self {
        self.hooks = hooks;
        self
    }

    /// Poll the MCP port for an announced catalog change and adopt it. Called
    /// before the first model request and after every executed tool call, so a
    /// server that adds or removes tools mid-session changes both the
    /// model-facing list and the catalog `prepare` resolves proposals against.
    fn refresh_mcp_catalog(&mut self) -> bool {
        let refreshed = match self.mcp.as_deref_mut() {
            Some(port) => port.refresh_catalog(),
            None => None,
        };
        match refreshed {
            Some(tools) => {
                self.mcp_tools = tools;
                true
            }
            None => false,
        }
    }

    pub fn with_cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = cancel;
        self
    }

    pub fn with_claude_md(mut self, include: bool) -> Self {
        self.include_claude_md = include;
        self
    }

    pub fn run(&mut self, prompt: &str, sink: &mut dyn FnMut(&EventEnvelope)) -> RunOutcome {
        let provider = self.provider;
        let snapshot = self.snapshot;
        let fs = self.fs;
        let trust = self.trust;
        let cancel = self.cancel.clone();
        let mode = self.mode.clone();
        let session = self.session.clone();
        let workspace_identity = self.workspace_identity.clone();
        let model = self.model.clone();
        let include_claude_md = self.include_claude_md;

        let run_id = match self
            .store
            .start_run(&session, provider.id(), &model, snapshot.revision)
        {
            Ok(run_id) => run_id,
            Err(err) => {
                return RunOutcome {
                    run_id: RunId::generate(),
                    state: TerminalState::Failed,
                    reason: Some(format!("store: {err}")),
                    verification: VerificationStatus::Skipped,
                    tool_calls: 0,
                    usage: Default::default(),
                    exit_code: 1,
                }
            }
        };

        let started = RunStartedData {
            state: "running".into(),
            policy_revision: snapshot.revision,
            provider: provider.id().to_string(),
            model: model.clone(),
        };
        let _ = emit(
            self.store,
            &session,
            &run_id,
            EventType::RunStarted,
            &serde_json::to_value(&started).expect("serializable"),
            sink,
        );

        let scheduler = RunScheduler::new(cancel.clone());
        let mut budget = Budget::new(
            snapshot.limits.clone(),
            snapshot.pricing_known,
            snapshot.provider.input_microusd_per_token,
            snapshot.provider.output_microusd_per_token,
        );
        let instructions = context::discover_instructions(fs, include_claude_md);
        let system = context::system_prompt(&instructions);
        let mut messages = vec![ChatMessage::user_text(prompt)];
        let mut verification = VerificationStatus::Skipped;
        let workspace_root: PathBuf = fs
            .resolve(".")
            .unwrap_or_else(|_| PathBuf::from("."));
        // Apply any catalog change announced before this run started; the
        // list itself is rebuilt per model request below, so a change announced
        // mid-run reaches the very next request.
        self.refresh_mcp_catalog();

        loop {
            if scheduler.ensure_running().is_err() {
                return self.finish(run_id, TerminalState::Cancelled, None, verification, &budget, sink);
            }
            if let Err(err) = budget.before_request(
                compaction::estimate_tokens(&messages),
                snapshot.limits.max_output_tokens,
            ) {
                return self.finish(
                    run_id,
                    TerminalState::Failed,
                    Some(err.code.as_str()),
                    verification,
                    &budget,
                    sink,
                );
            }
            messages = compaction::compact(
                std::mem::take(&mut messages),
                snapshot.provider.context_tokens,
            )
            .messages;

            // The model-facing list is the built-in registry plus every
            // discovered MCP tool. Collisions are dropped by `tool_specs`,
            // never renamed onto a built-in.
            let tool_definitions: Vec<ToolDefinition> = bollo_tools::tool_specs(&self.mcp_tools)
                .specs
                .into_iter()
                .map(|spec| ToolDefinition {
                    name: spec.provider_name,
                    description: spec.description,
                    input_schema: spec.input_schema,
                })
                .collect();
            let request = ModelRequest {
                model: model.clone(),
                system: system.clone(),
                messages: messages.clone(),
                tools: tool_definitions,
                max_output_tokens: snapshot.limits.max_output_tokens,
                deadline: Duration::from_secs(120),
            };
            let mut text = String::new();
            let stream_result = {
                let store_ref = &mut *self.store;
                let session_ref = &session;
                let mut on_event = |event: ProviderEvent| {
                    if let ProviderEvent::TextDelta(delta) = event {
                        if text.len() < 65_536 {
                            text.push_str(&delta);
                        }
                        let data = serde_json::to_value(AssistantDeltaData { text: delta })
                            .expect("serializable");
                        let _ = emit(
                            store_ref,
                            session_ref,
                            &run_id,
                            EventType::AssistantDelta,
                            &data,
                            sink,
                        );
                    }
                };
                provider.stream(&request, &cancel, &mut on_event)
            };
            let summary = match stream_result {
                Ok(summary) => summary,
                Err(err) => {
                    let state = if err.code == ErrorCode::Cancelled {
                        TerminalState::Cancelled
                    } else {
                        TerminalState::Failed
                    };
                    return self.finish(run_id, state, Some(err.code.as_str()), verification, &budget, sink);
                }
            };
            budget.record_usage(summary.usage.input_tokens, summary.usage.output_tokens);
            let usage = budget.usage().clone();
            let _ = emit(
                self.store,
                &session,
                &run_id,
                EventType::UsageUpdated,
                &json!({
                    "input_tokens": summary.usage.input_tokens,
                    "output_tokens": summary.usage.output_tokens,
                    "cost_microusd": usage.cost_microusd,
                    "cost_known": usage.cost_known,
                }),
                sink,
            );

            let mut assistant_blocks = Vec::new();
            if !text.is_empty() {
                assistant_blocks.push(ContentBlock::Text { text: text.clone() });
            }
            for intent in &summary.tool_intents {
                assistant_blocks.push(ContentBlock::ToolUse {
                    call_id: intent.call_id.clone(),
                    name: intent.name.clone(),
                    arguments: intent.arguments.clone(),
                });
            }
            if !assistant_blocks.is_empty() {
                messages.push(ChatMessage {
                    role: Role::Assistant,
                    content: assistant_blocks,
                });
            }
            if summary.tool_intents.is_empty() {
                return self.finish(run_id, TerminalState::Completed, None, verification, &budget, sink);
            }

            for intent in &summary.tool_intents {
                if scheduler.ensure_running().is_err() {
                    return self.finish(run_id, TerminalState::Cancelled, None, verification, &budget, sink);
                }
                if let Err(err) = budget.next_tool_call() {
                    return self.finish(
                        run_id,
                        TerminalState::Failed,
                        Some(err.code.as_str()),
                        verification,
                        &budget,
                        sink,
                    );
                }
                let tool_call_id =
                    ToolCallId::parse(intent.call_id.clone()).unwrap_or_else(|_| ToolCallId::generate());

                let prepared = match bollo_tools::prepare_with_catalog(
                    &intent.name,
                    &intent.arguments,
                    fs,
                    &fs.policy_root(),
                    &self.mcp_tools,
                ) {
                    Ok(prepared) => prepared,
                    Err(err) => {
                        record_denied(
                            self.store,
                            &session,
                            &run_id,
                            &tool_call_id,
                            &format!("{}: {}", err.code, err.message),
                            sink,
                        );
                        messages.push(tool_result_message(
                            &intent.call_id,
                            format!("{}: {}", err.code, err.message),
                            true,
                        ));
                        continue;
                    }
                };

                let decision = evaluate(&prepared.intent, snapshot);
                let constrained = constrain(
                    &mode,
                    prepared.class,
                    prepared.intent.effect,
                    decision.effect,
                );
                let mut effect = constrained.effect;
                let mut reason = constrained
                    .reason
                    .clone()
                    .unwrap_or_else(|| decision.reason.clone());
                let mode_denied = constrained.mode_denied;

                if effect != Effect::Deny {
                    let before_hooks: Vec<HookSpec> = self
                        .hooks
                        .iter()
                        .filter(|hook| hook.event == HookEvent::BeforeTool)
                        .cloned()
                        .collect();
                    for hook in before_hooks {
                        let payload = hook_payload(
                            HookEvent::BeforeTool,
                            &session,
                            &run_id,
                            &tool_call_id,
                            &prepared.tool_name,
                            &intent_hash(&prepared.intent),
                            &prepared.intent.summary,
                            None,
                        );
                        match run_before_hook(&hook, &payload, &workspace_root, trust, &cancel) {
                            HookOutcome::Continue => {}
                            HookOutcome::Denied { reason: hook_reason } => {
                                effect = Effect::Deny;
                                reason = hook_reason;
                                break;
                            }
                            HookOutcome::Recorded { .. } => {}
                        }
                    }
                }

                let proposed = ToolProposedData {
                    tool_call_id: tool_call_id.clone(),
                    tool_name: prepared.tool_name.clone(),
                    intent_hash: intent_hash(&prepared.intent),
                    decision: effect,
                    reason: reason.clone(),
                };
                let _ = emit(
                    self.store,
                    &session,
                    &run_id,
                    EventType::ToolProposed,
                    &serde_json::to_value(&proposed).expect("serializable"),
                    sink,
                );

                if effect == Effect::Deny {
                    let summary_text = if mode_denied {
                        format!("mode_denied: {reason}")
                    } else {
                        format!("policy_denied: {reason}")
                    };
                    record_denied(
                        self.store,
                        &session,
                        &run_id,
                        &tool_call_id,
                        &summary_text,
                        sink,
                    );
                    messages.push(tool_result_message(
                        &intent.call_id,
                        summary_text,
                        true,
                    ));
                    continue;
                }

                let mut approval_id: Option<ApprovalId> = None;
                if effect == Effect::Ask {
                    let receipt = pending_receipt(
                        &prepared.intent,
                        session.clone(),
                        run_id.clone(),
                        tool_call_id.clone(),
                        workspace_identity.clone(),
                        snapshot.revision,
                        prepared.target_preimage.clone(),
                        OffsetDateTime::now_utc(),
                        DEFAULT_APPROVAL_TTL_SECONDS,
                    );
                    approval_id = Some(receipt.approval_id.clone());
                    let _ = self.approvals.insert(receipt.clone());
                    let requested = ApprovalRequestedData {
                        approval_id: receipt.approval_id.clone(),
                        tool_call_id: tool_call_id.clone(),
                        intent_hash: receipt.intent_hash.clone(),
                        policy_revision: snapshot.revision,
                        expires_at: receipt.expires_at.clone(),
                        summary: prepared.intent.summary.clone(),
                    };
                    let _ = emit(
                        self.store,
                        &session,
                        &run_id,
                        EventType::ApprovalRequested,
                        &serde_json::to_value(&requested).expect("serializable"),
                        sink,
                    );
                    let request = ApprovalRequest {
                        approval_id: receipt.approval_id.clone(),
                        tool_name: prepared.tool_name.clone(),
                        summary: prepared.intent.summary.clone(),
                        intent_hash: receipt.intent_hash.clone(),
                        policy_revision: snapshot.revision,
                        expires_at: receipt.expires_at.clone(),
                    };
                    match self.channel.request(request) {
                        None => {
                            let _ = emit_run_finished(
                                self.store,
                                &session,
                                &run_id,
                                TerminalState::Blocked,
                                Some("approval_required"),
                                verification,
                                sink,
                            );
                            let _ = self.store.finish_run(
                                &run_id,
                                TerminalState::Blocked,
                                Some("approval_required"),
                                verification,
                            );
                            return RunOutcome {
                                run_id,
                                state: TerminalState::Blocked,
                                reason: Some("approval_required".into()),
                                verification,
                                tool_calls: budget.tool_calls(),
                                usage: usage.clone(),
                                exit_code: 3,
                            };
                        }
                        Some(true) => {
                            let _ = self.approvals.resolve(
                                &receipt.approval_id,
                                ApprovalState::Approved,
                                None,
                                OffsetDateTime::now_utc(),
                            );
                            let resolved = ApprovalResolvedData {
                                approval_id: receipt.approval_id.clone(),
                                decision: ApprovalOutcomeEvent::Approve,
                            };
                            let _ = emit(
                                self.store,
                                &session,
                                &run_id,
                                EventType::ApprovalResolved,
                                &serde_json::to_value(&resolved).expect("serializable"),
                                sink,
                            );
                            effect = Effect::Allow;
                        }
                        Some(false) => {
                            let _ = self.approvals.resolve(
                                &receipt.approval_id,
                                ApprovalState::Denied,
                                None,
                                OffsetDateTime::now_utc(),
                            );
                            let resolved = ApprovalResolvedData {
                                approval_id: receipt.approval_id.clone(),
                                decision: ApprovalOutcomeEvent::Deny,
                            };
                            let _ = emit(
                                self.store,
                                &session,
                                &run_id,
                                EventType::ApprovalResolved,
                                &serde_json::to_value(&resolved).expect("serializable"),
                                sink,
                            );
                            let summary_text = "approval denied by the user".to_string();
                            record_denied(
                                self.store,
                                &session,
                                &run_id,
                                &tool_call_id,
                                &summary_text,
                                sink,
                            );
                            messages.push(tool_result_message(
                                &intent.call_id,
                                summary_text,
                                true,
                            ));
                            continue;
                        }
                    }
                }

                let gate_decision = PolicyDecision {
                    effect,
                    rule_id: decision.rule_id.clone(),
                    provenance: decision.provenance.clone(),
                    reason: reason.clone(),
                };
                let expectation = ApprovalExpectation::for_intent(
                    &prepared.intent,
                    snapshot.revision,
                    &workspace_identity,
                    prepared.target_preimage.clone(),
                );
                let authorization: Authorization = match authorize(
                    &gate_decision,
                    &prepared.intent,
                    approval_id.as_ref(),
                    &expectation,
                    self.approvals,
                    OffsetDateTime::now_utc(),
                ) {
                    Ok(authorization) => authorization,
                    Err(err) => {
                        record_denied(
                            self.store,
                            &session,
                            &run_id,
                            &tool_call_id,
                            &format!("{}: {}", err.code, err.message),
                            sink,
                        );
                        messages.push(tool_result_message(
                            &intent.call_id,
                            format!("{}: {}", err.code, err.message),
                            true,
                        ));
                        continue;
                    }
                };

                // Journal intent *before* any effect. If this fails (for
                // example disk full), the effect is refused.
                if let Err(err) = self.store.record_operation_intent(
                    &run_id,
                    tool_call_id.as_str(),
                    &prepared.tool_name,
                    authorization.intent_hash(),
                ) {
                    let summary_text = format!(
                        "operation_unknown: refusing effect because the intent could not be journaled ({err})"
                    );
                    let _ = emit(
                        self.store,
                        &session,
                        &run_id,
                        EventType::ToolResult,
                        &serde_json::to_value(ToolResultData {
                            tool_call_id: tool_call_id.clone(),
                            status: ToolStatus::Unknown,
                            artifact_id: None,
                            summary: summary_text.clone(),
                        })
                        .expect("serializable"),
                        sink,
                    );
                    messages.push(tool_result_message(&intent.call_id, summary_text, true));
                    continue;
                }

                // Exactly one side-effecting action is in flight at a time;
                // the guard is released on every exit path, including errors.
                let effect_guard = match scheduler.begin_effect() {
                    Ok(guard) => guard,
                    Err(err) => {
                        record_denied(
                            self.store,
                            &session,
                            &run_id,
                            &tool_call_id,
                            &format!("{}: {}", err.code, err.message),
                            sink,
                        );
                        messages.push(tool_result_message(
                            &intent.call_id,
                            format!("{}: {}", err.code, err.message),
                            true,
                        ));
                        continue;
                    }
                };
                let outcome = {
                    // Explicit reborrows keep both mutable borrows local to the
                    // effect, so the MCP port cannot outlive the run loop.
                    let checkpoints: &mut CheckpointLog = &mut *self.checkpoints;
                    let mut context =
                        ExecuteContext::new(fs, checkpoints, snapshot.limits.tool_output_bytes);
                    execute_with_mcp(
                        &prepared,
                        &authorization,
                        &mut context,
                        self.mcp.as_deref_mut(),
                    )
                };
                drop(effect_guard);

                // Mirror captured checkpoints into the durable store so a later
                // process can preview and conditionally restore them. The upsert
                // is idempotent and never clears a previous restore marker.
                let captured: Vec<Checkpoint> = self.checkpoints.all().cloned().collect();
                for checkpoint in captured {
                    let _ = self.store.record_checkpoint(&CheckpointRecord {
                        id: checkpoint.id.to_string(),
                        session_id: session.clone(),
                        run_id: Some(run_id.clone()),
                        path: checkpoint.path.clone(),
                        existed: checkpoint.existed,
                        pre_sha256: checkpoint.pre_sha256.clone(),
                        post_sha256: checkpoint.post_sha256.clone(),
                        pre_text: checkpoint.pre_text.clone(),
                        captured_at: checkpoint.captured_at.clone(),
                        restored_at: None,
                    });
                }
                let operation_state = match outcome.status {
                    ToolStatus::Succeeded => OperationState::Succeeded,
                    ToolStatus::Failed | ToolStatus::Denied => OperationState::Failed,
                    ToolStatus::Unknown => OperationState::Unknown,
                };
                let _ = self.store.record_operation_result(
                    &run_id,
                    tool_call_id.as_str(),
                    operation_state,
                    None,
                );
                let status_text = status_str(outcome.status);
                let _ = emit(
                    self.store,
                    &session,
                    &run_id,
                    EventType::ToolResult,
                    &serde_json::to_value(ToolResultData {
                        tool_call_id: tool_call_id.clone(),
                        status: outcome.status,
                        artifact_id: None,
                        summary: truncate(&outcome.summary, 8_192),
                    })
                    .expect("serializable"),
                    sink,
                );

                if let PreparedAction::Exec { argv, .. } = &prepared.action {
                    if looks_like_verification(argv) {
                        verification = match outcome.status {
                            ToolStatus::Succeeded => VerificationStatus::Passed,
                            ToolStatus::Unknown => VerificationStatus::Inconclusive,
                            _ => VerificationStatus::Failed,
                        };
                        let exit_code = outcome
                            .data
                            .get("exit_code")
                            .and_then(|value| value.as_i64());
                        let result = VerificationResultData {
                            status: verification,
                            command: Some(argv.clone()),
                            exit_code,
                            artifact_id: None,
                        };
                        let _ = emit(
                            self.store,
                            &session,
                            &run_id,
                            EventType::VerificationResult,
                            &serde_json::to_value(&result).expect("serializable"),
                            sink,
                        );
                    }
                }

                messages.push(tool_result_message(
                    &intent.call_id,
                    summarize_tool_outcome(&outcome),
                    outcome.status != ToolStatus::Succeeded,
                ));

                // A server may announce a catalog change at any point (for
                // example, the call itself added a tool). Picking it up here
                // makes the refreshed catalog available to this run's next
                // prepare and next model request.
                self.refresh_mcp_catalog();

                let after_hooks: Vec<HookSpec> = self
                    .hooks
                    .iter()
                    .filter(|hook| hook.event == HookEvent::AfterTool)
                    .cloned()
                    .collect();
                for hook in after_hooks {
                    let payload = hook_payload(
                        HookEvent::AfterTool,
                        &session,
                        &run_id,
                        &tool_call_id,
                        &prepared.tool_name,
                        &intent_hash(&prepared.intent),
                        &prepared.intent.summary,
                        Some(status_text),
                    );
                    let _ = run_after_hook(&hook, &payload, &workspace_root, trust);
                }
            }
        }
    }

    fn finish(
        &mut self,
        run_id: RunId,
        state: TerminalState,
        reason: Option<&str>,
        verification: VerificationStatus,
        budget: &Budget,
        sink: &mut dyn FnMut(&EventEnvelope),
    ) -> RunOutcome {
        let _ = emit_run_finished(
            self.store,
            &self.session,
            &run_id,
            state,
            reason,
            verification,
            sink,
        );
        let _ = self.store.finish_run(&run_id, state, reason, verification);
        RunOutcome {
            run_id,
            state,
            reason: reason.map(str::to_string),
            verification,
            tool_calls: budget.tool_calls(),
            usage: budget.usage().clone(),
            exit_code: crate::exit_code_for(state, reason),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn hook_payload(
    event: HookEvent,
    session: &SessionId,
    run: &RunId,
    tool_call: &ToolCallId,
    tool_name: &str,
    intent_hash: &str,
    summary: &str,
    result_status: Option<&str>,
) -> HookPayload {
    HookPayload {
        schema_version: "0.1".into(),
        event,
        session_id: session.to_string(),
        run_id: run.to_string(),
        tool_call_id: tool_call.to_string(),
        tool_name: tool_name.to_string(),
        intent_hash: intent_hash.to_string(),
        summary: summary.to_string(),
        result_status: result_status.map(str::to_string),
    }
}

fn emit(
    store: &mut SqliteStore,
    session: &SessionId,
    run: &RunId,
    event_type: EventType,
    data: &serde_json::Value,
    sink: &mut dyn FnMut(&EventEnvelope),
) -> Result<EventEnvelope, BolloError> {
    let envelope = store
        .append_event(session, Some(run), event_type, data)
        .map_err(|err| BolloError::new(ErrorCode::Internal, err.to_string()))?;
    sink(&envelope);
    Ok(envelope)
}

#[allow(clippy::too_many_arguments)]
fn emit_run_finished(
    store: &mut SqliteStore,
    session: &SessionId,
    run: &RunId,
    state: TerminalState,
    reason: Option<&str>,
    verification: VerificationStatus,
    sink: &mut dyn FnMut(&EventEnvelope),
) -> Result<EventEnvelope, BolloError> {
    let data = RunFinishedData {
        state,
        reason: reason.map(str::to_string),
        verification,
    };
    emit(
        store,
        session,
        run,
        EventType::RunFinished,
        &serde_json::to_value(&data).expect("serializable"),
        sink,
    )
}

fn record_denied(
    store: &mut SqliteStore,
    session: &SessionId,
    run: &RunId,
    tool_call: &ToolCallId,
    summary: &str,
    sink: &mut dyn FnMut(&EventEnvelope),
) {
    let data = ToolResultData {
        tool_call_id: tool_call.clone(),
        status: ToolStatus::Denied,
        artifact_id: None,
        summary: truncate(summary, 8_192),
    };
    let _ = emit(
        store,
        session,
        run,
        EventType::ToolResult,
        &serde_json::to_value(&data).expect("serializable"),
        sink,
    );
}

fn tool_result_message(call_id: &str, content: String, is_error: bool) -> ChatMessage {
    ChatMessage {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            call_id: call_id.to_string(),
            content,
            is_error,
        }],
    }
}

fn summarize_tool_outcome(outcome: &ToolOutcome) -> String {
    if let Some(error) = &outcome.error {
        return truncate(&format!("{}: {}", error.code, error.message), 8_192);
    }
    let data = serde_json::to_string(&outcome.data).unwrap_or_else(|_| "{}".into());
    truncate(&data, 8_192)
}

fn status_str(status: ToolStatus) -> &'static str {
    match status {
        ToolStatus::Succeeded => "succeeded",
        ToolStatus::Failed => "failed",
        ToolStatus::Denied => "denied",
        ToolStatus::Unknown => "unknown",
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        text.to_string()
    } else {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &text[..end])
    }
}
