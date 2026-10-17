//! Terminal I/O and the interactive approval channel.
//!
//! The client can present an ask, but it can never answer one on the user's
//! behalf: end-of-input, empty input, an unrecognized word or a closed terminal
//! all mean "not approved", which makes the runtime block with
//! `approval_required` instead of proceeding.

use std::cell::RefCell;
use std::io::{BufRead, IsTerminal, Write};

use bollo_protocol::events::EventEnvelope;
use bollo_protocol::EventType;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal as RatatuiTerminal;

use crate::present::{LineClass, Presenter, Transcript};
use crate::screens::{self, Header};

/// The decision request shown to the user. Mirrors the runtime's approval
/// request without making this crate depend on the runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalPrompt {
    pub approval_id: String,
    pub tool_name: String,
    pub summary: String,
    pub intent_hash: String,
    pub policy_revision: u64,
    pub expires_at: String,
}

/// Interactive decision rights. `None` means *not approved*.
pub trait Approver {
    fn decide(&mut self, prompt: &ApprovalPrompt) -> Option<bool>;
}

/// The real-terminal side of the seam (ADR-016): raw mode, alternate screen
/// and every frame drawn through `screens::render` from journaled state.
struct LiveScreen {
    terminal: RatatuiTerminal<CrosstermBackend<std::io::Stdout>>,
    header: Header,
    approval: Option<ApprovalPrompt>,
    approval_expired: bool,
}

impl LiveScreen {
    fn draw(&mut self, transcript: &Transcript, input: &str, color: bool) {
        let state = screens::ScreenState {
            header: self.header.clone(),
            transcript,
            input: input.to_string(),
            approval: self.approval.as_ref(),
            approval_expired: self.approval_expired,
        };
        let _ = self.terminal.draw(|frame| screens::render(frame, &state, color));
    }
}

enum Mode {
    /// Injected streams: the whole client runs deterministically in tests.
    Injected { input: Box<dyn BufRead>, output: Box<dyn Write> },
    /// The real terminal: crossterm raw mode + ratatui frames.
    Live(Box<LiveScreen>),
}

/// Line-oriented seam. Input and output are injected so the whole client can
/// be driven deterministically in tests; `Terminal::open_real` swaps in the
/// ratatui/crossterm path without changing any caller-visible contract.
pub struct Terminal {
    mode: Mode,
    presenter: Presenter,
}

impl Terminal {
    pub fn new(input: Box<dyn BufRead>, output: Box<dyn Write>, presenter: Presenter) -> Self {
        Self {
            mode: Mode::Injected { input, output },
            presenter,
        }
    }

    /// The ratatui/crossterm screen. Enables raw mode and the alternate
    /// screen; [`Drop`] restores both. Refused (fall back to the linear
    /// renderer) when stdout is not a terminal or the window is narrower than
    /// the 50-column floor of the screen renderer.
    pub fn open_real(presenter: Presenter, header: Header) -> std::io::Result<Self> {
        use ratatui::crossterm::terminal::{enable_raw_mode, EnterAlternateScreen};
        if !std::io::stdout().is_terminal() {
            return Err(std::io::Error::other(
                "interactive screen needs a terminal on stdout",
            ));
        }
        let screen_width = ratatui::crossterm::terminal::size()
            .map(|(width, _)| width)
            .unwrap_or(0);
        if !screens::can_render(screen_width) {
            return Err(std::io::Error::other(
                "terminal is narrower than 50 columns; using the linear renderer",
            ));
        }
        enable_raw_mode()?;
        ratatui::crossterm::execute!(std::io::stdout(), EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(std::io::stdout());
        let terminal = RatatuiTerminal::new(backend)?;
        Ok(Self {
            mode: Mode::Live(Box::new(LiveScreen { terminal, header, approval: None, approval_expired: false })),
            presenter,
        })
    }

    pub fn is_live(&self) -> bool {
        matches!(self.mode, Mode::Live(_))
    }

    pub fn presenter(&self) -> &Presenter {
        &self.presenter
    }

    /// Show the approval overlay (live) with the given expiry state.
    fn set_overlay(&mut self, prompt: Option<ApprovalPrompt>, expired: bool) {
        if let Mode::Live(live) = &mut self.mode {
            live.approval = prompt;
            live.approval_expired = expired;
        }
    }

    fn draw(&mut self, transcript: &Transcript, input: &str) {
        if let Mode::Live(live) = &mut self.mode {
            let color = self.presenter.color();
            live.draw(transcript, input, color);
        }
    }

    pub fn write_line(&mut self, line: &str) {
        match &mut self.mode {
            Mode::Injected { output, .. } => {
                let fitted = self.presenter.fit(line);
                let _ = writeln!(output, "{fitted}");
                let _ = output.flush();
            }
            // On the live screen nothing is written raw (the frame owns the
            // terminal); the caller pushes the text into the transcript, from
            // which the next frame renders it.
            Mode::Live(_) => {}
        }
    }

    /// Write one durable event and record it in the transcript. Assistant text
    /// streams without newlines; everything else is a line.
    pub fn write_event(&mut self, event: &EventEnvelope, transcript: &Transcript) {
        let Some(rendered) = self.presenter.render(event) else {
            return;
        };
        let class = Presenter::line_class(event.event_type);
        match &mut self.mode {
            Mode::Injected { output, .. } => {
                if event.event_type == EventType::AssistantDelta {
                    let _ = write!(output, "{rendered}");
                    let _ = output.flush();
                } else {
                    let style = Presenter::style_for(event.event_type);
                    let styled = self.presenter.style(&rendered, style);
                    let _ = writeln!(output, "{styled}");
                    let _ = output.flush();
                }
            }
            // The transcript is the screen's source of truth; the next frame
            // shows the text in its role.
            Mode::Live(_) => {}
        }
        transcript.push(rendered, class);
        self.draw(transcript, "");
    }

    /// Read one line; `None` on end of input.
    pub fn read_line(&mut self) -> Option<String> {
        match &mut self.mode {
            Mode::Injected { input, .. } => {
                let mut buffer = String::new();
                match input.read_line(&mut buffer) {
                    Ok(0) => None,
                    Ok(_) => Some(buffer.trim_end_matches(['\r', '\n']).to_string()),
                    Err(_) => None,
                }
            }
            Mode::Live(_) => None,
        }
    }

    /// Read one prompt line. The transcript is passed for the live renderer,
    /// which redraws the frame from journaled state on every keystroke.
    /// Returns `None` on end of input (injected) or Ctrl-D on an empty line
    /// (live). Ctrl-C clears the live input; it never records a decision.
    pub fn prompt(&mut self, label: &str, transcript: &Transcript) -> Option<String> {
        match &mut self.mode {
            Mode::Injected { output, input } => {
                let _ = write!(output, "{label}");
                let _ = output.flush();
                let mut buffer = String::new();
                match input.read_line(&mut buffer) {
                    Ok(0) => None,
                    Ok(_) => Some(buffer.trim_end_matches(['\r', '\n']).to_string()),
                    Err(_) => None,
                }
            }
            Mode::Live(live) => {
                let mut line = String::new();
                let color = self.presenter.color();
                loop {
                    live.draw(transcript, &line, color);
                    use ratatui::crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
                    if !ratatui::crossterm::event::poll(std::time::Duration::from_millis(200))
                        .unwrap_or(false)
                    {
                        continue;
                    }
                    match ratatui::crossterm::event::read() {
                        Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => match key.code {
                            KeyCode::Enter => return Some(line),
                            KeyCode::Backspace => {
                                line.pop();
                            }
                            KeyCode::Esc => line.clear(),
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                line.clear();
                            }
                            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                if line.is_empty() {
                                    return None;
                                }
                                line.clear();
                            }
                            KeyCode::Char(character) => line.push(character),
                            _ => {}
                        },
                        Ok(_) => {}
                        Err(_) => return None,
                    }
                }
            }
        }
    }

    /// Note a line that did not come from the event stream (slash output).
    pub fn write_note(&mut self, text: &str, transcript: &Transcript, class: LineClass) {
        self.write_line(text);
        transcript.push(text.to_string(), class);
        self.draw(transcript, "");
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if matches!(self.mode, Mode::Live(_)) {
            use ratatui::crossterm::execute;
            use ratatui::crossterm::terminal::{disable_raw_mode, LeaveAlternateScreen};
            let _ = disable_raw_mode();
            let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
        }
    }
}

/// Terminal-backed approval channel.
pub struct TerminalApprover<'a> {
    terminal: &'a RefCell<Terminal>,
    transcript: &'a Transcript,
}

impl<'a> TerminalApprover<'a> {
    pub fn new(terminal: &'a RefCell<Terminal>, transcript: &'a Transcript) -> Self {
        Self {
            terminal,
            transcript,
        }
    }
}

impl Approver for TerminalApprover<'_> {
    fn decide(&mut self, prompt: &ApprovalPrompt) -> Option<bool> {
        let mut terminal = self.terminal.borrow_mut();
        if terminal.is_live() {
            // The overlay carries the approval-content contract; the raw rows
            // below are the injected-mode equivalent.
            terminal.set_overlay(Some(prompt.clone()), false);
        } else {
            terminal.write_line(&format!(
                "approval required: {} — {}",
                prompt.tool_name, prompt.summary
            ));
            terminal.write_line(&format!(
                "  intent {} · policy revision {} · expires {}",
                prompt.intent_hash, prompt.policy_revision, prompt.expires_at
            ));
        }
        let answer = terminal.prompt("approve? [y/n] ", self.transcript);
        let decision = match answer {
            Some(ref value) => match value.trim().to_ascii_lowercase().as_str() {
                "y" | "yes" => Some(true),
                "n" | "no" => Some(false),
                _ => None,
            },
            None => None,
        };
        if terminal.is_live() {
            terminal.set_overlay(None, false);
        } else if decision.is_none() {
            terminal.write_line("no decision recorded; the ask stays open (blocked)");
        }
        let text = format!(
            "approval decision: {}",
            match decision {
                Some(true) => "approved",
                Some(false) => "denied",
                None => "none (blocked)",
            }
        );
        self.transcript.push(text.clone(), LineClass::Decision);
        terminal.draw(self.transcript, "");
        decision
    }
}

/// Deterministic approver for scripted automation and tests.
#[derive(Debug, Default)]
pub struct ScriptedApprover {
    decisions: std::collections::VecDeque<bool>,
    seen: Vec<String>,
}

impl ScriptedApprover {
    pub fn new(decisions: Vec<bool>) -> Self {
        Self {
            decisions: decisions.into(),
            seen: Vec::new(),
        }
    }

    pub fn seen(&self) -> &[String] {
        &self.seen
    }
}

impl Approver for ScriptedApprover {
    fn decide(&mut self, prompt: &ApprovalPrompt) -> Option<bool> {
        self.seen.push(prompt.summary.clone());
        self.decisions.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollo_protocol::ids::{EventId, SessionId};
    use serde_json::json;

    fn prompt() -> ApprovalPrompt {
        ApprovalPrompt {
            approval_id: "ap_1".into(),
            tool_name: "write_file".into(),
            summary: "write src/main.rs".into(),
            intent_hash: "hash".into(),
            policy_revision: 3,
            expires_at: "2026-10-03T00:05:00Z".into(),
        }
    }

    /// Verdicts a hostile or absent terminal must never produce "yes" from.
    #[test]
    fn absent_or_ambiguous_answers_are_not_approval() {
        let cases: Vec<(&str, Option<bool>)> = vec![
            ("y\n", Some(true)),
            ("yes\n", Some(true)),
            ("Y\n", Some(true)),
            ("n\n", Some(false)),
            ("no\n", Some(false)),
            ("\n", None),
            ("maybe\n", None),
            ("y n\n", None),
        ];
        for (input, expected) in cases {
            let terminal = RefCell::new(Terminal::new(
                Box::new(std::io::Cursor::new(input.to_string())),
                Box::new(Vec::new()),
                Presenter::new(80, false),
            ));
            let transcript = Transcript::new(32);
            let mut approver = TerminalApprover::new(&terminal, &transcript);
            assert_eq!(approver.decide(&prompt()), expected, "input {input:?}");
        }
    }

    #[test]
    fn closed_input_blocks_instead_of_defaulting_to_yes() {
        let terminal = RefCell::new(Terminal::new(
            Box::new(std::io::Cursor::new(Vec::new())),
            Box::new(Vec::new()),
            Presenter::new(80, false),
        ));
        let transcript = Transcript::new(32);
        let mut approver = TerminalApprover::new(&terminal, &transcript);
        assert_eq!(approver.decide(&prompt()), None);
        assert!(transcript.decisions().iter().any(|line| line.contains("blocked")));
    }

    #[test]
    fn events_reach_the_terminal_and_the_transcript() {
        let terminal = RefCell::new(Terminal::new(
            Box::new(std::io::Cursor::new(Vec::new())),
            Box::new(Vec::new()),
            Presenter::new(80, false),
        ));
        let transcript = Transcript::new(32);
        let event = EventEnvelope {
            schema_version: "0.1".into(),
            event_id: EventId::generate(),
            session_id: SessionId::generate(),
            run_id: None,
            seq: 1,
            timestamp: "2026-10-03T00:00:00Z".into(),
            event_type: EventType::RunFinished,
            data: json!({"state": "blocked", "reason": "approval_required", "verification": "skipped"}),
        };
        {
            let mut terminal = terminal.borrow_mut();
            terminal.write_event(&event, &transcript);
        }
        assert_eq!(transcript.len(), 1);
        assert!(transcript.joined().contains("run finished: blocked"));
    }

    #[test]
    fn scripted_approver_remembers_questions_and_exhausts_to_none() {
        let mut approver = ScriptedApprover::new(vec![true]);
        assert_eq!(approver.decide(&prompt()), Some(true));
        assert_eq!(approver.decide(&prompt()), None);
        assert_eq!(
            approver.seen(),
            &["write src/main.rs".to_string(), "write src/main.rs".to_string()]
        );
    }
}
