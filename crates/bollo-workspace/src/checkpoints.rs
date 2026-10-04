//! Patch checkpoints: preimage capture, postimage recording and conditional
//! restore. Restore succeeds only when the current file still matches Bollo's
//! own postimage; user edits are never overwritten.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use bollo_protocol::ids::CheckpointId;
use bollo_protocol::timeutil;

use crate::WsError;

pub const MAX_PREIMAGE_BYTES: usize = 1_048_576;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub id: CheckpointId,
    /// Workspace-relative path as the user sees it.
    pub path: String,
    pub existed: bool,
    pub pre_sha256: Option<String>,
    pub post_sha256: Option<String>,
    /// Captured preimage, only for text files within the size cap. `None`
    /// means restore is unavailable and that limitation is disclosed.
    pub pre_text: Option<String>,
    pub captured_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestorePlan {
    WritePreimage { path: String, text: String },
    DeleteCreated { path: String },
}

#[derive(Debug, Clone, Default)]
pub struct CheckpointLog {
    entries: BTreeMap<String, Checkpoint>,
}

impl CheckpointLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, id: &CheckpointId) -> Option<&Checkpoint> {
        self.entries.get(id.as_str())
    }

    /// Rehydrate a checkpoint loaded from durable storage so the same
    /// conditional-restore checks apply to it.
    pub fn insert(&mut self, checkpoint: Checkpoint) {
        self.entries
            .insert(checkpoint.id.as_str().to_string(), checkpoint);
    }

    pub fn all(&self) -> impl Iterator<Item = &Checkpoint> {
        self.entries.values()
    }

    /// Capture the preimage of a mutation. `observed` is the current file
    /// content, or `None` when the file does not exist.
    pub fn capture(&mut self, path: &str, observed: Option<&[u8]>) -> Checkpoint {
        let (existed, pre_sha256, pre_text) = match observed {
            Some(bytes) => {
                let hash = hex::encode(Sha256::digest(bytes));
                let text = if bytes.len() <= MAX_PREIMAGE_BYTES && !bytes.contains(&0) {
                    String::from_utf8(bytes.to_vec()).ok()
                } else {
                    None
                };
                (true, Some(hash), text)
            }
            None => (false, None, None),
        };
        let checkpoint = Checkpoint {
            id: CheckpointId::generate(),
            path: path.to_string(),
            existed,
            pre_sha256,
            post_sha256: None,
            pre_text,
            captured_at: timeutil::now_rfc3339(),
        };
        self.entries
            .insert(checkpoint.id.as_str().to_string(), checkpoint.clone());
        checkpoint
    }

    /// Record the observed postimage hash after the mutation succeeded.
    pub fn mark_post(
        &mut self,
        id: &CheckpointId,
        post_bytes: &[u8],
    ) -> Result<(), WsError> {
        let entry = self
            .entries
            .get_mut(id.as_str())
            .ok_or_else(|| WsError::CheckpointNotFound(id.to_string()))?;
        entry.post_sha256 = Some(hex::encode(Sha256::digest(post_bytes)));
        Ok(())
    }

    /// Preview a restore. Refuses when the current file differs from the
    /// recorded postimage (user edits) or when no preimage was captured.
    pub fn preview(
        &self,
        id: &CheckpointId,
        current_sha256: Option<&str>,
    ) -> Result<RestorePlan, WsError> {
        let entry = self
            .entries
            .get(id.as_str())
            .ok_or_else(|| WsError::CheckpointNotFound(id.to_string()))?;
        let post = entry.post_sha256.as_deref().ok_or_else(|| {
            WsError::Conflict(format!(
                "checkpoint {} has no recorded postimage; cannot prove the file is ours",
                entry.id
            ))
        })?;
        if !entry.existed {
            return match current_sha256 {
                Some(current) if current == post => Ok(RestorePlan::DeleteCreated {
                    path: entry.path.clone(),
                }),
                Some(_) => Err(WsError::Conflict(format!(
                    "{} changed after Bollo created it; refusing to delete user content",
                    entry.path
                ))),
                None => Err(WsError::Conflict(format!(
                    "{} was already removed externally; nothing to restore",
                    entry.path
                ))),
            };
        }
        match current_sha256 {
            Some(current) if current == post => {
                let Some(text) = &entry.pre_text else {
                    return Err(WsError::Conflict(format!(
                        "checkpoint {} has no text preimage (binary or too large); restore unavailable",
                        entry.id
                    )));
                };
                Ok(RestorePlan::WritePreimage {
                    path: entry.path.clone(),
                    text: text.clone(),
                })
            }
            Some(_) => Err(WsError::Conflict(format!(
                "{} was edited after Bollo's change; refusing to overwrite user edits",
                entry.path
            ))),
            None => Err(WsError::Conflict(format!(
                "{} no longer exists; restore requires a matching postimage",
                entry.path
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_only_matching_postimage() {
        let mut log = CheckpointLog::new();
        let checkpoint = log.capture("src/main.rs", Some(b"original"));
        let id = checkpoint.id.clone();
        log.mark_post(&id, b"modified").unwrap();

        // User edited after us: refuse.
        let user_hash = hex::encode(Sha256::digest(b"user edit"));
        assert!(matches!(
            log.preview(&id, Some(&user_hash)),
            Err(WsError::Conflict(_))
        ));

        // Still our postimage: restore the preimage.
        let our_post = hex::encode(Sha256::digest(b"modified"));
        match log.preview(&id, Some(&our_post)).unwrap() {
            RestorePlan::WritePreimage { path, text } => {
                assert_eq!(path, "src/main.rs");
                assert_eq!(text, "original");
            }
            other => panic!("unexpected plan {other:?}"),
        }
    }

    #[test]
    fn new_files_are_deleted_only_when_unchanged() {
        let mut log = CheckpointLog::new();
        let checkpoint = log.capture("created.txt", None);
        let id = checkpoint.id.clone();
        log.mark_post(&id, b"hello").unwrap();
        let our_post = hex::encode(Sha256::digest(b"hello"));
        assert_eq!(
            log.preview(&id, Some(&our_post)).unwrap(),
            RestorePlan::DeleteCreated {
                path: "created.txt".into()
            }
        );
        let changed = hex::encode(Sha256::digest(b"changed"));
        assert!(matches!(
            log.preview(&id, Some(&changed)),
            Err(WsError::Conflict(_))
        ));
    }

    #[test]
    fn binary_or_oversize_preimages_disclose_unavailable_restore() {
        let mut log = CheckpointLog::new();
        let binary = [0u8, 1, 2, 3];
        let checkpoint = log.capture("blob.bin", Some(&binary));
        let id = checkpoint.id.clone();
        log.mark_post(&id, b"x").unwrap();
        let current = hex::encode(Sha256::digest(b"x"));
        assert!(matches!(
            log.preview(&id, Some(&current)),
            Err(WsError::Conflict(_))
        ));
    }

    #[test]
    fn missing_checkpoint_is_reported() {
        let log = CheckpointLog::new();
        assert!(matches!(
            log.preview(&CheckpointId::generate(), None),
            Err(WsError::CheckpointNotFound(_))
        ));
    }
}
