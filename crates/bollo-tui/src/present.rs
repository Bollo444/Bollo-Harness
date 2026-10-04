//! Presentation layer for the interactive client.
//!
//! Rules that matter for safety, not cosmetics:
//! - approval, policy, verification and terminal events are *decisions*; the
//!   transcript buffer may evict text lines under pressure but never a decision
//!   line, so the reason a run stopped is always still on screen;
//! - rendering is width-aware, so an 80-column and a 120-column terminal both
//!   stay readable, and the no-color path emits no ANSI escapes at all.

use std::cell::RefCell;

use bollo_protocol::events::EventEnvelope;
use bollo_protocol::EventType;

pub const MIN_WIDTH: usize = 40;
pub const MAX_WIDTH: usize = 240;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Dim,
    Tool,
    Approval,
    Error,
}

/// How a rendered line is treated by transcript eviction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineClass {
    /// Assistant text and other chatter: evictable.
    Text,
    /// Informational event: evictable, but rendered in full.
    Event,
    /// Permission / policy / verification / terminal: never evicted.
    Decision,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Presenter {
    width: usize,
    color: bool,
}

impl Default for Presenter {
    fn default() -> Self {
        Self::new(80, false)
    }
}

impl Presenter {
    pub fn new(width: usize, color: bool) -> Self {
        Self {
            width: width.clamp(MIN_WIDTH, MAX_WIDTH),
            color,
        }
    }

    /// Read the terminal width from `COLUMNS` when present; `NO_COLOR` always
    /// wins over an explicitly requested color path.
    pub fn from_env(color: bool) -> Self {
        let width = std::env::var("COLUMNS")
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(80);
        Self::new(width, color && std::env::var_os("NO_COLOR").is_none())
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn color(&self) -> bool {
        self.color
    }

    /// Fit one line to the width, appending an ellipsis when truncated.
    pub fn fit(&self, line: &str) -> String {
        if line.chars().count() <= self.width {
            return line.to_string();
        }
        let keep = self.width.saturating_sub(1);
        let mut out: String = line.chars().take(keep).collect();
        out.push('…');
        out
    }

    pub fn style(&self, text: &str, kind: Style) -> String {
        if !self.color {
            return text.to_string();
        }
        let code = match kind {
            Style::Dim => "2",
            Style::Tool => "36",
            Style::Approval => "33",
            Style::Error => "31",
        };
        format!("\u{1b}[{code}m{text}\u{1b}[0m")
    }

    pub fn style_for(event_type: EventType) -> Style {
        match event_type {
            EventType::ToolProposed | EventType::ToolResult => Style::Tool,
            EventType::ApprovalRequested | EventType::ApprovalResolved => Style::Approval,
            EventType::VerificationResult => Style::Error,
            _ => Style::Dim,
        }
    }

    pub fn line_class(event_type: EventType) -> LineClass {
        match event_type {
            EventType::ApprovalRequested
            | EventType::ApprovalResolved
            | EventType::PolicyChanged
            | EventType::VerificationResult
            | EventType::RunFinished => LineClass::Decision,
            EventType::AssistantDelta => LineClass::Text,
            _ => LineClass::Event,
        }
    }

    /// Render one durable event. `None` means "nothing visible" (for example an
    /// empty delta), never "dropped a decision".
    pub fn render(&self, event: &EventEnvelope) -> Option<String> {
        let line = match event.event_type {
            EventType::AssistantDelta => {
                let text = event.data.get("text").and_then(|value| value.as_str())?;
                if text.is_empty() {
                    return None;
                }
                return Some(text.to_string());
            }
            EventType::RunStarted => format!(
                "run {} started ({})",
                event
                    .run_id
                    .as_ref()
                    .map(|run| run.to_string())
                    .unwrap_or_default(),
                event.data.get("provider").and_then(|v| v.as_str()).unwrap_or("?"),
            ),
            EventType::ToolProposed => format!(
                "tool {} → {} ({})",
                event.data.get("tool_name").and_then(|v| v.as_str()).unwrap_or("?"),
                event.data.get("decision").and_then(|v| v.as_str()).unwrap_or("?"),
                event.data.get("reason").and_then(|v| v.as_str()).unwrap_or("")
            ),
            EventType::ToolResult => format!(
                "tool result {}: {}",
                event.data.get("status").and_then(|v| v.as_str()).unwrap_or("?"),
                event.data.get("summary").and_then(|v| v.as_str()).unwrap_or("")
            ),
            EventType::ApprovalRequested => format!(
                "approval required: {} (expires {})",
                event.data.get("summary").and_then(|v| v.as_str()).unwrap_or(""),
                event.data.get("expires_at").and_then(|v| v.as_str()).unwrap_or("?")
            ),
            EventType::ApprovalResolved => format!(
                "approval {}: {}",
                event
                    .data
                    .get("approval_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?"),
                event.data.get("decision").and_then(|v| v.as_str()).unwrap_or("?")
            ),
            EventType::PolicyChanged => format!(
                "policy changed → revision {} ({} / {})",
                event
                    .data
                    .get("policy_revision")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
                event.data.get("profile").and_then(|v| v.as_str()).unwrap_or("?"),
                event.data.get("sandbox").and_then(|v| v.as_str()).unwrap_or("?")
            ),
            EventType::VerificationResult => format!(
                "verification: {} ({})",
                event.data.get("status").and_then(|v| v.as_str()).unwrap_or("?"),
                event
                    .data
                    .get("command")
                    .and_then(|v| v.as_array())
                    .map(|argv| argv
                        .iter()
                        .filter_map(|part| part.as_str())
                        .collect::<Vec<_>>()
                        .join(" "))
                    .unwrap_or_default()
            ),
            EventType::UsageUpdated => format!(
                "usage: in={} out={} cost={}",
                event.data.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                event.data.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                event
                    .data
                    .get("cost_microusd")
                    .and_then(|v| v.as_u64())
                    .map(|micro| format!("{micro}µ$"))
                    .unwrap_or_else(|| "unknown".into())
            ),
            EventType::RunFinished => format!(
                "run finished: {} (verification: {})",
                event.data.get("state").and_then(|v| v.as_str()).unwrap_or("?"),
                event
                    .data
                    .get("verification")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
            ),
        };
        Some(self.fit(&line))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TranscriptLine {
    class: LineClass,
    text: String,
}

/// Bounded transcript buffer. Under pressure it evicts the oldest *evictable*
/// line; decision lines (approvals, policy changes, verification, terminal
/// state) survive arbitrary churn.
#[derive(Debug)]
pub struct Transcript {
    lines: RefCell<Vec<TranscriptLine>>,
    capacity: usize,
}

impl Transcript {
    pub fn new(capacity: usize) -> Self {
        Self {
            lines: RefCell::new(Vec::new()),
            capacity: capacity.max(2),
        }
    }

    pub fn len(&self) -> usize {
        self.lines.borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn push(&self, text: String, class: LineClass) {
        let mut lines = self.lines.borrow_mut();
        if lines.len() >= self.capacity {
            match lines.iter().position(|line| line.class != LineClass::Decision) {
                Some(index) => {
                    lines.remove(index);
                }
                None => {
                    lines.remove(0);
                }
            }
        }
        lines.push(TranscriptLine { class, text });
    }

    pub fn snapshot(&self) -> Vec<String> {
        self.lines
            .borrow()
            .iter()
            .map(|line| line.text.clone())
            .collect()
    }

    pub fn joined(&self) -> String {
        self.snapshot().join("\n")
    }

    pub fn decisions(&self) -> Vec<String> {
        self.lines
            .borrow()
            .iter()
            .filter(|line| line.class == LineClass::Decision)
            .map(|line| line.text.clone())
            .collect()
    }
}

impl Default for Transcript {
    fn default() -> Self {
        Self::new(2_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollo_protocol::ids::{EventId, SessionId};
    use serde_json::json;

    fn event(event_type: EventType, data: serde_json::Value) -> EventEnvelope {
        EventEnvelope {
            schema_version: "0.1".into(),
            event_id: EventId::generate(),
            session_id: SessionId::generate(),
            run_id: None,
            seq: 1,
            timestamp: "2026-10-03T00:00:00Z".into(),
            event_type,
            data,
        }
    }

    #[test]
    fn width_is_respected_at_80_and_120_columns() {
        let long = "x".repeat(500);
        for width in [80usize, 120] {
            let presenter = Presenter::new(width, false);
            let fitted = presenter.fit(&long);
            assert!(fitted.chars().count() <= width);
            assert!(fitted.ends_with('…'));
        }
        let presenter = Presenter::new(80, false);
        assert_eq!(presenter.fit("short"), "short");
    }

    #[test]
    fn no_color_path_never_emits_ansi() {
        let plain = Presenter::new(80, false);
        assert!(!plain.style("tool", Style::Tool).contains('\u{1b}'));
        let colored = Presenter::new(80, true);
        assert!(colored.style("tool", Style::Tool).contains('\u{1b}'));
    }

    #[test]
    fn decisions_survive_transcript_pressure() {
        let presenter = Presenter::default();
        let transcript = Transcript::new(8);
        let approval = event(
            EventType::ApprovalRequested,
            json!({
                "approval_id": "ap_1",
                "tool_call_id": "call_1",
                "intent_hash": "h",
                "policy_revision": 1,
                "expires_at": "2026-10-03T00:05:00Z",
                "summary": "write src/main.rs"
            }),
        );
        let finished = event(
            EventType::RunFinished,
            json!({"state": "blocked", "reason": "approval_required", "verification": "skipped"}),
        );
        for forced in [approval, finished] {
            let line = presenter.render(&forced).unwrap();
            transcript.push(line, Presenter::line_class(forced.event_type));
        }
        // Hundreds of text lines churn the buffer.
        for index in 0..500 {
            let delta = event(EventType::AssistantDelta, json!({"text": format!("chunk {index}")}));
            let line = presenter.render(&delta).unwrap();
            transcript.push(line, Presenter::line_class(delta.event_type));
        }
        let decisions = transcript.decisions();
        assert_eq!(decisions.len(), 2);
        assert!(decisions[0].contains("approval required: write src/main.rs"));
        assert!(decisions[1].contains("run finished: blocked"));
        assert!(transcript.len() <= 8);
    }

    #[test]
    fn empty_deltas_render_nothing() {
        let presenter = Presenter::default();
        let delta = event(EventType::AssistantDelta, json!({"text": ""}));
        assert!(presenter.render(&delta).is_none());
    }

    #[test]
    fn tool_and_verification_lines_are_readable() {
        let presenter = Presenter::new(120, false);
        let proposed = event(
            EventType::ToolProposed,
            json!({"tool_name": "write_file", "decision": "ask", "reason": "policy"}),
        );
        assert_eq!(
            presenter.render(&proposed).unwrap(),
            "tool write_file → ask (policy)"
        );
        let verification = event(
            EventType::VerificationResult,
            json!({"status": "failed", "command": ["cargo", "test"], "exit_code": 101}),
        );
        assert_eq!(
            presenter.render(&verification).unwrap(),
            "verification: failed (cargo test)"
        );
    }
}
