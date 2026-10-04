//! Terminal I/O and the interactive approval channel.
//!
//! The client can present an ask, but it can never answer one on the user's
//! behalf: end-of-input, empty input, an unrecognized word or a closed terminal
//! all mean "not approved", which makes the runtime block with
//! `approval_required` instead of proceeding.

use std::cell::RefCell;
use std::io::{BufRead, Write};

use bollo_protocol::events::EventEnvelope;
use bollo_protocol::EventType;

use crate::present::{LineClass, Presenter, Transcript};

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

/// Line-oriented terminal. Input and output are injected so the whole client
/// can be driven deterministically in tests.
pub struct Terminal {
    input: Box<dyn BufRead>,
    output: Box<dyn Write>,
    presenter: Presenter,
}

impl Terminal {
    pub fn new(input: Box<dyn BufRead>, output: Box<dyn Write>, presenter: Presenter) -> Self {
        Self {
            input,
            output,
            presenter,
        }
    }

    pub fn presenter(&self) -> &Presenter {
        &self.presenter
    }

    pub fn write_line(&mut self, line: &str) {
        let fitted = self.presenter.fit(line);
        let _ = writeln!(self.output, "{fitted}");
        let _ = self.output.flush();
    }

    /// Write one durable event and record it in the transcript. Assistant text
    /// streams without newlines; everything else is a line.
    pub fn write_event(&mut self, event: &EventEnvelope, transcript: &Transcript) {
        let Some(rendered) = self.presenter.render(event) else {
            return;
        };
        let class = Presenter::line_class(event.event_type);
        if event.event_type == EventType::AssistantDelta {
            let _ = write!(self.output, "{rendered}");
            let _ = self.output.flush();
        } else {
            let style = Presenter::style_for(event.event_type);
            let styled = self.presenter.style(&rendered, style);
            let _ = writeln!(self.output, "{styled}");
            let _ = self.output.flush();
        }
        transcript.push(rendered, class);
    }

    /// Read one line; `None` on end of input.
    pub fn read_line(&mut self) -> Option<String> {
        let mut buffer = String::new();
        match self.input.read_line(&mut buffer) {
            Ok(0) => None,
            Ok(_) => Some(buffer.trim_end_matches(['\r', '\n']).to_string()),
            Err(_) => None,
        }
    }

    pub fn prompt(&mut self, label: &str) -> Option<String> {
        let _ = write!(self.output, "{label}");
        let _ = self.output.flush();
        self.read_line()
    }

    /// Note a line that did not come from the event stream (slash output).
    pub fn write_note(&mut self, text: &str, transcript: &Transcript, class: LineClass) {
        self.write_line(text);
        transcript.push(text.to_string(), class);
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
        terminal.write_line(&format!(
            "approval required: {} — {}",
            prompt.tool_name, prompt.summary
        ));
        terminal.write_line(&format!(
            "  intent {} · policy revision {} · expires {}",
            prompt.intent_hash, prompt.policy_revision, prompt.expires_at
        ));
        let answer = terminal.prompt("approve? [y/n] ");
        let decision = match answer {
            Some(ref value) => match value.trim().to_ascii_lowercase().as_str() {
                "y" | "yes" => Some(true),
                "n" | "no" => Some(false),
                _ => None,
            },
            None => None,
        };
        if decision.is_none() {
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
        drop(terminal);
        self.transcript.push(text, LineClass::Decision);
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
