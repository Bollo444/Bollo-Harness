//! Intent normalization: workspace-relative paths, the `outside:` namespace,
//! whole-path globs, argv identity and canonical intent hashing.
//!
//! Normalization is lexical and pure. Symlink resolution happens in
//! `bollo-workspace` *before* policy so the decision sees the real target.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use bollo_protocol::vocab::{EffectClass, ToolClass};

use crate::PolicyError;

/// A normalized path, either workspace-relative or in the explicit outside
/// namespace. `display()` is the stable identity used for hashing and globs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "scope")]
pub enum ScopedPath {
    Workspace { relative: String },
    Outside { normalized: String },
}

impl ScopedPath {
    pub fn display(&self) -> String {
        match self {
            ScopedPath::Workspace { relative } => relative.clone(),
            ScopedPath::Outside { normalized } => format!("outside:{normalized}"),
        }
    }

    pub fn is_outside(&self) -> bool {
        matches!(self, ScopedPath::Outside { .. })
    }

    /// Globs match the whole normalized identity.
    pub fn matches_glob(&self, pattern: &str) -> bool {
        glob_matches(pattern, &self.display())
    }
}

/// Normalize a path lexically: separators to `/`, collapse `.`/`..`.
pub fn normalize_lexical(raw: &str) -> Result<String, PolicyError> {
    if raw.trim().is_empty() {
        return Err(PolicyError::Path("empty path".into()));
    }
    if raw.contains('\0') {
        return Err(PolicyError::Path("NUL byte in path".into()));
    }
    let replaced = raw.replace('\\', "/");
    let is_abs = replaced.starts_with('/');
    let mut stack: Vec<&str> = Vec::new();
    for part in replaced.split('/') {
        match part {
            "" | "." => {}
            ".." => match stack.last() {
                Some(&"..") | None => {
                    if !is_abs {
                        stack.push("..");
                    }
                    // An absolute path cannot go above its root: drop.
                }
                Some(_) => {
                    stack.pop();
                }
            },
            other => stack.push(other),
        }
    }
    let joined = stack.join("/");
    Ok(if is_abs {
        format!("/{joined}")
    } else {
        joined
    })
}

fn is_absolute(normalized: &str) -> bool {
    normalized.starts_with('/')
        || (normalized.len() >= 2 && normalized.as_bytes()[1] == b':')
}

fn join(root: &str, relative: &str) -> String {
    if root.ends_with('/') {
        format!("{root}{relative}")
    } else {
        format!("{root}/{relative}")
    }
}

fn within(root: &str, candidate: &str) -> bool {
    let (root, candidate) = if cfg!(windows) {
        (root.to_ascii_lowercase(), candidate.to_ascii_lowercase())
    } else {
        (root.to_string(), candidate.to_string())
    };
    if candidate == root {
        return true;
    }
    let prefix = if root.ends_with('/') {
        root
    } else {
        format!("{root}/")
    };
    candidate.starts_with(&prefix)
}

/// Resolve `raw` against the canonical workspace root.
pub fn scoped_path(root: &str, raw: &str) -> Result<ScopedPath, PolicyError> {
    let root_norm = normalize_lexical(root)?;
    if !is_absolute(&root_norm) {
        return Err(PolicyError::Path(format!(
            "workspace root must be absolute: {root:?}"
        )));
    }
    let raw_norm = normalize_lexical(raw)?;
    let candidate = if is_absolute(&raw_norm) {
        raw_norm
    } else {
        join(&root_norm, &raw_norm)
    };
    let candidate = normalize_lexical(&candidate)?;
    if within(&root_norm, &candidate) {
        let relative = candidate
            .get(root_norm.len()..)
            .unwrap_or("")
            .trim_start_matches('/')
            .to_string();
        Ok(ScopedPath::Workspace {
            relative: if relative.is_empty() {
                ".".to_string()
            } else {
                relative
            },
        })
    } else {
        Ok(ScopedPath::Outside {
            normalized: candidate,
        })
    }
}

/// Whole-path glob matching: `*` never crosses `/`, `**` matches across
/// segments (including zero), `?` matches one character.
pub fn glob_matches(pattern: &str, path: &str) -> bool {
    let (pattern, path) = if cfg!(windows) {
        (pattern.to_ascii_lowercase(), path.to_ascii_lowercase())
    } else {
        (pattern.to_string(), path.to_string())
    };
    let pat: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    let seg: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    match_segments(&pat, &seg)
}

fn match_segments(pat: &[&str], seg: &[&str]) -> bool {
    if pat.is_empty() {
        return seg.is_empty();
    }
    if pat[0] == "**" {
        if match_segments(&pat[1..], seg) {
            return true;
        }
        if !seg.is_empty() {
            return match_segments(pat, &seg[1..]);
        }
        return false;
    }
    if seg.is_empty() {
        return false;
    }
    if !match_segment(pat[0], seg[0]) {
        return false;
    }
    match_segments(&pat[1..], &seg[1..])
}

fn match_segment(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut mark = 0usize;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            pi += 1;
            mark = ti;
        } else if let Some(sp) = star {
            pi = sp + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// The normalized description of a proposed action. Everything policy sees is
/// in this structure; nothing else may influence the decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedIntent {
    pub tool: String,
    pub class: ToolClass,
    pub effect: EffectClass,
    pub paths: Vec<ScopedPath>,
    pub argv: Option<Vec<String>>,
    pub cwd: Option<ScopedPath>,
    pub environment_names: Vec<String>,
    pub summary: String,
    /// Canonical digest of the exact prepared arguments, computed by the tool
    /// layer. A changed argument changes this digest and therefore the intent
    /// hash an approval is bound to.
    pub arguments_digest: String,
}

impl NormalizedIntent {
    /// Canonical form: sorted paths/environment for a stable hash.
    pub fn canonical(&self) -> NormalizedIntent {
        let mut clone = self.clone();
        clone.paths.sort_by_key(|p| p.display());
        clone.environment_names.sort();
        clone
    }
}

/// Map a tool class and path scope to the normalized effect category.
pub fn effect_class_for(class: ToolClass, any_outside: bool) -> EffectClass {
    match class {
        ToolClass::Read | ToolClass::Search | ToolClass::Git => {
            if any_outside {
                EffectClass::OutsideWorkspace
            } else {
                EffectClass::Read
            }
        }
        ToolClass::Write => {
            if any_outside {
                EffectClass::OutsideWorkspace
            } else {
                EffectClass::WorkspaceMutation
            }
        }
        ToolClass::Exec => EffectClass::Execution,
        ToolClass::Mcp => EffectClass::ExternalMutation,
    }
}

/// SHA-256 over the canonical serialization: the approval identity.
pub fn intent_hash(intent: &NormalizedIntent) -> String {
    let canonical = intent.canonical();
    let bytes = serde_json::to_vec(&canonical).expect("NormalizedIntent is always serializable");
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> &'static str {
        if cfg!(windows) {
            "C:/proj"
        } else {
            "/proj"
        }
    }

    #[test]
    fn glob_star_does_not_cross_slash() {
        assert!(glob_matches("src/*.rs", "src/main.rs"));
        assert!(!glob_matches("src/*.rs", "src/nested/main.rs"));
        assert!(glob_matches("src/**/*.rs", "src/nested/deep/main.rs"));
        assert!(glob_matches("src/**", "src/nested/deep/main.rs"));
    }

    #[test]
    fn glob_double_star_matches_at_any_depth_including_root() {
        assert!(glob_matches("**/.env", ".env"));
        assert!(glob_matches("**/.env", "config/.env"));
        assert!(glob_matches("**/.env", "a/b/c/.env"));
        assert!(glob_matches("**/*.pem", "keys/server.pem"));
        assert!(glob_matches("dir/**", "dir/a.txt"));
    }

    #[test]
    fn glob_question_mark_and_literals() {
        assert!(glob_matches("src/?.rs", "src/a.rs"));
        assert!(!glob_matches("src/?.rs", "src/ab.rs"));
        assert!(glob_matches("src/main.rs", "src/main.rs"));
        assert!(!glob_matches("src/main.rs", "src/other.rs"));
    }

    #[test]
    fn scoped_path_resolves_inside_and_outside() {
        let inside = scoped_path(root(), "src/../src/main.rs").unwrap();
        assert_eq!(inside.display(), "src/main.rs");
        let outside = scoped_path(root(), "../secrets.env").unwrap();
        assert!(outside.is_outside());
        assert!(outside.display().starts_with("outside:"));
        let absolute_inside = scoped_path(root(), &format!("{}/src/main.rs", root())).unwrap();
        assert_eq!(absolute_inside.display(), "src/main.rs");
        let absolute_outside = scoped_path(root(), "/etc/passwd");
        if cfg!(windows) {
            // "/etc/passwd" normalizes to a drive-less absolute path that is not
            // under C:/proj, so it is still outside.
            assert!(absolute_outside.unwrap().is_outside());
        } else {
            assert!(absolute_outside.unwrap().is_outside());
        }
    }

    #[test]
    fn rejects_nul_and_empty_paths() {
        assert!(scoped_path(root(), "bad\0name").is_err());
        assert!(scoped_path(root(), "  ").is_err());
    }

    #[test]
    fn intent_hash_is_deterministic_and_order_insensitive() {
        let mut a = NormalizedIntent {
            tool: "read_file".into(),
            class: ToolClass::Read,
            effect: EffectClass::Read,
            paths: vec![
                scoped_path(root(), "b.txt").unwrap(),
                scoped_path(root(), "a.txt").unwrap(),
            ],
            argv: None,
            cwd: None,
            environment_names: vec!["B".into(), "A".into()],
            summary: "read two files".into(),
            arguments_digest: "0".repeat(64),
        };
        let b = a.clone();
        assert_eq!(intent_hash(&a), intent_hash(&b));
        a.paths.reverse();
        a.environment_names.reverse();
        assert_eq!(intent_hash(&a), intent_hash(&b));
    }

    #[test]
    fn effect_class_mapping() {
        assert_eq!(
            effect_class_for(ToolClass::Read, false),
            EffectClass::Read
        );
        assert_eq!(
            effect_class_for(ToolClass::Write, true),
            EffectClass::OutsideWorkspace
        );
        assert_eq!(
            effect_class_for(ToolClass::Exec, false),
            EffectClass::Execution
        );
        assert_eq!(
            effect_class_for(ToolClass::Mcp, false),
            EffectClass::ExternalMutation
        );
    }
}
