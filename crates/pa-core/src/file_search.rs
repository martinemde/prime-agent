//! The `@` file-completion walk run on the daemon's machine, for a client that does not share
//! its filesystem. It follows the client's local walk in `pa-tui`'s `fuzzy_file_search` (fd's
//! rules: hidden entries included, links followed, `.git` pruned, the default ignore files, at
//! most [`MAX_RESULTS`] matches); the client ranks what comes back exactly as it ranks a local
//! walk. Keep the two in step.

use std::path::Path;
use std::sync::{Mutex, PoisonError};

use ignore::{WalkBuilder, WalkState};

/// fd's `--max-results`: the walk stops once this many entries match.
pub const MAX_RESULTS: usize = 100;

/// How a walk entry matches: an fd-style regex against the entry's name, or against its
/// root-joined path in full-path mode. No pattern matches every entry.
#[derive(Debug, Clone, Copy, Default)]
pub struct FileMatch<'a> {
    pub pattern: Option<&'a str>,
    pub case_insensitive: bool,
    pub full_path: bool,
}

/// Walk `root` and return up to [`MAX_RESULTS`] matching entries relative to it, `/`-separated,
/// directories carrying a trailing `/`.
///
/// # Errors
/// Returns an error when the pattern is not a valid regex.
pub fn walk_matching(root: &Path, file_match: FileMatch<'_>) -> Result<Vec<String>, String> {
    let regex = file_match
        .pattern
        .filter(|pattern| !pattern.is_empty())
        .map(|pattern| {
            fancy_regex::RegexBuilder::new(pattern)
                .case_insensitive(file_match.case_insensitive)
                .build()
                .map_err(|error| format!("Invalid file search pattern: {error}"))
        })
        .transpose()?;
    let found: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let mut builder = WalkBuilder::new(root);
    builder.hidden(false);
    builder.follow_links(true);
    builder.filter_entry(|entry| entry.file_name() != ".git");
    builder.build_parallel().run(|| {
        let (found, regex) = (&found, regex.as_ref());
        Box::new(move |entry| {
            let Ok(entry) = entry else {
                return WalkState::Continue;
            };
            let Some(file_type) = entry.file_type() else {
                return WalkState::Continue;
            };
            if entry.depth() == 0 || !(file_type.is_file() || file_type.is_dir()) {
                return WalkState::Continue;
            }
            if let Some(regex) = regex {
                let target = if file_match.full_path {
                    entry.path().to_string_lossy()
                } else {
                    entry.file_name().to_string_lossy()
                };
                if !regex.is_match(&target).unwrap_or(false) {
                    return WalkState::Continue;
                }
            }
            let relative = entry.path().strip_prefix(root).unwrap_or(entry.path());
            let mut form = relative.to_string_lossy().replace('\\', "/");
            if file_type.is_dir() {
                form.push('/');
            }
            let mut found = found.lock().unwrap_or_else(PoisonError::into_inner);
            if found.len() == MAX_RESULTS {
                return WalkState::Quit;
            }
            found.push(form);
            WalkState::Continue
        })
    });
    Ok(found.into_inner().unwrap_or_else(PoisonError::into_inner))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_walk_matches_names_or_full_paths_and_marks_directories() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/nested")).unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "").unwrap();
        std::fs::write(dir.path().join("src/nested/Main.txt"), "").unwrap();
        std::fs::write(dir.path().join(".git/main"), "").unwrap();
        let walk = |file_match| {
            let mut found = walk_matching(dir.path(), file_match).unwrap();
            found.sort();
            found
        };
        let by_name = FileMatch {
            pattern: Some("main"),
            case_insensitive: true,
            full_path: false,
        };
        let by_path = FileMatch {
            pattern: Some("src[\\\\/]nested[\\\\/]"),
            case_insensitive: true,
            full_path: true,
        };
        assert_eq!(
            [walk(by_name), walk(by_path), walk(FileMatch::default())],
            [
                vec!["src/main.rs".to_string(), "src/nested/Main.txt".to_string()],
                vec!["src/nested/Main.txt".to_string()],
                vec![
                    "src/".to_string(),
                    "src/main.rs".to_string(),
                    "src/nested/".to_string(),
                    "src/nested/Main.txt".to_string(),
                ],
            ]
        );
    }
}
