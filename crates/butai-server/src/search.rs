//! Workspace search: fuzzy filename matching (nucleo) plus content grep.
//!
//! Runs off the actor thread, on the blocking pool, because walking a tree and
//! grepping it is exactly the kind of filesystem work that freezes the daemon
//! when the directory is on a share that has gone away.

use std::path::{Path, PathBuf};

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
const MAX_FILES_WALKED: usize = 4000;
const MAX_NAME_HITS: usize = 12;
const MAX_GREP_HITS: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub path: PathBuf,
    pub line: Option<u32>,
    pub preview: String,
}

pub fn run_search(root: &Path, query: &str) -> Vec<SearchHit> {
    let mut hits = fuzzy_files(root, query);
    if query.chars().count() >= 3 {
        hits.extend(grep_content(root, query));
    }
    hits.truncate(MAX_NAME_HITS + MAX_GREP_HITS);
    hits
}

fn fuzzy_files(root: &Path, query: &str) -> Vec<SearchHit> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files, &mut 0);
    if query.trim().is_empty() {
        files.truncate(MAX_NAME_HITS);
        return files
            .into_iter()
            .map(|p| SearchHit { path: p, line: None, preview: String::new() })
            .collect();
    }
    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let names: Vec<String> = files.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    let mut scored = pattern.match_list(names, &mut matcher);
    scored.truncate(MAX_NAME_HITS);
    scored
        .into_iter()
        .map(|(name, _)| SearchHit {
            path: PathBuf::from(name),
            line: None,
            preview: String::new(),
        })
        .collect()
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>, count: &mut usize) {
    if *count >= MAX_FILES_WALKED {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        if *count >= MAX_FILES_WALKED {
            return;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || matches!(name.as_str(), "target" | "node_modules") {
            continue;
        }
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            collect_files(root, &path, out, count);
        } else {
            *count += 1;
            if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_path_buf());
            }
        }
    }
}

/// `grep -rn` content hits as (path, line, text).
fn grep_content(root: &Path, query: &str) -> Vec<SearchHit> {
    let out = std::process::Command::new("grep")
        .args([
            "-rnIF",
            "-m",
            "2",
            "--exclude-dir=.git",
            "--exclude-dir=target",
            "--exclude-dir=node_modules",
            "--",
            query,
            ".",
        ])
        .current_dir(root)
        .output();
    let Ok(out) = out else { return Vec::new() };
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .filter_map(|l| {
            let (path, rest) = l.split_once(':')?;
            let (line, preview) = rest.split_once(':')?;
            Some(SearchHit {
                path: PathBuf::from(path.trim_start_matches("./")),
                line: line.parse().ok(),
                preview: preview.trim().chars().take(60).collect(),
            })
        })
        .take(MAX_GREP_HITS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_files_and_content() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main_module.rs"), "let needle_here = 1;\n").unwrap();
        std::fs::write(dir.path().join("other.txt"), "nothing\n").unwrap();

        let by_name = run_search(dir.path(), "mainmod");
        assert!(by_name.iter().any(|h| h.path.ends_with("main_module.rs")), "{by_name:?}");

        let by_content = run_search(dir.path(), "needle_here");
        assert!(
            by_content.iter().any(|h| h.line == Some(1) && h.preview.contains("needle_here")),
            "{by_content:?}"
        );
    }
}
