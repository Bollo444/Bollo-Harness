//! Context compaction. Triggered at a percentage of the provider capacity,
//! the kept window always starts at a text-only user message, so no
//! `tool_result` is ever separated from its `tool_use`. Failure (no safe
//! boundary) leaves the original messages untouched and says so.

use bollo_providers::ChatMessage;

pub const COMPACT_TRIGGER_PERCENT: u64 = 80;

/// Cheap token estimate (~4 bytes per token) over serialized content.
pub fn estimate_tokens(messages: &[ChatMessage]) -> u64 {
    let bytes: usize = messages
        .iter()
        .map(|message| serde_json::to_string(message).map(|s| s.len()).unwrap_or(0))
        .sum();
    (bytes as u64) / 4 + messages.len() as u64
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompactionOutcome {
    pub messages: Vec<ChatMessage>,
    pub dropped: usize,
    pub summary: Option<String>,
}

/// Compact when the estimate exceeds the trigger fraction of the capacity.
pub fn compact(messages: Vec<ChatMessage>, capacity_tokens: u64) -> CompactionOutcome {
    let trigger = capacity_tokens.saturating_mul(COMPACT_TRIGGER_PERCENT) / 100;
    if estimate_tokens(&messages) <= trigger {
        return CompactionOutcome {
            messages,
            dropped: 0,
            summary: None,
        };
    }
    let mut cut = 0usize;
    for (index, message) in messages.iter().enumerate() {
        if index == 0 {
            continue;
        }
        let is_user_text = message.role == bollo_providers::Role::User
            && !message.content.is_empty()
            && message
                .content
                .iter()
                .all(|block| matches!(block, bollo_providers::ContentBlock::Text { .. }));
        if is_user_text {
            cut = index;
            if estimate_tokens(&messages[index..]) <= trigger {
                break;
            }
        }
    }
    if cut == 0 {
        return CompactionOutcome {
            messages,
            dropped: 0,
            summary: Some(
                "[compaction could not find a safe boundary; narrow the context instead]".into(),
            ),
        };
    }
    let summary = format!(
        "[compacted {cut} earlier message(s); summaries are lossy and cannot replace permission \
         grants or prove tool success]"
    );
    let mut kept = messages[cut..].to_vec();
    kept.insert(0, ChatMessage::assistant_text(summary.clone()));
    CompactionOutcome {
        messages: kept,
        dropped: cut,
        summary: Some(summary),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollo_providers::{ContentBlock, Role};
    use serde_json::json;

    fn pair(index: usize) -> Vec<ChatMessage> {
        vec![
            ChatMessage::user_text(format!("request {index} {}", "x".repeat(200))),
            ChatMessage {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    call_id: format!("call_{index}"),
                    name: "read_file".into(),
                    arguments: json!({"path": "a.txt"}),
                }],
            },
            ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    call_id: format!("call_{index}"),
                    content: "y".repeat(200),
                    is_error: false,
                }],
            },
        ]
    }

    #[test]
    fn compaction_preserves_complete_tool_pairs() {
        let mut messages = Vec::new();
        for index in 0..8 {
            messages.extend(pair(index));
        }
        let outcome = compact(messages, 1_000);
        assert!(outcome.dropped > 0, "expected compaction to trigger");
        assert!(outcome.summary.is_some());
        // No orphan tool results at the start; every result still follows its
        // assistant tool_use in the kept window.
        let kept = &outcome.messages;
        for (index, message) in kept.iter().enumerate() {
            if message
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
            {
                assert!(index > 0, "tool result must not be the first message");
                assert!(kept[index - 1]
                    .content
                    .iter()
                    .any(|block| matches!(block, ContentBlock::ToolUse { .. })));
            }
        }
    }

    #[test]
    fn small_contexts_are_not_compacted() {
        let messages = vec![ChatMessage::user_text("hello")];
        let outcome = compact(messages.clone(), 100_000);
        assert_eq!(outcome.dropped, 0);
        assert!(outcome.summary.is_none());
        assert_eq!(outcome.messages, messages);
    }

    #[test]
    fn a_single_oversized_turn_is_reported_not_mangled() {
        let messages = vec![ChatMessage::user_text("z".repeat(40_000))];
        let outcome = compact(messages.clone(), 1_000);
        assert_eq!(outcome.dropped, 0);
        assert_eq!(outcome.messages, messages);
        assert!(outcome.summary.unwrap().contains("could not find a safe boundary"));
    }
}
