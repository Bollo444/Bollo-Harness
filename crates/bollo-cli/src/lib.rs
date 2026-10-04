//! `bollo` — the headless and interactive client for the hybrid harness.
//!
//! Exit codes are the documented contract (`docs/reference/cli.md`):
//!
//! | Exit | Meaning |
//! |---|---|
//! | 0 | Run completed normally; inspect `verification` separately |
//! | 1 | Runtime/provider/tool-loop fatal failure |
//! | 2 | Invalid arguments/config, missing credential, unsupported capability |
//! | 3 | Blocked for required approval |
//! | 4 | Budget or run limit reached |
//! | 5 | Recovery required / unknown effects |
//! | 130 | User cancellation |
//!
//! Completion is never reported as "tests passed": verification is a separate
//! field on the run outcome and in the event stream.

pub mod args;
pub mod commands;
pub mod composition;
pub mod render;

use std::ffi::OsString;
use std::sync::{Mutex, Once, OnceLock};

use clap::Parser;

use bollo_protocol::cancel::CancellationToken;

pub use args::Cli;

/// A failure that maps onto one documented exit code.
#[derive(Debug)]
pub struct CliError {
    pub code: i32,
    pub message: String,
}

impl CliError {
    /// Invalid arguments, configuration or credentials (exit 2).
    pub fn usage(message: impl Into<String>) -> Self {
        Self {
            code: 2,
            message: message.into(),
        }
    }

    /// Fatal runtime/provider/tool-loop failure (exit 1).
    pub fn runtime(message: impl Into<String>) -> Self {
        Self {
            code: 1,
            message: message.into(),
        }
    }

    /// Recovery required: unknown effects, unreadable store (exit 5).
    pub fn recovery(message: impl Into<String>) -> Self {
        Self {
            code: 5,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for CliError {}

/// Parse and run. Returns the process exit code.
pub fn run_from<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(err) => {
            let code = err.exit_code();
            let _ = err.print();
            return code;
        }
    };
    match commands::dispatch(cli) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("bollo: {}", err.message);
            err.code
        }
    }
}

static CURRENT_CANCEL: OnceLock<Mutex<Option<CancellationToken>>> = OnceLock::new();
static CTRL_C_INSTALLED: Once = Once::new();

/// Register the current run's cancellation token and install the SIGINT handler
/// once per process. Ctrl-C maps to exit 130 after best-effort cleanup.
pub fn install_cancel_handler(token: CancellationToken) {
    let slot = CURRENT_CANCEL.get_or_init(|| Mutex::new(None));
    *slot.lock().expect("cancel slot") = Some(token);
    CTRL_C_INSTALLED.call_once(|| {
        let _ = ctrlc::set_handler(|| {
            if let Some(slot) = CURRENT_CANCEL.get() {
                if let Some(token) = slot.lock().expect("cancel slot").clone() {
                    token.cancel();
                }
            }
        });
    });
}

/// True when Ctrl-C was observed for the most recent run.
pub fn cancellation_requested() -> bool {
    CURRENT_CANCEL
        .get()
        .and_then(|slot| slot.lock().ok().and_then(|guard| guard.clone()))
        .map(|token| token.is_cancelled())
        .unwrap_or(false)
}
