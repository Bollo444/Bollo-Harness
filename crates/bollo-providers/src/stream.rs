//! Streaming normalization: assemble fragmented tool-call arguments by call id.
//!
//! Rules (docs/architecture/context-and-providers.md):
//! - never execute until the final arguments validate;
//! - repeated tool ids within one response are deduplicated;
//! - oversize or invalid JSON is rejected before dispatch;
//! - an empty argument stream means `{}` only when the tool schema allows it;
//!   validation of the resulting shape is the tool layer's job.

use std::collections::BTreeMap;

use serde_json::Value;

use bollo_protocol::errors::ErrorCode;

use crate::{ProviderError, ToolIntent};

pub const MAX_TOOL_ARGUMENT_BYTES: usize = 1_048_576;

#[derive(Debug, Default)]
struct PendingCall {
    name: String,
    args: String,
}

#[derive(Debug, Default)]
pub struct StreamAssembler {
    order: Vec<String>,
    calls: BTreeMap<String, PendingCall>,
}

impl StreamAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool call. A repeated id within the same response is
    /// deduplicated (the first registration wins).
    pub fn begin(&mut self, call_id: &str, name: &str) {
        if self.calls.contains_key(call_id) {
            return;
        }
        self.order.push(call_id.to_string());
        self.calls.insert(
            call_id.to_string(),
            PendingCall {
                name: name.to_string(),
                args: String::new(),
            },
        );
    }

    pub fn is_tracking(&self, call_id: &str) -> bool {
        self.calls.contains_key(call_id)
    }

    pub fn push_arguments(&mut self, call_id: &str, fragment: &str) -> Result<(), ProviderError> {
        let entry = self.calls.get_mut(call_id).ok_or_else(|| {
            ProviderError::new(
                ErrorCode::ProviderError,
                format!("argument fragment for unknown tool call {call_id}"),
            )
        })?;
        if entry.args.len() + fragment.len() > MAX_TOOL_ARGUMENT_BYTES {
            return Err(ProviderError::new(
                ErrorCode::ProviderError,
                format!("tool arguments exceed {MAX_TOOL_ARGUMENT_BYTES} bytes"),
            ));
        }
        entry.args.push_str(fragment);
        Ok(())
    }

    /// Validate every registered call and return complete intents in order.
    pub fn finish(&mut self) -> Result<Vec<ToolIntent>, ProviderError> {
        let mut intents = Vec::new();
        for call_id in std::mem::take(&mut self.order) {
            let Some(call) = self.calls.remove(&call_id) else {
                continue;
            };
            if call.name.is_empty() {
                return Err(ProviderError::new(
                    ErrorCode::ProviderError,
                    format!("tool call {call_id} completed without a name"),
                ));
            }
            let arguments = if call.args.trim().is_empty() {
                Value::Object(serde_json::Map::new())
            } else {
                let value: Value = serde_json::from_str(&call.args).map_err(|err| {
                    ProviderError::new(
                        ErrorCode::ProviderError,
                        format!("tool arguments for {call_id} are not valid JSON: {err}"),
                    )
                })?;
                if !value.is_object() {
                    return Err(ProviderError::new(
                        ErrorCode::ProviderError,
                        format!("tool arguments for {call_id} must be a JSON object"),
                    ));
                }
                value
            };
            intents.push(ToolIntent {
                call_id,
                name: call.name,
                arguments,
            });
        }
        Ok(intents)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn assembles_fragmented_arguments_into_one_intent() {
        let mut assembler = StreamAssembler::new();
        assembler.begin("call_1", "apply_patch");
        let fragments = ["{\"pa", "th\":\"src/", "main.rs\",\"old_", "text\":\"a\",\"new_te", "xt\":\"b\"}"];
        for fragment in fragments {
            assembler.push_arguments("call_1", fragment).unwrap();
        }
        let intents = assembler.finish().unwrap();
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].call_id, "call_1");
        assert_eq!(intents[0].name, "apply_patch");
        assert_eq!(intents[0].arguments, json!({"path": "src/main.rs", "old_text": "a", "new_text": "b"}));
    }

    #[test]
    fn invalid_json_is_rejected_before_dispatch() {
        let mut assembler = StreamAssembler::new();
        assembler.begin("call_1", "exec");
        assembler.push_arguments("call_1", "{\"argv\": [").unwrap();
        assert!(assembler.finish().is_err());
    }

    #[test]
    fn non_object_arguments_are_rejected() {
        let mut assembler = StreamAssembler::new();
        assembler.begin("call_1", "exec");
        assembler.push_arguments("call_1", "[1,2,3]").unwrap();
        assert!(assembler.finish().is_err());
    }

    #[test]
    fn repeated_ids_are_deduplicated() {
        let mut assembler = StreamAssembler::new();
        assembler.begin("call_1", "read_file");
        assembler.begin("call_1", "read_file");
        assembler.push_arguments("call_1", "{\"path\":\"a\"}").unwrap();
        let intents = assembler.finish().unwrap();
        assert_eq!(intents.len(), 1);
    }

    #[test]
    fn empty_arguments_become_empty_object() {
        let mut assembler = StreamAssembler::new();
        assembler.begin("call_1", "git_status");
        let intents = assembler.finish().unwrap();
        assert_eq!(intents[0].arguments, json!({}));
    }

    #[test]
    fn missing_name_and_unknown_call_are_errors() {
        let mut assembler = StreamAssembler::new();
        assembler.begin("call_1", "");
        assert!(assembler.finish().is_err());

        let mut assembler = StreamAssembler::new();
        assert!(assembler.push_arguments("ghost", "{}").is_err());
    }

    #[test]
    fn oversize_arguments_are_rejected() {
        let mut assembler = StreamAssembler::new();
        assembler.begin("call_1", "write_file");
        let chunk = "x".repeat(1024);
        let mut error = None;
        for _ in 0..(MAX_TOOL_ARGUMENT_BYTES / 1024 + 2) {
            if let Err(err) = assembler.push_arguments("call_1", &chunk) {
                error = Some(err);
                break;
            }
        }
        assert!(error.is_some());
    }
}
