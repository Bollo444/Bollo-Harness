//! Command-line surface (`docs/reference/cli.md`).
//!
//! No `--api-key` flag exists: credentials are environment references resolved
//! from the trusted user configuration only. Headless runs accept `--prompt` or
//! `--prompt-file` (mutually exclusive) and choose `--output text|ndjson`.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

use bollo_protocol::vocab::{ModeKind, Profile, SandboxMode};

#[derive(Debug, Parser)]
#[command(
    name = "bollo",
    version,
    about = "Bollo hybrid CLI harness (MVP): bounded agent loop, explicit policy, durable journal"
)]
pub struct Cli {
    /// Workspace root; discovery reads metadata only and never executes project code
    #[arg(long, global = true, value_name = "PATH")]
    pub workspace: Option<PathBuf>,

    /// Permission profile
    #[arg(long, global = true, value_enum)]
    pub profile: Option<ProfileArg>,

    /// Execution isolation selection (independent of the profile)
    #[arg(long, global = true, value_enum)]
    pub sandbox: Option<SandboxArg>,

    /// Provider binding for this session
    #[arg(long, global = true, value_enum)]
    pub provider: Option<ProviderArg>,

    /// Model id for this session
    #[arg(long, global = true, value_name = "ID")]
    pub model: Option<String>,

    /// Operational mode (modes constrain, never widen)
    #[arg(long, global = true, value_enum)]
    pub mode: Option<ModeArg>,

    /// Tool-call ceiling for this session
    #[arg(long, global = true, value_name = "N")]
    pub max_tool_calls: Option<u32>,

    /// Wall-clock ceiling for this session
    #[arg(long, global = true, value_name = "N")]
    pub max_run_seconds: Option<u64>,

    /// Spend cap for this session
    #[arg(long, global = true, value_name = "N")]
    pub max_spend_cents: Option<u64>,

    /// Acknowledge the risk of unrestricted/host-mode execution explicitly
    #[arg(long, global = true)]
    pub acknowledge_risk: bool,

    /// Disable the color path
    #[arg(long, global = true)]
    pub no_color: bool,

    /// State directory override (defaults to the per-user state directory)
    #[arg(long, global = true, value_name = "DIR")]
    pub state_dir: Option<PathBuf>,

    /// Deterministic provider script (test driver; not a network provider)
    #[arg(long, global = true, value_name = "FILE")]
    pub script: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Headless run: one request, then exit (stdout carries only the output format)
    Run(RunArgs),

    /// Continue a persisted session with a new run after recovery checks
    Resume(ResumeArgs),

    /// Local session metadata; never touches source files
    Sessions(SessionsArgs),

    /// Durable run records: usage and classifier audit without the API
    Runs(RunsArgs),

    /// Effective values/rules and their origins
    Policy(PolicyArgs),

    /// Configuration syntax and semantic checks
    Config(ConfigArgs),

    /// Capabilities, credential presence, endpoint/config diagnostics
    Doctor,

    /// Configured MCP servers, enablement and trust status
    Mcp(McpArgs),

    /// Patch checkpoints: preimage/postimage preview and conditional restore
    Checkpoint(CheckpointArgs),
}

#[derive(Debug, Args)]
pub struct RunArgs {
    #[arg(long, value_name = "TEXT", conflicts_with = "prompt_file")]
    pub prompt: Option<String>,

    #[arg(long = "prompt-file", value_name = "PATH")]
    pub prompt_file: Option<PathBuf>,

    #[arg(long, value_enum, default_value_t = OutputArg::Text)]
    pub output: OutputArg,

    /// Include opt-in `CLAUDE.md` compatibility instructions
    #[arg(long)]
    pub include_claude_md: bool,

    /// Resume the most recent interrupted run's session instead of starting a new one
    #[arg(long)]
    pub resume_last: bool,
}

#[derive(Debug, Args)]
pub struct ResumeArgs {
    /// Session id to resume (see `bollo sessions list`)
    #[arg(value_name = "SESSION_ID")]
    pub session_id: String,

    #[arg(long, value_name = "TEXT", conflicts_with = "prompt_file")]
    pub prompt: Option<String>,

    #[arg(long = "prompt-file", value_name = "PATH")]
    pub prompt_file: Option<PathBuf>,

    #[arg(long, value_enum, default_value_t = OutputArg::Text)]
    pub output: OutputArg,

    /// Required when unknown effects exist: acknowledge them (they are never replayed)
    #[arg(long)]
    pub acknowledge_unknown: bool,
}

#[derive(Debug, Args)]
pub struct SessionsArgs {
    #[command(subcommand)]
    pub command: SessionsCommand,
}

#[derive(Debug, Subcommand)]
pub enum SessionsCommand {
    /// List local sessions in this state directory
    List,
    /// Export replayed events as NDJSON into the chosen path
    Export {
        #[arg(value_name = "SESSION_ID")]
        id: String,
        #[arg(long, value_name = "PATH")]
        output: PathBuf,
    },
    /// Delete local session state (never source files)
    Delete {
        #[arg(value_name = "SESSION_ID")]
        id: String,
        /// Required confirmation for a destructive local operation
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Args)]
pub struct RunsArgs {
    #[command(subcommand)]
    pub command: RunsCommand,
}

#[derive(Debug, Subcommand)]
pub enum RunsCommand {
    /// List durable run records, newest first (read-only; no API required)
    List {
        /// Restrict the listing to one session (see `bollo sessions list`)
        #[arg(long, value_name = "SESSION_ID")]
        session: Option<String>,
    },
}

#[derive(Debug, Args)]
pub struct PolicyArgs {
    #[command(subcommand)]
    pub command: PolicyCommand,
}

#[derive(Debug, Subcommand)]
pub enum PolicyCommand {
    /// Effective profile, sandbox, limits and rules with provenance
    Show,
    /// Evaluate an example intent (`{"tool":..,"arguments":{..}}`) without executing
    Explain {
        #[arg(long, value_name = "FILE")]
        intent: PathBuf,
    },
}

#[derive(Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Parse and validate the trusted user and project configuration
    Validate,
}

#[derive(Debug, Args)]
pub struct McpArgs {
    #[command(subcommand)]
    pub command: McpCommand,
}

#[derive(Debug, Subcommand)]
pub enum McpCommand {
    /// Configured servers; starting any server requires explicit trust
    List,
}

#[derive(Debug, Args)]
pub struct CheckpointArgs {
    #[command(subcommand)]
    pub command: CheckpointCommand,
}

#[derive(Debug, Subcommand)]
pub enum CheckpointCommand {
    /// Recorded checkpoints for a session (defaults to the most recent session)
    List {
        #[arg(long, value_name = "SESSION_ID")]
        session: Option<String>,
    },
    /// Compare preimage/postimage/current without changing anything
    Preview {
        #[arg(value_name = "CHECKPOINT_ID")]
        id: String,
    },
    /// Conditional restore: only hash-matching files are touched
    Restore {
        #[arg(value_name = "CHECKPOINT_ID")]
        id: String,
        /// Required confirmation for a mutating local operation
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputArg {
    Text,
    Ndjson,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ProfileArg {
    ReadOnly,
    Balanced,
    WorkspaceAuto,
    Unrestricted,
}

impl From<ProfileArg> for Profile {
    fn from(value: ProfileArg) -> Self {
        match value {
            ProfileArg::ReadOnly => Profile::ReadOnly,
            ProfileArg::Balanced => Profile::Balanced,
            ProfileArg::WorkspaceAuto => Profile::WorkspaceAuto,
            ProfileArg::Unrestricted => Profile::Unrestricted,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SandboxArg {
    Workspace,
    Off,
}

impl From<SandboxArg> for SandboxMode {
    fn from(value: SandboxArg) -> Self {
        match value {
            SandboxArg::Workspace => SandboxMode::Workspace,
            SandboxArg::Off => SandboxMode::Off,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ProviderArg {
    Anthropic,
    Xai,
    /// Deterministic script-driven provider used by tests and CI
    Replay,
}

impl ProviderArg {
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderArg::Anthropic => "anthropic",
            ProviderArg::Xai => "xai",
            ProviderArg::Replay => "replay",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ModeArg {
    Inspect,
    Plan,
    Build,
    Parallel,
}

impl From<ModeArg> for ModeKind {
    fn from(value: ModeArg) -> Self {
        match value {
            ModeArg::Inspect => ModeKind::Inspect,
            ModeArg::Plan => ModeKind::Plan,
            ModeArg::Build => ModeKind::Build,
            ModeArg::Parallel => ModeKind::Parallel,
        }
    }
}
