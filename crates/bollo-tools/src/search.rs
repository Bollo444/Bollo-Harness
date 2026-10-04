//! Bounded workspace search: root-scoped, no subprocess preprocessors, with
//! explicit caps on files, bytes and results. Truncation is a result flag.

use std::path::{Path, PathBuf};

use regex::Regex;

use bollo_protocol::errors::ErrorCode;

use crate::{SearchMatch, ToolError};

pub const MAX_FILES: usize = 10_000;
pub const MAX_TOTAL_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_FILE_BYTES: u64 = 256 * 1024;
pub const PRUNE_DIRS: [&str; 4] = [".git", "node_modules", "target", ".venv"];

pub struct SearchResult {
    pub matches: Vec<SearchMatch>,
    pub truncated: bool,
    pub files_scanned: usize,
}

pub fn search(
    root: &Path,
    display_root: &str,
    pattern: &str,
    max_results: usize,
) -> Result<SearchResult, ToolError> {
    let regex = Regex::new(pattern)
        .map_err(|err| ToolError::new(ErrorCode::InvalidToolArguments, format!("invalid pattern: {err}")))?;
    let mut pending: Vec<PathBuf> = vec![root.to_path_buf()];
    let mut matches: Vec<SearchMatch> = Vec::new();
    let mut files_scanned = 0usize;
    let mut bytes_scanned = 0u64;
    let mut truncated = false;

    'outer: while let Some(directory) = pending.pop() {
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            if metadata.is_dir() {
                if PRUNE_DIRS.contains(&name.as_str()) {
                    continue;
                }
                pending.push(path);
                continue;
            }
            if !metadata.is_file() {
                continue;
            }
            files_scanned += 1;
            if files_scanned > MAX_FILES {
                truncated = true;
                break 'outer;
            }
            if metadata.len() > MAX_FILE_BYTES {
                continue;
            }
            if bytes_scanned + metadata.len() > MAX_TOTAL_BYTES {
                truncated = true;
                break 'outer;
            }
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(_) => continue,
            };
            if bytes.contains(&0) {
                continue;
            }
            bytes_scanned += bytes.len() as u64;
            let text = String::from_utf8_lossy(&bytes);
            let display = relative_display(root, &path, display_root);
            for (index, line) in text.lines().enumerate() {
                if regex.is_match(line) {
                    matches.push(SearchMatch {
                        path: display.clone(),
                        line: (index + 1) as u64,
                        text: line.chars().take(4_096).collect(),
                    });
                    if matches.len() >= max_results {
                        truncated = true;
                        break 'outer;
                    }
                }
            }
        }
    }
    Ok(SearchResult {
        matches,
        truncated,
        files_scanned,
    })
}

fn relative_display(root: &Path, path: &Path, display_root: &str) -> String {
    let relative = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    if display_root == "." || display_root.is_empty() {
        relative
    } else {
        format!("{}/{}", display_root.trim_end_matches('/'), relative)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SearchData;

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\nlet secret = 1;\n").unwrap();
        std::fs::write(dir.path().join("README.md"), "# hello\n").unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".git/config"), "secret = hidden\n").unwrap();
        dir
    }

    #[test]
    fn finds_matches_and_prunes_vcs_dirs() {
        let dir = fixture();
        let result = search(dir.path(), ".", "secret", 100).unwrap();
        assert_eq!(result.matches.len(), 1);
        assert!(result.matches[0].path.starts_with("src/"));
    }

    #[test]
    fn invalid_pattern_is_a_clear_error() {
        let dir = fixture();
        assert!(search(dir.path(), ".", "(", 10).is_err());
    }

    #[test]
    fn result_cap_marks_truncation() {
        let dir = fixture();
        let result = search(dir.path(), ".", ".", 1).unwrap();
        assert_eq!(result.matches.len(), 1);
        assert!(result.truncated);
    }

    #[test]
    fn binary_files_are_skipped() {
        let dir = fixture();
        std::fs::write(dir.path().join("blob.bin"), [0u8, 1, 2, 3]).unwrap();
        let result = search(dir.path(), ".", "secret", 100).unwrap();
        assert_eq!(result.matches.len(), 1);
    }

    #[test]
    fn exposes_typed_data_for_the_model() {
        let dir = fixture();
        let result = search(dir.path(), ".", "hello", 10).unwrap();
        let data = SearchData {
            matches: result.matches,
            count: 1,
            truncated: result.truncated,
        };
        assert_eq!(data.matches[0].text, "# hello");
    }
}
