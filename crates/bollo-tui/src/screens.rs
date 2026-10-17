//! The offscreen-assertable screen renderer (ADR-016 + its style direction).
//!
//! The whole interactive screen is derived from journaled state each frame:
//! the header facts, the transcript's own `Presenter` strings (roles, not new
//! text) and the current input line. The approval overlay appears only while
//! an ask is actually open, and an expired ask renders inert with its reason
//! instead of vanishing. Every region is probed under `TestBackend` — the
//! offscreen test contract from the style direction lives in this module's
//! tests — so the screen is provable without any terminal attached.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::present::{LineClass, Transcript};
use crate::terminal::ApprovalPrompt;

/// Facts shown in the header row. Defaults stay honest: an unfilled field
/// renders `unknown`, never a plausible-looking fabrication.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Header {
    pub workspace: String,
    pub binding: String,
    pub profile: String,
    pub sandbox: String,
}

impl Header {
    fn line(&self) -> String {
        format!(
            "bollo | ws {} | {} | profile {} | sandbox {}",
            self.workspace, self.binding, self.profile, self.sandbox
        )
    }
}

/// One frame's state. Nothing display-only lives here: callers pass the
/// transcript they already journal into and the prompt text as typed.
pub struct ScreenState<'a> {
    pub header: Header,
    pub transcript: &'a Transcript,
    pub input: String,
    pub approval: Option<&'a ApprovalPrompt>,
    pub approval_expired: bool,
}

/// Risk vocabulary first: the title carries the words even without color.
const APPROVAL_TITLE_OPEN: &str = " APPROVAL REQUIRED — y approves, n denies ";
const APPROVAL_TITLE_EXPIRED: &str = " APPROVAL EXPIRED — no decision can be recorded ";

const MIN_COLUMNS: u16 = 50;

/// Below 50 columns the interaction design prefers the linear renderer; the
/// screen renderer refuses rather than claim a layout it cannot draw.
pub fn can_render(width: u16) -> bool {
    width >= MIN_COLUMNS
}

/// The approval overlay content, identical for the live and golden paths.
fn approval_lines(prompt: &ApprovalPrompt) -> Vec<String> {
    vec![
        format!("tool            {}", prompt.tool_name),
        format!("summary         {}", prompt.summary),
        format!("intent hash     {}", prompt.intent_hash),
        format!("policy          revision {}", prompt.policy_revision),
        format!("expires         {}", prompt.expires_at),
        "[y] approve once   [n] deny with reason".into(),
    ]
}

/// Draw one full frame. Everything visible comes from `state`; no widget
/// invents text the journal does not have.
pub fn render(frame: &mut Frame<'_>, state: &ScreenState<'_>, color: bool) {
    let size = frame.area();
    if !can_render(size.width) {
        return;
    }

    let [header, conversation, prompt] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(1),    // conversation pane
            Constraint::Length(1), // prompt line
        ])
        .areas(size);

    let header_style = if color {
        Style::new().add_modifier(Modifier::DIM)
    } else {
        Style::new()
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(state.header.line(), header_style))),
        header,
    );
    frame.render_widget(conversation_paragraph(state.transcript, conversation), conversation);
    frame.render_widget(prompt_paragraph(&state.input), prompt);
    if let Some(ask) = state.approval {
        render_overlay(frame, ask, state.approval_expired, color);
    }
}

fn role_span(class: LineClass, text: String) -> Span<'static> {
    match class {
        // Decisions are bold and never evicted from the buffer above.
        LineClass::Decision => Span::styled(text, Style::new().add_modifier(Modifier::BOLD)),
        // Informational chatter is dim.
        LineClass::Event => Span::styled(text, Style::new().add_modifier(Modifier::DIM)),
        // Assistant text is the reading surface: default foreground.
        LineClass::Text => Span::raw(text),
    }
}

fn conversation_paragraph(transcript: &Transcript, area: ratatui::layout::Rect) -> Paragraph<'static> {
    let lines = transcript.lines_with_class();
    let start = lines.len().saturating_sub(area.height as usize);
    let lines: Vec<Line<'static>> = lines[start..]
        .iter()
        .map(|(class, text)| Line::from(vec![role_span(*class, text.clone())]))
        .collect();
    Paragraph::new(lines)
}

fn prompt_paragraph(input: &str) -> Paragraph<'static> {
    Paragraph::new(Line::from(Span::styled(
        format!("> {input}"),
        Style::new().add_modifier(Modifier::BOLD),
    )))
}

fn overlay_area(frame: &Frame<'_>, line_count: usize) -> ratatui::layout::Rect {
    let area = frame.area();
    let width = area.width.saturating_mul(3) / 4;
    let height = (line_count as u16).saturating_add(2); // borders
    ratatui::layout::Rect {
        x: area.width.saturating_sub(width) / 2,
        y: area.height.saturating_sub(height) / 2,
        width: width.min(area.width),
        height: height.min(area.height),
    }
}

fn render_overlay(frame: &mut Frame<'_>, prompt: &ApprovalPrompt, expired: bool, color: bool) {
    let lines = approval_lines(prompt);
    let area = overlay_area(frame, lines.len());
    frame.render_widget(Clear, area);
    let (title, title_style) = if expired {
        (
            APPROVAL_TITLE_EXPIRED,
            Style::new().add_modifier(Modifier::BOLD),
        )
    } else if color {
        (
            APPROVAL_TITLE_OPEN,
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        )
    } else {
        (
            APPROVAL_TITLE_OPEN,
            Style::new().add_modifier(Modifier::BOLD),
        )
    };
    let block = Block::bordered().title(Span::styled(title, title_style));
    let body: Vec<Line<'static>> = lines.iter().map(|line| Line::from(line.clone())).collect();
    frame.render_widget(Paragraph::new(body).block(block), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::present::Presenter;
    use bollo_protocol::events::EventEnvelope;
    use bollo_protocol::ids::{EventId, SessionId};
    use bollo_protocol::EventType;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Cell;
    use ratatui::Terminal;
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

    fn header() -> Header {
        Header {
            workspace: "ws_fix".into(),
            binding: "anthropic/test-model".into(),
            profile: "balanced".into(),
            sandbox: "off".into(),
        }
    }

    fn state<'a>(transcript: &'a Transcript, input: &str) -> ScreenState<'a> {
        ScreenState {
            header: header(),
            transcript,
            input: input.to_string(),
            approval: None,
            approval_expired: false,
        }
    }

    /// Render offscreen and return each screen row, trailing blanks trimmed.
    fn rows(
        width: u16,
        height: u16,
        screen: &ScreenState<'_>,
        color: bool,
    ) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
        terminal
            .draw(|frame| render(frame, screen, color))
            .expect("draw");
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|row| {
                let mut line = String::new();
                for col in 0..width {
                    line.push_str(buffer.get(col, row).symbol());
                }
                line.trim_end().to_string()
            })
            .collect()
    }

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

    /// Cell assertions at 80 and 120 columns: header text, one streaming delta
    /// sequence and the prompt line are all present (contract item 1).
    #[test]
    fn renders_contracted_regions_at_80_and_120_columns() {
        for (width, height) in [(80u16, 9u16), (120u16, 9u16)] {
            let presenter = Presenter::new(width as usize, false);
            let transcript = Transcript::new(64);
            let delta = event(EventType::AssistantDelta, json!({"text": "reading hello.txt"}));
            transcript.push(
                presenter.render(&delta).unwrap(),
                Presenter::line_class(delta.event_type),
            );
            let finished = event(
                EventType::RunFinished,
                json!({"state": "completed", "reason": null, "verification": "skipped"}),
            );
            transcript.push(
                presenter.render(&finished).unwrap(),
                Presenter::line_class(finished.event_type),
            );
            let screen = rows(width, height, &state(&transcript, "explain more"), false);
            let screen_text = screen.join("\n");
            assert!(screen[0].contains("bollo"), "{width}: {screen_text:?}");
            assert!(screen[0].contains("ws ws_fix"), "{width}: {screen_text:?}");
            assert!(screen[0].contains("profile balanced"), "{width}: {screen_text:?}");
            assert!(
                screen[0].contains("anthropic/test-model"),
                "{width}: {screen_text:?}"
            );
            assert!(screen_text.contains("reading hello.txt"), "{width}: {screen_text:?}");
            assert!(screen_text.contains("run finished: completed"), "{width}: {screen_text:?}");
            assert!(
                screen[height as usize - 1].contains("> explain more"),
                "{width}: {screen_text:?}"
            );
        }
    }

    /// The streaming path shows delta text as it arrives (contract item 1).
    #[test]
    fn streaming_delta_sequence_is_visible() {
        let presenter = Presenter::default();
        let transcript = Transcript::new(64);
        for text in ["reading", " hello.txt"] {
            let delta = event(EventType::AssistantDelta, json!({ "text": text }));
            transcript.push(
                presenter.render(&delta).unwrap(),
                Presenter::line_class(delta.event_type),
            );
        }
        let screen_text = rows(80, 6, &state(&transcript, ""), false).join("\n");
        assert!(screen_text.contains("reading"), "{screen_text:?}");
        assert!(screen_text.contains("hello.txt"), "{screen_text:?}");
    }

    /// Overlay assertions: full content while open; the expired state renders
    /// the same box inert with its reason, never a disappearing box (item 2).
    #[test]
    fn approval_overlay_shows_the_contract_and_expires_inert() {
        let transcript = Transcript::new(64);
        let ask = prompt();
        let mut screen = ScreenState {
            header: header(),
            transcript: &transcript,
            input: String::new(),
            approval: Some(&ask),
            approval_expired: false,
        };
        let open = rows(90, 10, &screen, false).join("\n");
        for expected in [
            "APPROVAL REQUIRED",
            "write_file",
            "write src/main.rs",
            "revision 3",
            "expires",
            "[y] approve once",
            "[n] deny",
        ] {
            assert!(open.contains(expected), "missing {expected:?}: {open:?}");
        }

        screen.approval_expired = true;
        let expired = rows(90, 10, &screen, false).join("\n");
        assert!(expired.contains("APPROVAL EXPIRED"), "{expired:?}");
        assert!(expired.contains("no decision can be recorded"), "{expired:?}");
        assert!(expired.contains("write_file"), "expired overlay lists the tool: {expired:?}");
        assert!(!expired.contains("y approves"), "expired overlay is inert: {expired:?}");
    }

    /// Color-agnostic checks: with color off, no cell that carries a word is
    /// colored, and the words remain the payload (item 3).
    #[test]
    fn no_color_carries_words_not_style_only() {
        let presenter = Presenter::new(80, false);
        let transcript = Transcript::new(64);
        let delta = event(EventType::AssistantDelta, json!({"text": "plain text line"}));
        transcript.push(
            presenter.render(&delta).unwrap(),
            Presenter::line_class(delta.event_type),
        );
        let screen = state(&transcript, "");
        let mut terminal = Terminal::new(TestBackend::new(80, 6)).expect("test terminal");
        terminal.draw(|frame| render(frame, &screen, false)).expect("draw");
        let buffer = terminal.backend().buffer();
        for cell in buffer.content() {
            let paints = |fg: Option<Color>| fg.is_some_and(|color| color != Color::Reset);
            if cell.symbol() != " "
                && (paints(cell.style().fg) || paints(cell.style().bg))
            {
                panic!(
                    "colored cell {:?} style={{fg: {:?}, bg: {:?}, modifier: {:?}}}",
                    cell.symbol(),
                    cell.style().fg,
                    cell.style().bg,
                    cell.style().add_modifier
                );
            }
        }
        let screen_text = rows(80, 6, &screen, false).join("\n");
        assert!(screen_text.contains("plain text line"), "{screen_text:?}");
    }

    /// Eviction proof: transcript pressure evicts text before decisions, and
    /// the last decision stays visible in the drawn buffer (item 4).
    #[test]
    fn transcript_pressure_keeps_the_last_decision_visible() {
        let presenter = Presenter::default();
        let transcript = Transcript::new(8);
        let finished = event(
            EventType::RunFinished,
            json!({"state": "blocked", "reason": "approval_required", "verification": "skipped"}),
        );
        transcript.push(
            presenter.render(&finished).unwrap(),
            Presenter::line_class(finished.event_type),
        );
        for index in 0..40 {
            let delta = event(
                EventType::AssistantDelta,
                json!({"text": format!("chunk {index}")}),
            );
            transcript.push(
                presenter.render(&delta).unwrap(),
                Presenter::line_class(delta.event_type),
            );
        }
        let screen_text = rows(80, 12, &state(&transcript, ""), false).join("\n");
        assert!(
            screen_text.contains("run finished: blocked"),
            "decision must survive pressure: {screen_text:?}"
        );
        assert!(
            !screen_text.contains("chunk 0"),
            "evicted chatter must leave the screen: {screen_text:?}"
        );
        assert!(
            screen_text.contains("chunk 39"),
            "newest chatter is on screen: {screen_text:?}"
        );
    }

    /// Below 50 columns the renderer refuses (linear output takes over).
    #[test]
    fn narrow_terminals_refuse_the_screen_renderer() {
        assert!(!can_render(49));
        assert!(can_render(50));
    }
}
