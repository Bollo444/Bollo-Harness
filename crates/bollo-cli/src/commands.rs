//! Subcommand implementations. Every mutating command funnels through the same
//! runtime loop; the read-only commands reuse the same policy snapshot, so what
//! `bollo policy show` prints is exactly what the run enforces.

use std::cell::RefCell;
use std::io::IsTerminal;
use std::path::PathBuf;

use bollo_core::{NoChannel, RunOutcome};
use bollo_modes::{constrain, effective};
use bollo_policy::{evaluate, validate_startup};
use bollo_protocol::cancel::CancellationToken;
use bollo_protocol::events::EventEnvelope;
use bollo_protocol::ids::{CheckpointId, SessionId};
use bollo_protocol::vocab::{ModeKind, SandboxMode};
use bollo_protocol::{ndjson, EventType};
use bollo_store::{CheckpointRecord, SqliteStore};
use bollo_tui::{Presenter, SlashCommands, Terminal, TuiSession};
use bollo_workspace::{Checkpoint, CheckpointLog, RestorePlan};

use crate::args::*;
use crate::composition::{self, summarize, Composition, InteractiveRunner, TurnSettings};
use crate::render::HeadlessRenderer;
use crate::CliError;

/// Prompt files are read locally with a size cap.
pub const MAX_PROMPT_BYTES: u64 = 65_536;

pub fn dispatch(cli: Cli) -> Result<i32, CliError> {
    match &cli.command {
        Some(Command::Run(args)) => cmd_run(&cli, args),
        Some(Command::Resume(args)) => cmd_resume(&cli, args),
        Some(Command::Sessions(args)) => cmd_sessions(&cli, args),
        Some(Command::Policy(args)) => cmd_policy(&cli, args),
        Some(Command::Config(args)) => cmd_config(&cli, args),
        Some(Command::Doctor) => cmd_doctor(&cli),
        Some(Command::Mcp(args)) => cmd_mcp(&cli, args),
        Some(Command::Checkpoint(args)) => cmd_checkpoint(&cli, args),
        None => cmd_interactive(&cli),
    }
}

fn resolve_prompt(
    prompt: &Option<String>,
    prompt_file: &Option<PathBuf>,
) -> Result<String, CliError> {
    match (prompt, prompt_file) {
        (Some(text), _) => Ok(text.clone()),
        (None, Some(path)) => {
            let metadata = std::fs::metadata(path).map_err(|err| {
                CliError::usage(format!("cannot read prompt file {}: {err}", path.display()))
            })?;
            if !metadata.is_file() {
                return Err(CliError::usage(format!(
                    "prompt file {} is not a regular file",
                    path.display()
                )));
            }
            if metadata.len() > MAX_PROMPT_BYTES {
                return Err(CliError::usage(format!(
                    "prompt file {} exceeds the {MAX_PROMPT_BYTES}-byte cap",
                    path.display()
                )));
            }
            let bytes = std::fs::read(path).map_err(|err| {
                CliError::usage(format!("cannot read prompt file {}: {err}", path.display()))
            })?;
            if bytes.contains(&0) {
                return Err(CliError::usage("prompt file contains binary content"));
            }
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        }
        (None, None) => Err(CliError::usage(
            "missing prompt: pass --prompt TEXT or --prompt-file PATH",
        )),
    }
}

fn note_warnings(composition: &Composition) {
    for warning in &composition.warnings {
        eprintln!("bollo: note: {warning}");
    }
}

fn work_mode(cli: &Cli) -> ModeKind {
    cli.mode.map(Into::into).unwrap_or(ModeKind::Build)
}

fn report_outcome(outcome: &RunOutcome) {
    eprintln!("bollo: run {} {}", outcome.run_id, summarize(outcome));
    eprintln!(
        "bollo: verification is reported separately from completion; inspect the \
         verification_result events"
    );
}

// --- run / resume ------------------------------------------------------------

fn cmd_run(cli: &Cli, args: &RunArgs) -> Result<i32, CliError> {
    let prompt = resolve_prompt(&args.prompt, &args.prompt_file)?;
    let mut composition = Composition::build(cli)?;
    composition.require_risk_acknowledgement()?;
    composition.ensure_provider()?;
    composition.ensure_mcp();
    note_warnings(&composition);
    let session = composition.new_session(Some("run"))?;
    let cancel = CancellationToken::new();
    crate::install_cancel_handler(cancel.clone());

    let mut renderer =
        HeadlessRenderer::new(matches!(args.output, OutputArg::Ndjson), cancel.clone());
    let settings = TurnSettings {
        session,
        mode: work_mode(cli),
        include_claude_md: args.include_claude_md,
    };
    let mut channel = NoChannel;
    let outcome = {
        let mut sink = |event: &EventEnvelope| renderer.on_event(event);
        composition::execute_turn(
            &mut composition,
            &settings,
            &prompt,
            &mut sink,
            &mut channel,
            cancel.clone(),
        )
    };
    report_outcome(&outcome);
    if renderer.broken_pipe() {
        return Ok(130);
    }
    Ok(outcome.exit_code)
}

fn cmd_resume(cli: &Cli, args: &ResumeArgs) -> Result<i32, CliError> {
    let prompt = resolve_prompt(&args.prompt, &args.prompt_file)?;
    let session = SessionId::parse(args.session_id.clone())
        .map_err(|err| CliError::usage(format!("invalid session id: {err}")))?;
    let mut composition = Composition::build(cli)?;
    composition.require_risk_acknowledgement()?;
    composition.ensure_provider()?;
    composition.ensure_mcp();
    note_warnings(&composition);
    // A stored session must exist; resuming never creates one implicitly.
    composition
        .store
        .last_seq(&session)
        .map_err(|_| CliError::recovery(format!("no stored session {session}")))?;

    let (interrupted, unknown) = composition.recovery_report(&session)?;
    if unknown > 0 {
        if !args.acknowledge_unknown {
            return Err(CliError::recovery(format!(
                "session {session} has {unknown} unknown operation(s) and {interrupted} interrupted \
                 run(s); inspect them first (`bollo sessions export {} --output out.ndjson`) and \
                 pass --acknowledge-unknown to continue. Unknown effects are never replayed.",
                session.as_str()
            )));
        }
        eprintln!(
            "bollo: acknowledged {unknown} unknown operation(s) from an interrupted run; they are \
             recorded as unknown and not replayed"
        );
    } else if interrupted > 0 {
        eprintln!(
            "bollo: found {interrupted} interrupted run(s) with no unknown operations; starting a \
             new run in this session"
        );
    }

    let cancel = CancellationToken::new();
    crate::install_cancel_handler(cancel.clone());
    let mut renderer =
        HeadlessRenderer::new(matches!(args.output, OutputArg::Ndjson), cancel.clone());
    let settings = TurnSettings {
        session,
        mode: work_mode(cli),
        include_claude_md: false,
    };
    let mut channel = NoChannel;
    let outcome = {
        let mut sink = |event: &EventEnvelope| renderer.on_event(event);
        composition::execute_turn(
            &mut composition,
            &settings,
            &prompt,
            &mut sink,
            &mut channel,
            cancel.clone(),
        )
    };
    report_outcome(&outcome);
    if renderer.broken_pipe() {
        return Ok(130);
    }
    Ok(outcome.exit_code)
}

// --- sessions ----------------------------------------------------------------

fn cmd_sessions(cli: &Cli, args: &SessionsArgs) -> Result<i32, CliError> {
    let mut composition = Composition::build(cli)?;
    match &args.command {
        SessionsCommand::List => {
            let sessions = composition
                .store
                .list_sessions()
                .map_err(|err| CliError::recovery(format!("cannot list sessions: {err}")))?;
            if sessions.is_empty() {
                println!("no local sessions in {}", composition.state_dir.display());
                return Ok(0);
            }
            for session in sessions {
                println!(
                    "{}\t{}\tseq={}\t{}",
                    session.id.as_str(),
                    session.created_at,
                    session.last_seq,
                    session.label.unwrap_or_else(|| "-".into())
                );
            }
            Ok(0)
        }
        SessionsCommand::Export { id, output } => {
            let session = parse_session(id)?;
            let events = composition
                .store
                .replay(&session, 0)
                .map_err(|err| CliError::recovery(format!("cannot replay session: {err}")))?;
            let mut file = std::fs::File::create(output).map_err(|err| {
                CliError::usage(format!("cannot create {}: {err}", output.display()))
            })?;
            for event in &events {
                ndjson::write_event(&mut file, event).map_err(|err| {
                    CliError::runtime(format!("cannot write {}: {err}", output.display()))
                })?;
            }
            eprintln!(
                "bollo: exported {} event(s) to {} (tool summaries and hashes only; provider \
                 credentials never enter the journal)",
                events.len(),
                output.display()
            );
            Ok(0)
        }
        SessionsCommand::Delete { id, yes } => {
            if !yes {
                return Err(CliError::usage(
                    "refusing to delete local session state without --yes",
                ));
            }
            let session = parse_session(id)?;
            let deleted = composition
                .store
                .delete_session(&session)
                .map_err(|err| CliError::recovery(format!("cannot delete session: {err}")))?;
            eprintln!(
                "bollo: deleted {deleted} local row(s) for session {session}; source files were \
                 never touched"
            );
            Ok(0)
        }
    }
}

fn parse_session(id: &str) -> Result<SessionId, CliError> {
    SessionId::parse(id.to_string())
        .map_err(|err| CliError::usage(format!("invalid session id {id:?}: {err}")))
}

// --- policy ------------------------------------------------------------------

fn cmd_policy(cli: &Cli, args: &PolicyArgs) -> Result<i32, CliError> {
    let composition = Composition::build(cli)?;
    match &args.command {
        PolicyCommand::Show => {
            for line in policy_lines(&composition) {
                println!("{line}");
            }
            Ok(0)
        }
        PolicyCommand::Explain { intent } => explain_intent(&composition, intent, work_mode(cli)),
    }
}

fn policy_lines(composition: &Composition) -> Vec<String> {
    let snapshot = &composition.snapshot;
    let mut lines = Vec::new();
    lines.push(format!(
        "workspace      {} (identity {})",
        composition.root.policy_root(),
        &composition.root.identity_hash()[..12]
    ));
    lines.push(format!("profile        {}", snapshot.profile.as_str()));
    lines.push(format!(
        "sandbox        {} (probe: {})",
        match snapshot.sandbox {
            SandboxMode::Workspace => "workspace",
            SandboxMode::Off => "off",
        },
        snapshot.capabilities.backend
    ));
    lines.push(format!(
        "provider       {} model={} credential={} ({})",
        composition.provider_label(),
        if composition.model.is_empty() {
            "<unset>"
        } else {
            composition.model.as_str()
        },
        composition.credential_env(),
        if composition.credential_present() {
            "present"
        } else {
            "absent"
        }
    ));
    lines.push(format!(
        "limits         tool_calls={} run_seconds={} max_output_tokens={} spend_cents={} tool_output_bytes={}",
        snapshot.limits.max_tool_calls,
        snapshot.limits.max_run_seconds,
        snapshot.limits.max_output_tokens,
        snapshot
            .limits
            .max_spend_cents
            .map(|cents| cents.to_string())
            .unwrap_or_else(|| "unbounded".into()),
        snapshot.limits.tool_output_bytes
    ));
    lines.push(format!("policy         revision {}", snapshot.revision));
    lines.push("rules:".to_string());
    if snapshot.rules.is_empty() {
        lines.push("  (no explicit rules; profile defaults apply)".to_string());
    }
    for rule in &snapshot.rules {
        lines.push(format!(
            "  {} {} tool={}{}{} [{}]",
            rule.id,
            rule.effect.as_str(),
            rule.tool,
            rule.path_glob
                .as_ref()
                .map(|glob| format!(" path={glob}"))
                .unwrap_or_default(),
            rule.argv_prefix
                .as_ref()
                .map(|argv| format!(" argv_prefix={}", argv.join(" ")))
                .unwrap_or_default(),
            rule.source.as_str()
        ));
    }
    lines.push("provenance:".to_string());
    for entry in &snapshot.provenance {
        lines.push(format!(
            "  {} = {} ({})",
            entry.field,
            entry.value,
            entry.source.as_str()
        ));
    }
    if !snapshot.warnings.is_empty() {
        lines.push("warnings:".to_string());
        for warning in &snapshot.warnings {
            lines.push(format!("  - {warning}"));
        }
    }
    lines
}

fn explain_intent(
    composition: &Composition,
    intent_path: &std::path::Path,
    mode: ModeKind,
) -> Result<i32, CliError> {
    let json = std::fs::read_to_string(intent_path).map_err(|err| {
        CliError::usage(format!("cannot read intent {}: {err}", intent_path.display()))
    })?;
    let value: serde_json::Value = serde_json::from_str(&json)
        .map_err(|err| CliError::usage(format!("invalid intent JSON: {err}")))?;
    let tool = value
        .get("tool")
        .and_then(|value| value.as_str())
        .ok_or_else(|| CliError::usage("intent needs a \"tool\" string"))?;
    let arguments = value
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));

    let prepared = bollo_tools::prepare(
        tool,
        &arguments,
        &composition.fs,
        &composition.root.policy_root(),
    )
    .map_err(|err| CliError::usage(format!("intent does not normalize: {err}")))?;

    let decision = evaluate(&prepared.intent, &composition.snapshot);
    let descriptor = effective(mode, &composition.snapshot);
    let constrained = constrain(
        &descriptor,
        prepared.class,
        prepared.intent.effect,
        decision.effect,
    );
    println!("tool           {}", prepared.tool_name);
    println!("effect_class   {}", format!("{:?}", prepared.intent.effect).to_lowercase());
    println!("policy_effect  {}", decision.effect.as_str());
    println!(
        "rule           {}",
        decision.rule_id.clone().unwrap_or_else(|| "-".into())
    );
    println!("reason         {}", decision.reason);
    println!(
        "mode           {} → {}",
        mode.as_str(),
        constrained.effect.as_str()
    );
    if let Some(reason) = &constrained.reason {
        println!("mode_reason    {reason}");
    }
    if constrained.mode_denied {
        println!("mode_denied    true");
    }
    println!("intent_hash    {}", bollo_policy::intent_hash(&prepared.intent));
    println!("note           evaluation only; nothing was executed and no approval was created");
    Ok(0)
}

// --- config / doctor / mcp ---------------------------------------------------

fn cmd_config(cli: &Cli, args: &ConfigArgs) -> Result<i32, CliError> {
    match &args.command {
        ConfigCommand::Validate => {
            let composition = Composition::build(cli)?;
            let user_path = composition
                .user_config_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "<defaults>".into());
            println!("user_config    {user_path} (valid)");
            println!(
                "project_config {} ({})",
                composition
                    .project_config_path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "absent".into()),
                if composition.project_config_path.is_some() {
                    "valid, restrictions only"
                } else {
                    "not present"
                }
            );
            validate_startup(&composition.snapshot)
                .map_err(|err| CliError::usage(format!("startup refused: {err}")))?;
            println!("snapshot       revision {} (valid)", composition.snapshot.revision);
            Ok(0)
        }
    }
}

fn cmd_doctor(cli: &Cli) -> Result<i32, CliError> {
    let composition = Composition::build(cli)?;
    let snapshot = &composition.snapshot;
    println!("bollo          {}", env!("CARGO_PKG_VERSION"));
    println!(
        "platform       {} ({})",
        format!("{:?}", snapshot.capabilities.platform).to_lowercase(),
        snapshot.capabilities.backend
    );
    println!(
        "sandbox        filesystem_containment={} network_denied={} workspace_auto={}",
        snapshot.capabilities.filesystem_containment,
        snapshot.capabilities.network_denied,
        snapshot.capabilities.supports_workspace_auto()
    );
    println!("live_http      {}", cfg!(feature = "live-http"));
    println!("workspace      {}", composition.root.policy_root());
    println!("workspace_id   {}", composition.root.identity_hash());
    println!("state_dir      {}", composition.state_dir.display());
    println!(
        "store          schema_version={} sessions={}",
        composition.store.schema_version().unwrap_or(0),
        composition
            .store
            .list_sessions()
            .map(|sessions| sessions.len())
            .unwrap_or(0)
    );
    println!(
        "user_config    {}",
        composition
            .user_config_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "<defaults>".into())
    );
    println!(
        "project_config {}",
        composition
            .project_config_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "absent".into())
    );
    println!(
        "provider       {} model={} credential_env={} present={}",
        composition.provider_label(),
        if composition.model.is_empty() {
            "<unset>"
        } else {
            composition.model.as_str()
        },
        composition.credential_env(),
        composition.credential_present()
    );
    println!(
        "replay_script  {}",
        composition.provider_script_label().unwrap_or_else(|| "-".into())
    );
    println!(
        "profile        {} sandbox={} limits(tool_calls={}, run_seconds={}, spend_cents={})",
        snapshot.profile.as_str(),
        match snapshot.sandbox {
            SandboxMode::Workspace => "workspace",
            SandboxMode::Off => "off",
        },
        snapshot.limits.max_tool_calls,
        snapshot.limits.max_run_seconds,
        snapshot
            .limits
            .max_spend_cents
            .map(|cents| cents.to_string())
            .unwrap_or_else(|| "unbounded".into())
    );
    println!(
        "recovery       stale_operations_marked_unknown={}",
        composition.recovered_operations
    );
    let hooks = &composition.user_config.hooks;
    if hooks.is_empty() {
        println!("hooks          none configured");
    } else {
        for hook in hooks {
            let spec = composition
                .hooks
                .iter()
                .find(|spec| spec.id == hook.id)
                .expect("hook specs are built from the user config");
            println!(
                "hook           {} event={:?} enabled={} trusted={}",
                hook.id,
                hook.event,
                hook.enabled,
                composition.trust.verify(
                    &spec.argv,
                    composition.root.canonical(),
                    &spec.env_names
                )
            );
        }
    }
    let servers = &composition.user_config.mcp_servers;
    if servers.is_empty() {
        println!("mcp            no servers configured");
    } else {
        for server in servers {
            let mut argv = vec![server.command.clone()];
            argv.extend(server.args.clone());
            println!(
                "mcp            {} enabled={} trusted={} argv={}",
                server.id,
                server.enabled,
                composition
                    .trust
                    .verify(&argv, composition.root.canonical(), &server.env_allowlist),
                argv.join(" ")
            );
        }
    }
    for warning in &composition.warnings {
        println!("warning        {warning}");
    }
    println!("note           diagnostics read metadata only; no project code was executed");
    Ok(0)
}

fn cmd_mcp(cli: &Cli, args: &McpArgs) -> Result<i32, CliError> {
    match &args.command {
        McpCommand::List => {
            let composition = Composition::build(cli)?;
            if composition.user_config.mcp_servers.is_empty() {
                println!("no MCP servers configured");
                return Ok(0);
            }
            for server in &composition.user_config.mcp_servers {
                let mut argv = vec![server.command.clone()];
                argv.extend(server.args.clone());
                println!(
                    "{}\tenabled={}\ttrusted={}\tcommand={}\targs={}\tenv_allowlist={}",
                    server.id,
                    server.enabled,
                    composition
                        .trust
                        .verify(&argv, composition.root.canonical(), &server.env_allowlist),
                    server.command,
                    server.args.len(),
                    server.env_allowlist.join(",")
                );
            }
            println!("note: enabled is not trusted, and no server is started by this command");
            Ok(0)
        }
    }
}

// --- checkpoints -------------------------------------------------------------

fn cmd_checkpoint(cli: &Cli, args: &CheckpointArgs) -> Result<i32, CliError> {
    let mut composition = Composition::build(cli)?;
    match &args.command {
        CheckpointCommand::List { session } => {
            let session = match session {
                Some(id) => parse_session(id)?,
                None => composition
                    .store
                    .list_sessions()
                    .map_err(|err| CliError::recovery(format!("cannot list sessions: {err}")))?
                    .first()
                    .map(|record| record.id.clone())
                    .ok_or_else(|| {
                        CliError::recovery("no local sessions; nothing to list checkpoints for")
                    })?,
            };
            let checkpoints = composition
                .store
                .checkpoints_for_session(&session)
                .map_err(|err| CliError::recovery(format!("cannot read checkpoints: {err}")))?;
            if checkpoints.is_empty() {
                println!("no checkpoints recorded for session {session}");
                return Ok(0);
            }
            for checkpoint in checkpoints {
                println!(
                    "{}\t{}\texisted={}\tpost={}\trestored={}",
                    checkpoint.id,
                    checkpoint.path,
                    checkpoint.existed,
                    short_hash(checkpoint.post_sha256.as_deref()),
                    checkpoint.restored_at.unwrap_or_else(|| "-".into())
                );
            }
            Ok(0)
        }
        CheckpointCommand::Preview { id } => {
            let record = load_checkpoint(&composition.store, id)?;
            let checkpoint = checkpoint_from_record(&record)?;
            let mut log = CheckpointLog::new();
            log.insert(checkpoint);
            let current = composition.fs.current_sha256(&record.path);
            println!("checkpoint     {}", record.id);
            println!("path           {}", record.path);
            println!("preimage       {}", short_hash(record.pre_sha256.as_deref()));
            println!("postimage      {}", short_hash(record.post_sha256.as_deref()));
            println!("current        {}", short_hash(current.as_deref()));
            match log.preview(&CheckpointId::parse(record.id.clone()).map_err(to_usage)?, current.as_deref()) {
                Ok(RestorePlan::WritePreimage { path, text }) => {
                    println!("plan           write preimage to {path} ({} bytes)", text.len());
                }
                Ok(RestorePlan::DeleteCreated { path }) => {
                    println!("plan           delete {path} (created by Bollo, still identical)");
                }
                Err(err) => println!("plan           refused: {err}"),
            }
            Ok(0)
        }
        CheckpointCommand::Restore { id, yes } => {
            if !yes {
                return Err(CliError::usage(
                    "refusing to restore without --yes (run `checkpoint preview` first)",
                ));
            }
            let record = load_checkpoint(&composition.store, id)?;
            let checkpoint = checkpoint_from_record(&record)?;
            let post = record.post_sha256.clone().ok_or_else(|| {
                CliError::recovery(format!(
                    "checkpoint {id} has no recorded postimage; cannot prove the file is ours"
                ))
            })?;
            let checkpoint_id = CheckpointId::parse(record.id.clone()).map_err(to_usage)?;
            let mut log = CheckpointLog::new();
            log.insert(checkpoint);
            let current = composition.fs.current_sha256(&record.path);
            match log.preview(&checkpoint_id, current.as_deref()) {
                Ok(RestorePlan::WritePreimage { path, text }) => {
                    composition
                        .fs
                        .write_text(&path, &text, Some(&post))
                        .map_err(|err| CliError::recovery(format!("restore refused: {err}")))?;
                    eprintln!("bollo: restored preimage of {path}");
                }
                Ok(RestorePlan::DeleteCreated { path }) => {
                    composition
                        .fs
                        .remove_file(&path, &post)
                        .map_err(|err| CliError::recovery(format!("restore refused: {err}")))?;
                    eprintln!("bollo: deleted {path} (created by Bollo and still identical)");
                }
                Err(err) => {
                    return Err(CliError::recovery(format!(
                        "conditional restore refused for {}: {err}",
                        record.path
                    )));
                }
            }
            composition
                .store
                .mark_checkpoint_restored(&record.id)
                .map_err(|err| CliError::recovery(format!("cannot record the restore: {err}")))?;
            Ok(0)
        }
    }
}

fn to_usage(err: impl std::fmt::Display) -> CliError {
    CliError::usage(err.to_string())
}

fn short_hash(hash: Option<&str>) -> String {
    match hash {
        Some(value) if value.len() >= 12 => format!("{}…", &value[..12]),
        Some(value) => value.to_string(),
        None => "-".into(),
    }
}

fn load_checkpoint(store: &SqliteStore, id: &str) -> Result<CheckpointRecord, CliError> {
    store
        .checkpoint(id)
        .map_err(|_| CliError::recovery(format!("no checkpoint {id} in this state directory")))
}

fn checkpoint_from_record(record: &CheckpointRecord) -> Result<Checkpoint, CliError> {
    Ok(Checkpoint {
        id: CheckpointId::parse(record.id.clone()).map_err(to_usage)?,
        path: record.path.clone(),
        existed: record.existed,
        pre_sha256: record.pre_sha256.clone(),
        post_sha256: record.post_sha256.clone(),
        pre_text: record.pre_text.clone(),
        captured_at: record.captured_at.clone(),
    })
}

// --- interactive -------------------------------------------------------------

fn cmd_interactive(cli: &Cli) -> Result<i32, CliError> {
    if !std::io::stdin().is_terminal() {
        return Err(CliError::usage(
            "interactive mode needs a terminal; use `bollo run --prompt TEXT` for headless mode",
        ));
    }
    let mut composition = Composition::build(cli)?;
    composition.require_risk_acknowledgement()?;
    note_warnings(&composition);
    let session = composition.new_session(Some("interactive"))?;
    let cancel = CancellationToken::new();
    crate::install_cancel_handler(cancel.clone());

    let presenter = Presenter::from_env(!cli.no_color);
    let terminal = Terminal::new(
        Box::new(std::io::stdin().lock()),
        Box::new(std::io::stdout()),
        presenter,
    );
    let mut tui = TuiSession::new(terminal, 2_000);
    let settings = TurnSettings {
        session,
        mode: work_mode(cli),
        include_claude_md: false,
    };
    let composition = RefCell::new(composition);
    let mut runner = InteractiveRunner {
        composition: &composition,
        settings: settings.clone(),
        cancel,
    };
    let mut commands = CompositionCommands {
        composition: &composition,
        settings,
    };
    let code = tui.run(&mut runner, &mut commands, None);
    Ok(code)
}

/// Slash commands that need composition-wide state. Reads only: nothing here
/// can widen policy or approve an ask.
struct CompositionCommands<'a> {
    composition: &'a RefCell<Composition>,
    settings: TurnSettings,
}

impl SlashCommands for CompositionCommands<'_> {
    fn handle(&mut self, name: &str, argument: &str) -> Option<Vec<String>> {
        match name {
            "help" => Some(vec![
                "  /diff /permissions /sessions /mcp /doctor /model /checkpoint list".to_string(),
            ]),
            "diff" => Some(self.diff_lines()),
            "permissions" | "policy" => Some(policy_lines(&self.composition.borrow())),
            "sessions" => Some(self.session_lines()),
            "mcp" => Some(self.mcp_lines()),
            "doctor" => Some(self.doctor_lines()),
            "model" => Some(self.model_lines()),
            "checkpoint" => Some(self.checkpoint_lines(argument)),
            "compact" => Some(vec![
                "compaction is automatic at 80% of the provider context capacity; complete tool \
                 pairs and pending approvals are preserved"
                    .to_string(),
            ]),
            _ => None,
        }
    }
}

impl CompositionCommands<'_> {
    fn diff_lines(&self) -> Vec<String> {
        let composition = self.composition.borrow();
        let events = composition
            .store
            .replay(&self.settings.session, 0)
            .unwrap_or_default();
        let presenter = Presenter::default();
        let mut recent: Vec<&EventEnvelope> = events
            .iter()
            .filter(|event| {
                matches!(
                    event.event_type,
                    EventType::ToolResult | EventType::VerificationResult | EventType::ToolProposed
                )
            })
            .collect();
        recent.reverse();
        recent.truncate(20);
        let mut lines = Vec::new();
        for event in recent {
            if let Some(line) = presenter.render(event) {
                lines.push(line);
            }
        }
        if lines.is_empty() {
            lines.push("no tool activity in this session yet".into());
        }
        lines
    }

    fn session_lines(&self) -> Vec<String> {
        let composition = self.composition.borrow();
        composition
            .store
            .list_sessions()
            .unwrap_or_default()
            .iter()
            .map(|session| {
                format!(
                    "{}  seq={}  {}",
                    session.id.as_str(),
                    session.last_seq,
                    session.label.clone().unwrap_or_else(|| "-".into())
                )
            })
            .collect()
    }

    fn mcp_lines(&self) -> Vec<String> {
        let composition = self.composition.borrow();
        if composition.user_config.mcp_servers.is_empty() {
            return vec!["no MCP servers configured".into()];
        }
        composition
            .user_config
            .mcp_servers
            .iter()
            .map(|server| {
                let mut argv = vec![server.command.clone()];
                argv.extend(server.args.clone());
                format!(
                    "{} enabled={} trusted={}",
                    server.id,
                    server.enabled,
                    composition
                        .trust
                        .verify(&argv, composition.root.canonical(), &server.env_allowlist)
                )
            })
            .collect()
    }

    fn doctor_lines(&self) -> Vec<String> {
        let composition = self.composition.borrow();
        vec![
            format!(
                "platform {} · sandbox containment={} network_denied={}",
                format!("{:?}", composition.snapshot.capabilities.platform).to_lowercase(),
                composition.snapshot.capabilities.filesystem_containment,
                composition.snapshot.capabilities.network_denied
            ),
            format!("state_dir {}", composition.state_dir.display()),
            format!(
                "provider {} model={} credential={} present={}",
                composition.provider_label(),
                if composition.model.is_empty() {
                    "<unset>"
                } else {
                    composition.model.as_str()
                },
                composition.credential_env(),
                composition.credential_present()
            ),
            format!(
                "recovery stale_operations_marked_unknown={}",
                composition.recovered_operations
            ),
        ]
    }

    fn model_lines(&self) -> Vec<String> {
        let composition = self.composition.borrow();
        vec![
            format!("provider {}", composition.provider_label()),
            format!(
                "model {}",
                if composition.model.is_empty() {
                    "<unset>"
                } else {
                    composition.model.as_str()
                }
            ),
            format!("profile {}", composition.snapshot.profile.as_str()),
            format!(
                "mode {}",
                self.settings.mode.as_str()
            ),
        ]
    }

    fn checkpoint_lines(&self, argument: &str) -> Vec<String> {
        let composition = self.composition.borrow();
        if !argument.is_empty() && argument != "list" {
            return vec![format!("unknown checkpoint argument {argument:?}; try /checkpoint list")];
        }
        let checkpoints = composition
            .store
            .checkpoints_for_session(&self.settings.session)
            .unwrap_or_default();
        if checkpoints.is_empty() {
            return vec!["no checkpoints recorded for this session".into()];
        }
        checkpoints
            .iter()
            .map(|checkpoint| {
                format!(
                    "{} {} existed={} restored={}",
                    checkpoint.id,
                    checkpoint.path,
                    checkpoint.existed,
                    checkpoint.restored_at.as_deref().unwrap_or("-")
                )
            })
            .collect()
    }
}


