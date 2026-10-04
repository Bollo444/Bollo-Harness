//! Canonical workspace discovery. Discovery reads metadata only; it never
//! executes project code, sources shells or loads `.env` files.

use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::WsError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRoot {
    canonical: PathBuf,
    identity_hash: String,
}

impl WorkspaceRoot {
    pub fn discover(path: &Path) -> Result<Self, WsError> {
        let canonical = std::fs::canonicalize(path)
            .map_err(|err| WsError::Io(format!("cannot resolve {}: {err}", path.display())))?;
        if !canonical.is_dir() {
            return Err(WsError::InvalidPath(format!(
                "{} is not a directory",
                canonical.display()
            )));
        }
        let identity_hash = compute_identity(&canonical);
        Ok(Self {
            canonical,
            identity_hash,
        })
    }

    pub fn canonical(&self) -> &Path {
        &self.canonical
    }

    /// Policy-facing path string: forward slashes, no Windows verbatim prefix.
    pub fn policy_root(&self) -> String {
        policy_string(&self.canonical)
    }

    /// Identity binds the resolved root and the repository identity record
    /// (currently the git HEAD ref, read as a file; a replaced checkout changes
    /// it and invalidates stored trust).
    pub fn identity_hash(&self) -> &str {
        &self.identity_hash
    }
}

fn compute_identity(root: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(policy_string(root).as_bytes());
    if let Ok(head) = std::fs::read_to_string(root.join(".git").join("HEAD")) {
        hasher.update(head.trim().as_bytes());
    }
    hex::encode(hasher.finalize())
}

/// Convert a path to a policy string: forward slashes, verbatim prefix removed.
pub fn policy_string(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    text.strip_prefix("//?/").unwrap_or(&text).to_string()
}

/// Lexically normalize `.`/`..` without touching the filesystem.
pub fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push("/"),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(part) => out.push(part),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_current_directory_without_executing_anything() {
        let root = WorkspaceRoot::discover(Path::new(".")).unwrap();
        assert!(root.canonical().is_dir());
        assert!(!root.policy_root().contains('\\'));
        assert_eq!(root.identity_hash().len(), 64);
    }

    #[test]
    fn rejects_missing_and_file_paths() {
        assert!(WorkspaceRoot::discover(Path::new("definitely-not-here-12345")).is_err());
        let file = std::env::current_exe().unwrap();
        assert!(WorkspaceRoot::discover(&file).is_err());
    }

    #[test]
    fn lexical_normalize_collapses_dots() {
        let normalized = lexical_normalize(Path::new("a/b/../c/./d"));
        assert_eq!(normalized, PathBuf::from("a/c/d"));
    }
}
