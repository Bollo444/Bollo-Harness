//! Interactive client over the runtime's event stream.
//!
//! The client renders the same durable events the headless renderer emits, so
//! there is exactly one event model and no second state machine: it may drop
//! text frames under pressure, but never a permission decision or a terminal
//! event. Approvals are keyboard-driven, and "not answered" is never "yes".

pub mod present;
pub mod slash;
pub mod terminal;

use std::cell::RefCell;

use bollo_protocol::events::EventEnvelope;

pub use present::{LineClass, Presenter, Transcript};
pub use slash::{help, is_client_command, parse, SlashCommands};
pub use terminal::{ApprovalPrompt, Approver, ScriptedApprover, Terminal, TerminalApprover};

/// The client's view of one turn's result. The runtime's own outcome type is
/// translated by the composition root, so this crate stays UI-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnOutcome {
    pub exit_code: i32,
    pub summary: String,
}

/// Port the runtime loop is driven through.
pub trait TurnRunner {
    fn run_turn(
        &mut self,
        prompt: &str,
        sink: &mut dyn FnMut(&EventEnvelope),
        approvals: &mut dyn Approver,
    ) -> TurnOutcome;
}

/// Commands the composition root advertises in `/help`.
pub const COMPOSITION_COMMANDS: &[&str] = &[
    "/diff                recent tool results and verification status",
    "/permissions         effective profile, sandbox and rules",
    "/sessions            local sessions in this store",
    "/mcp                 configured MCP servers and trust",
    "/doctor              capability and credential diagnostics",
    "/model               current provider/model binding",
    "/checkpoint list     patch checkpoints recorded for this session",
];

/// One interactive client session: terminal state, transcript and the run loop.
pub struct TuiSession {
    terminal: RefCell<Terminal>,
    transcript: Transcript,
    presenter: Presenter,
    last_exit: i32,
}

impl TuiSession {
    pub fn new(terminal: Terminal, transcript_capacity: usize) -> Self {
        let presenter = terminal.presenter().clone();
        Self {
            terminal: RefCell::new(terminal),
            transcript: Transcript::new(transcript_capacity),
            presenter,
            last_exit: 0,
        }
    }

    pub fn presenter(&self) -> &Presenter {
        &self.presenter
    }

    pub fn transcript(&self) -> &Transcript {
        &self.transcript
    }

    pub fn last_exit(&self) -> i32 {
        self.last_exit
    }

    /// Run one turn through the injected runner, streaming events to the
    /// terminal and asking the human for approval when the runtime needs it.
    pub fn run_turn(&mut self, runner: &mut dyn TurnRunner, prompt: &str) -> TurnOutcome {
        let terminal = &self.terminal;
        let transcript = &self.transcript;
        let mut approver = TerminalApprover::new(terminal, transcript);
        let mut sink = |event: &EventEnvelope| {
            terminal.borrow_mut().write_event(event, transcript);
        };
        let outcome = runner.run_turn(prompt, &mut sink, &mut approver);
        self.last_exit = outcome.exit_code;
        outcome
    }

    /// The interactive loop. Returns the process exit code: 0 for a clean
    /// `/quit` or end of input, otherwise the last turn's code.
    pub fn run(
        &mut self,
        runner: &mut dyn TurnRunner,
        commands: &mut dyn SlashCommands,
        initial_prompt: Option<String>,
    ) -> i32 {
        if let Some(prompt) = initial_prompt {
            let outcome = self.run_turn(runner, &prompt);
            self.report(&outcome.summary);
        }
        loop {
            let Some(line) = self.terminal.borrow_mut().prompt("bollo> ") else {
                break;
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Some((name, argument)) = parse(trimmed) else {
                let outcome = self.run_turn(runner, trimmed);
                self.report(&outcome.summary);
                continue;
            };
            match name {
                "quit" | "exit" => return 0,
                "help" => {
                    let mut lines = help(COMPOSITION_COMMANDS);
                    lines.extend(commands.handle("help", "").unwrap_or_default());
                    self.print_lines(&lines);
                }
                "transcript" => {
                    let text = self.transcript.joined();
                    self.print_lines(&[text]);
                }
                other => match commands.handle(other, argument) {
                    Some(lines) => self.print_lines(&lines),
                    None => {
                        let message = format!("unknown command /{other}; try /help");
                        self.print_lines(&[message]);
                    }
                },
            }
        }
        if self.last_exit != 0 {
            self.last_exit
        } else {
            0
        }
    }

    fn report(&mut self, summary: &str) {
        if summary.is_empty() {
            return;
        }
        let text = format!("turn finished: {summary}");
        let mut terminal = self.terminal.borrow_mut();
        terminal.write_note(&text, &self.transcript, LineClass::Decision);
    }

    fn print_lines(&mut self, lines: &[String]) {
        let mut terminal = self.terminal.borrow_mut();
        for line in lines {
            terminal.write_note(line, &self.transcript, LineClass::Event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollo_protocol::ids::{EventId, SessionId};
    use std::io::Cursor;
    use std::sync::{Arc, Mutex};

    struct RecordingRunner {
        prompts: Arc<Mutex<Vec<String>>>,
        outcomes: Vec<TurnOutcome>,
        emit_finished: bool,
    }

    impl RecordingRunner {
        fn new(outcomes: Vec<TurnOutcome>) -> Self {
            Self {
                prompts: Arc::new(Mutex::new(Vec::new())),
                outcomes,
                emit_finished: true,
            }
        }
    }

    impl TurnRunner for RecordingRunner {
        fn run_turn(
            &mut self,
            prompt: &str,
            sink: &mut dyn FnMut(&EventEnvelope),
            _approvals: &mut dyn Approver,
        ) -> TurnOutcome {
            self.prompts.lock().unwrap().push(prompt.to_string());
            if self.emit_finished {
                sink(&EventEnvelope {
                    schema_version: "0.1".into(),
                    event_id: EventId::generate(),
                    session_id: SessionId::generate(),
                    run_id: None,
                    seq: 1,
                    timestamp: "2026-10-03T00:00:00Z".into(),
                    event_type: bollo_protocol::EventType::RunFinished,
                    data: serde_json::json!({
                        "state": "completed",
                        "reason": null,
                        "verification": "skipped"
                    }),
                });
            }
            self.outcomes.remove(0)
        }
    }

    #[derive(Default)]
    struct Commands {
        handled: Vec<String>,
    }

    impl SlashCommands for Commands {
        fn handle(&mut self, name: &str, argument: &str) -> Option<Vec<String>> {
            self.handled.push(format!("{name} {argument}").trim().to_string());
            if name == "policy" {
                Some(vec!["profile balanced".into()])
            } else {
                None
            }
        }
    }

    fn session(input: &str) -> TuiSession {
        TuiSession::new(
            Terminal::new(
                Box::new(Cursor::new(input.to_string())),
                Box::new(Vec::new()),
                Presenter::new(80, false),
            ),
            128,
        )
    }

    #[test]
    fn loop_runs_turns_and_exits_cleanly() {
        let mut tui = session("explain the repo\n/quit\n");
        let mut runner = RecordingRunner::new(vec![TurnOutcome {
            exit_code: 0,
            summary: "completed".into(),
        }]);
        let prompts = runner.prompts.clone();
        let mut commands = Commands::default();
        let code = tui.run(&mut runner, &mut commands, None);
        assert_eq!(code, 0);
        assert_eq!(prompts.lock().unwrap().as_slice(), ["explain the repo"]);
        assert!(tui.transcript().joined().contains("turn finished: completed"));
    }

    #[test]
    fn delegated_and_unknown_commands_are_reported() {
        let mut tui = session("/policy show\n/nope\n/quit\n");
        let mut runner = RecordingRunner::new(Vec::new());
        let mut commands = Commands::default();
        let code = tui.run(&mut runner, &mut commands, None);
        assert_eq!(code, 0);
        assert_eq!(commands.handled, vec!["policy show".to_string(), "nope".to_string()]);
        let transcript = tui.transcript().joined();
        assert!(transcript.contains("profile balanced"));
        assert!(transcript.contains("unknown command /nope"));
    }

    #[test]
    fn end_of_input_returns_the_last_turn_code() {
        let mut tui = session("do the thing\n");
        let mut runner = RecordingRunner::new(vec![TurnOutcome {
            exit_code: 3,
            summary: "blocked: approval_required".into(),
        }]);
        let mut commands = Commands::default();
        assert_eq!(tui.run(&mut runner, &mut commands, None), 3);
    }

    #[test]
    fn initial_prompt_runs_before_the_loop() {
        let mut tui = session("/quit\n");
        let mut runner = RecordingRunner::new(vec![TurnOutcome {
            exit_code: 0,
            summary: "completed".into(),
        }]);
        let prompts = runner.prompts.clone();
        let mut commands = Commands::default();
        assert_eq!(tui.run(&mut runner, &mut commands, Some("one shot".into())), 0);
        assert_eq!(prompts.lock().unwrap().as_slice(), ["one shot"]);
    }
}
