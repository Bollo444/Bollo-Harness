//! Explicit executable trust. Project configuration can request an extension
//! but cannot create a trust record; trust is a host-side user action bound to
//! a resolved executable, exact argv, cwd and environment names.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use bollo_protocol::timeutil;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustRecord {
    /// Resolved absolute program path.
    pub program: String,
    pub executable_sha256: String,
    pub argv: Vec<String>,
    pub cwd: String,
    pub env_names: Vec<String>,
    pub granted_at: String,
}

/// In-memory trust store. The host owns it; it is never populated from project
/// content, model output or MCP results.
#[derive(Debug, Default)]
pub struct TrustStore {
    records: Vec<TrustRecord>,
}

impl TrustStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Grant trust for a resolved program identity. Fails when the program
    /// cannot be resolved or read.
    pub fn grant(
        &mut self,
        argv: &[String],
        cwd: &Path,
        env_names: &[String],
    ) -> Result<TrustRecord, String> {
        let program = resolve_program(argv.first().ok_or("empty argv")?)
            .ok_or_else(|| format!("cannot resolve executable {:?}", argv.first()))?;
        let bytes = std::fs::read(&program).map_err(|err| err.to_string())?;
        let record = TrustRecord {
            program: program.to_string_lossy().replace('\\', "/"),
            executable_sha256: hex::encode(Sha256::digest(&bytes)),
            argv: argv.to_vec(),
            cwd: cwd.to_string_lossy().replace('\\', "/"),
            env_names: env_names.to_vec(),
            granted_at: timeutil::now_rfc3339(),
        };
        self.records.push(record.clone());
        Ok(record)
    }

    /// Verify an exact extension identity. Any change (argv, cwd, env names,
    /// resolved executable bytes) invalidates the record.
    pub fn verify(&self, argv: &[String], cwd: &Path, env_names: &[String]) -> bool {
        let Some(program) = argv.first().and_then(|p| resolve_program(p)) else {
            return false;
        };
        let program_display = program.to_string_lossy().replace('\\', "/");
        let cwd_display = cwd.to_string_lossy().replace('\\', "/");
        let Ok(bytes) = std::fs::read(&program) else {
            return false;
        };
        let hash = hex::encode(Sha256::digest(&bytes));
        self.records.iter().any(|record| {
            record.program == program_display
                && record.executable_sha256 == hash
                && record.argv == argv
                && record.cwd == cwd_display
                && record.env_names == env_names
        })
    }
}

/// Resolve a program: absolute paths are checked directly, bare names are
/// searched on a trusted PATH. Returns `None` when nothing executable is found.
pub fn resolve_program(program: &str) -> Option<PathBuf> {
    let candidate = Path::new(program);
    if candidate.is_absolute() {
        return candidate.is_file().then(|| candidate.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        let full = directory.join(candidate);
        if full.is_file() {
            return Some(full);
        }
        #[cfg(windows)]
        {
            for extension in ["exe", "cmd", "bat"] {
                let with_extension = directory.join(format!("{program}.{extension}"));
                if with_extension.is_file() {
                    return Some(with_extension);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn current_exe() -> String {
        std::env::current_exe().unwrap().to_string_lossy().into_owned()
    }

    #[test]
    fn trust_binds_exact_identity() {
        let cwd = std::env::current_dir().unwrap();
        let argv = vec![current_exe(), "--version".to_string()];
        let mut store = TrustStore::new();
        assert!(!store.verify(&argv, &cwd, &[]));
        store.grant(&argv, &cwd, &[]).unwrap();
        assert!(store.verify(&argv, &cwd, &[]));
        // Changed argv or env names invalidate.
        let changed = vec![current_exe(), "--help".to_string()];
        assert!(!store.verify(&changed, &cwd, &[]));
        assert!(!store.verify(&argv, &cwd, &["SECRET".to_string()]));
        let other_cwd = tempfile::tempdir().unwrap();
        assert!(!store.verify(&argv, other_cwd.path(), &[]));
    }

    #[test]
    fn unresolvable_programs_are_rejected() {
        let mut store = TrustStore::new();
        assert!(store
            .grant(&["definitely-not-a-program-xyz".to_string()], Path::new("."), &[])
            .is_err());
        assert!(!store.verify(
            &["definitely-not-a-program-xyz".to_string()],
            Path::new("."),
            &[]
        ));
    }

    #[test]
    fn resolve_program_finds_path_binaries() {
        #[cfg(windows)]
        let found = resolve_program("cmd");
        #[cfg(unix)]
        let found = resolve_program("sh");
        assert!(found.is_some());
    }
}
