//! Opaque typed identifiers.
//!
//! All ids match `^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$` (max 128 ASCII characters,
//! per docs/contracts/event.schema.json). Deserialization validates, so an
//! invalid id can never enter the runtime through a wire boundary.

use serde::{Deserialize, Serialize};

use crate::errors::ProtocolError;

/// Validate the shared opaque-id pattern.
pub fn is_valid_opaque_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > 128 {
        return false;
    }
    if !bytes[0].is_ascii_alphanumeric() {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
}

macro_rules! opaque_id {
    ($name:ident, $prefix:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// Parse and validate an existing identifier string.
            pub fn parse(value: impl Into<String>) -> Result<Self, ProtocolError> {
                let value = value.into();
                if is_valid_opaque_id(&value) {
                    Ok(Self(value))
                } else {
                    Err(ProtocolError::InvalidId(value))
                }
            }

            /// Generate a fresh identifier with the type's prefix.
            pub fn generate() -> Self {
                Self(format!("{}_{}", $prefix, uuid::Uuid::new_v4().simple()))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = ProtocolError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::parse(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

opaque_id!(SessionId, "sess");
opaque_id!(RunId, "run");
opaque_id!(ToolCallId, "call");
opaque_id!(EventId, "evt");
opaque_id!(ApprovalId, "apr");
opaque_id!(ArtifactId, "art");
opaque_id!(ServerId, "srv");
opaque_id!(CheckpointId, "ckpt");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_pattern_examples() {
        assert!(is_valid_opaque_id("a"));
        assert!(is_valid_opaque_id("sess_0123456789abcdef"));
        assert!(is_valid_opaque_id("A-Z_9"));
        assert!(is_valid_opaque_id(&"x".repeat(128)));
    }

    #[test]
    fn rejects_invalid_ids() {
        assert!(!is_valid_opaque_id(""));
        assert!(!is_valid_opaque_id("_leading"));
        assert!(!is_valid_opaque_id("-leading"));
        assert!(!is_valid_opaque_id("has space"));
        assert!(!is_valid_opaque_id("sémantique"));
        assert!(!is_valid_opaque_id(&"x".repeat(129)));
    }

    #[test]
    fn generated_ids_roundtrip_through_json() {
        let id = SessionId::generate();
        let json = serde_json::to_string(&id).unwrap();
        let back: SessionId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
        assert!(id.as_str().starts_with("sess_"));
    }

    #[test]
    fn deserialization_rejects_bad_payload() {
        let bad = serde_json::from_str::<RunId>("\"bad id\"");
        assert!(bad.is_err());
    }
}
