//! Handle-relative filesystem operations. Every path is resolved against the
//! canonical root, symlinks are resolved *before* policy sees the target, and
//! writes are atomic replacements guarded by preimage hashes.

use std::io::Write;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::root::{lexical_normalize, policy_string, WorkspaceRoot};
use crate::WsError;

pub const MAX_READ_BYTES: u64 = 1_048_576;
pub const MAX_WRITE_BYTES: u64 = 1_048_576;
pub const DEFAULT_LIMIT_LINES: u64 = 200;
pub const MAX_LIMIT_LINES: u64 = 2_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRead {
    pub text: String,
    pub start_line: u64,
    pub end_line: u64,
    pub total_lines: u64,
    pub sha256: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileWrite {
    pub pre_sha256: Option<String>,
    pub post_sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePatch {
    pub pre_sha256: String,
    pub post_sha256: String,
    pub replaced_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct WorkspaceFs {
    root: PathBuf,
}

impl WorkspaceFs {
    pub fn new(root: &WorkspaceRoot) -> Self {
        Self {
            root: root.canonical().to_path_buf(),
        }
    }

    /// Root as a policy string.
    pub fn policy_root(&self) -> String {
        policy_string(&self.root)
    }

    /// Resolve a caller path to a real absolute path inside the workspace.
    /// Rejects traversal, NUL and symlink escapes.
    pub fn resolve(&self, raw: &str) -> Result<PathBuf, WsError> {
        let real = self.resolve_relaxed(raw)?;
        if !real.starts_with(&self.root) {
            return Err(WsError::OutsideScope(policy_string(&real)));
        }
        Ok(real)
    }

    /// Symlink-resolving path resolution that returns outside targets instead
    /// of refusing them. Prepare uses this so policy can see the real target
    /// and deny it; execution still goes through [`WorkspaceFs::resolve`].
    pub fn resolve_relaxed(&self, raw: &str) -> Result<PathBuf, WsError> {
        if raw.contains('\0') || raw.trim().is_empty() {
            return Err(WsError::InvalidPath(raw.to_string()));
        }
        let candidate = Path::new(raw);
        let joined = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            self.root.join(candidate)
        };
        let normalized = lexical_normalize(&joined);
        canonicalize_existing(&normalized)
    }

    /// Read text with line slicing and a 1 MiB capture cap.
    pub fn read_text(
        &self,
        path: &str,
        offset: Option<u64>,
        limit: Option<u64>,
    ) -> Result<FileRead, WsError> {
        let absolute = self.resolve(path)?;
        let metadata = std::fs::metadata(&absolute)
            .map_err(|err| WsError::Io(format!("{}: {err}", absolute.display())))?;
        if !metadata.is_file() {
            return Err(WsError::SpecialFile(absolute.display().to_string()));
        }
        let bytes = std::fs::read(&absolute)
            .map_err(|err| WsError::Io(format!("{}: {err}", absolute.display())))?;
        if bytes.contains(&0) {
            return Err(WsError::BinaryContent);
        }
        let sha256 = hex::encode(Sha256::digest(&bytes));
        let truncated_capture = bytes.len() as u64 > MAX_READ_BYTES;
        let captured = if truncated_capture {
            &bytes[..MAX_READ_BYTES as usize]
        } else {
            &bytes[..]
        };
        let text = String::from_utf8_lossy(captured).into_owned();
        let lines: Vec<&str> = text.lines().collect();
        let total_lines = lines.len() as u64;
        let start = offset.unwrap_or(1).max(1);
        let count = limit.unwrap_or(DEFAULT_LIMIT_LINES).min(MAX_LIMIT_LINES);
        let slice_start = (start - 1).min(total_lines);
        let slice_end = (slice_start + count).min(total_lines);
        let text_slice = lines[slice_start as usize..slice_end as usize].join("\n");
        Ok(FileRead {
            text: text_slice,
            start_line: if total_lines == 0 { 0 } else { slice_start + 1 },
            end_line: slice_end,
            total_lines,
            sha256,
            truncated: truncated_capture || slice_end < total_lines,
        })
    }

    /// Atomic replacement. `expected` semantics: `Some(hash)` requires the
    /// current content to match; `None` requires the file not to exist.
    pub fn write_text(
        &self,
        path: &str,
        content: &str,
        expected: Option<&str>,
    ) -> Result<FileWrite, WsError> {
        if content.len() as u64 > MAX_WRITE_BYTES {
            return Err(WsError::TooLarge(content.len() as u64));
        }
        let absolute = self.resolve(path)?;
        let current = std::fs::read(&absolute).ok();
        let current_hash = current.as_ref().map(|bytes| hex::encode(Sha256::digest(bytes)));
        match (expected, current_hash.as_deref()) {
            (Some(expected), Some(actual)) if expected == actual => {}
            (Some(expected), Some(actual)) => {
                return Err(WsError::PreimageConflict(format!(
                    "expected {expected}, found {actual}"
                )))
            }
            (Some(_), None) => {
                return Err(WsError::PreimageConflict(
                    "expected an existing file with a matching hash; it does not exist".into(),
                ))
            }
            (None, Some(_)) => {
                return Err(WsError::PreimageConflict(
                    "expected_sha256 null requires the file not to exist".into(),
                ))
            }
            (None, None) => {}
        }
        write_atomic(&absolute, content.as_bytes())?;
        let post_sha256 = hex::encode(Sha256::digest(content.as_bytes()));
        Ok(FileWrite {
            pre_sha256: current_hash,
            post_sha256,
            bytes: content.len() as u64,
        })
    }

    /// Exact unique replacement; refuses stale preimages and ambiguous matches.
    pub fn patch_text(
        &self,
        path: &str,
        old_text: &str,
        new_text: &str,
        expected_sha256: &str,
    ) -> Result<FilePatch, WsError> {
        if old_text.is_empty() {
            return Err(WsError::InvalidPath("old_text must not be empty".into()));
        }
        if new_text.len() as u64 > MAX_WRITE_BYTES {
            return Err(WsError::TooLarge(new_text.len() as u64));
        }
        let absolute = self.resolve(path)?;
        let bytes = std::fs::read(&absolute)
            .map_err(|err| WsError::Io(format!("{}: {err}", absolute.display())))?;
        let current_hash = hex::encode(Sha256::digest(&bytes));
        if current_hash != expected_sha256 {
            return Err(WsError::PreimageConflict(format!(
                "expected {expected_sha256}, found {current_hash}"
            )));
        }
        let text = String::from_utf8_lossy(&bytes);
        let occurrences = text.matches(old_text).count();
        match occurrences {
            0 => {
                return Err(WsError::AmbiguousMatch(
                    "old_text does not appear in the file".into(),
                ))
            }
            1 => {}
            n => {
                return Err(WsError::AmbiguousMatch(format!(
                    "old_text appears {n} times; provide a unique fragment"
                )))
            }
        }
        let replaced = text.replacen(old_text, new_text, 1);
        write_atomic(&absolute, replaced.as_bytes())?;
        Ok(FilePatch {
            pre_sha256: current_hash,
            post_sha256: hex::encode(Sha256::digest(replaced.as_bytes())),
            replaced_bytes: old_text.len() as u64,
        })
    }

    /// Conditional deletion used by checkpoint restore: the file is removed
    /// only when its current hash matches the recorded postimage, so a user
    /// edit made after Bollo's change is never deleted.
    pub fn remove_file(&self, path: &str, expected_sha256: &str) -> Result<(), WsError> {
        let absolute = self.resolve(path)?;
        let bytes = std::fs::read(&absolute)
            .map_err(|err| WsError::Io(format!("{}: {err}", absolute.display())))?;
        let current = hex::encode(Sha256::digest(&bytes));
        if current != expected_sha256 {
            return Err(WsError::PreimageConflict(format!(
                "expected {expected_sha256}, found {current}"
            )));
        }
        std::fs::remove_file(&absolute)
            .map_err(|err| WsError::Io(format!("{}: {err}", absolute.display())))
    }

    /// Current content hash, if the file exists and is a regular file.
    pub fn current_sha256(&self, path: &str) -> Option<String> {
        let absolute = self.resolve(path).ok()?;
        let bytes = std::fs::read(absolute).ok()?;
        Some(hex::encode(Sha256::digest(&bytes)))
    }
}

fn canonicalize_existing(path: &Path) -> Result<PathBuf, WsError> {
    if path.exists() {
        return std::fs::canonicalize(path)
            .map_err(|err| WsError::Io(format!("{}: {err}", path.display())));
    }
    let mut suffix: Vec<std::ffi::OsString> = Vec::new();
    let mut current = path.to_path_buf();
    loop {
        if let Some(name) = current.file_name() {
            suffix.push(name.to_os_string());
        }
        current = current
            .parent()
            .ok_or_else(|| WsError::InvalidPath(path.display().to_string()))?
            .to_path_buf();
        if current.exists() {
            let base = std::fs::canonicalize(&current)
                .map_err(|err| WsError::Io(format!("{}: {err}", current.display())))?;
            let mut resolved = base;
            for part in suffix.iter().rev() {
                resolved.push(part);
            }
            return Ok(resolved);
        }
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), WsError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| WsError::Io(format!("{}: {err}", parent.display())))?;
    }
    let temp = path.with_extension(format!("bollo-tmp-{}", std::process::id()));
    {
        let mut file = std::fs::File::create(&temp)
            .map_err(|err| WsError::Io(format!("{}: {err}", temp.display())))?;
        file.write_all(bytes)
            .map_err(|err| WsError::Io(format!("{}: {err}", temp.display())))?;
        file.sync_all()
            .map_err(|err| WsError::Io(format!("{}: {err}", temp.display())))?;
    }
    if let Err(err) = rename_replace(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(err);
    }
    Ok(())
}

fn rename_replace(from: &Path, to: &Path) -> Result<(), WsError> {
    // std::fs::rename replaces on POSIX; on Windows it fails when the target
    // exists, so remove first as the documented atomic-replace fallback.
    if cfg!(windows) && to.exists() {
        std::fs::remove_file(to).map_err(|err| WsError::Io(err.to_string()))?;
    }
    std::fs::rename(from, to).map_err(|err| WsError::Io(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, WorkspaceFs) {
        let dir = tempfile::tempdir().unwrap();
        let root = WorkspaceRoot::discover(dir.path()).unwrap();
        let fs = WorkspaceFs::new(&root);
        (dir, fs)
    }

    #[test]
    fn write_read_roundtrip_with_hashes() {
        let (_dir, fs) = fixture();
        let write = fs
            .write_text("src/main.rs", "fn main() {}\n", None)
            .unwrap();
        assert!(write.pre_sha256.is_none());
        let read = fs.read_text("src/main.rs", None, None).unwrap();
        assert_eq!(read.text, "fn main() {}");
        assert_eq!(read.sha256, write.post_sha256);
        assert_eq!(read.total_lines, 1);
        assert!(!read.truncated);
    }

    #[test]
    fn write_requires_matching_preimage() {
        let (_dir, fs) = fixture();
        let write = fs.write_text("a.txt", "one", None).unwrap();
        // Wrong hash is refused.
        assert!(matches!(
            fs.write_text("a.txt", "two", Some(&"0".repeat(64))),
            Err(WsError::PreimageConflict(_))
        ));
        // Correct hash replaces.
        fs.write_text("a.txt", "two", Some(&write.post_sha256)).unwrap();
        // Null expected means "must not exist".
        assert!(matches!(
            fs.write_text("a.txt", "three", None),
            Err(WsError::PreimageConflict(_))
        ));
    }

    #[test]
    fn patch_requires_unique_match_and_matching_hash() {
        let (_dir, fs) = fixture();
        let write = fs
            .write_text("a.txt", "alpha beta gamma", None)
            .unwrap();
        assert!(matches!(
            fs.patch_text("a.txt", "missing", "x", &write.post_sha256),
            Err(WsError::AmbiguousMatch(_))
        ));
        assert!(matches!(
            fs.patch_text("a.txt", "alpha", "x", &"0".repeat(64)),
            Err(WsError::PreimageConflict(_))
        ));
        let patch = fs
            .patch_text("a.txt", "beta", "BETA", &write.post_sha256)
            .unwrap();
        assert_eq!(fs.read_text("a.txt", None, None).unwrap().text, "alpha BETA gamma");
        assert_eq!(patch.pre_sha256, write.post_sha256);
    }

    #[test]
    fn duplicate_fragments_are_ambiguous() {
        let (_dir, fs) = fixture();
        let write = fs.write_text("a.txt", "x x", None).unwrap();
        assert!(matches!(
            fs.patch_text("a.txt", "x", "y", &write.post_sha256),
            Err(WsError::AmbiguousMatch(_))
        ));
    }

    #[test]
    fn traversal_and_outside_paths_are_rejected() {
        let (dir, fs) = fixture();
        let outside = dir.path().parent().unwrap().join("escape.txt");
        let raw = format!("../{}", outside.file_name().unwrap().to_string_lossy());
        assert!(matches!(fs.resolve(&raw), Err(WsError::OutsideScope(_))));
        assert!(fs.resolve("bad\0name").is_err());
    }

    #[test]
    fn binary_and_special_files_are_rejected() {
        let (dir, fs) = fixture();
        std::fs::write(dir.path().join("blob.bin"), [0u8, 1, 2]).unwrap();
        assert!(matches!(fs.read_text("blob.bin", None, None), Err(WsError::BinaryContent)));
        assert!(matches!(
            fs.read_text(".", None, None),
            Err(WsError::SpecialFile(_))
        ));
    }

    #[test]
    fn reads_are_line_sliced_and_bounded() {
        let (_dir, fs) = fixture();
        let content = (1..=500)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs.write_text("big.txt", &content, None).unwrap();
        let read = fs.read_text("big.txt", Some(10), Some(5)).unwrap();
        assert_eq!(read.start_line, 10);
        assert_eq!(read.end_line, 14);
        assert_eq!(read.total_lines, 500);
        assert!(read.truncated);
        assert!(read.text.starts_with("line 10"));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_rejected() {
        let (dir, fs) = fixture();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        assert!(matches!(
            fs.resolve("link/secret.txt"),
            Err(WsError::OutsideScope(_))
        ));
    }

    #[test]
    fn conditional_delete_refuses_edited_content() {
        let (_dir, fs) = fixture();
        let write = fs.write_text("created.txt", "ours", None).unwrap();
        std::fs::write(std::path::Path::new(&fs.policy_root()).join("created.txt"), "user edit")
            .unwrap();
        assert!(matches!(
            fs.remove_file("created.txt", &write.post_sha256),
            Err(WsError::PreimageConflict(_))
        ));
        assert_eq!(fs.read_text("created.txt", None, None).unwrap().text, "user edit");
        // Restoring our own postimage hash still works.
        let updated = fs.write_text("created.txt", "ours", Some(&fs.current_sha256("created.txt").unwrap())).unwrap();
        fs.remove_file("created.txt", &updated.post_sha256).unwrap();
        assert!(!std::path::Path::new(&fs.policy_root()).join("created.txt").exists());
    }
}
