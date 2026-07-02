//! `sym etc` -- the non-symbol answer surface (#704).
//!
//! find-content (#707) is an exact, line-accurate content search over watched
//! repos, built on the ripgrep engine crates (grep-searcher + grep-regex +
//! ignore). It is a direct disk scan at query time, deliberately NOT an
//! index: a tokenized index returns nothing on punctuation-heavy literals
//! (`<<<<<<<`, `--spacing-0.5`), and any content corpus goes stale on git
//! operations that fire no edit hooks. Scanning the working tree gives grep
//! parity and freshness by construction.

use std::path::{Path, PathBuf};

use grep_regex::RegexMatcherBuilder;
use grep_searcher::sinks::Lossy;
use grep_searcher::{BinaryDetection, SearcherBuilder};
use ignore::WalkBuilder;
use serde::Serialize;

use crate::error::{LegionError, Result};

/// Files larger than this are skipped and counted: lockfiles and generated
/// blobs flood output without answering orientation queries.
pub const MAX_FILE_SIZE: u64 = 2 * 1024 * 1024;

/// Hard cap on returned hits; the CLI reports the suppressed count so
/// truncation is never silent.
pub const MAX_HITS: usize = 500;

/// One matching line.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ContentHit {
    pub repo: String,
    /// Repo-relative path.
    pub path: String,
    pub line: u64,
    /// The matching line, without its terminator.
    pub text: String,
}

/// Everything a scan produced, including what it did not return: matches
/// beyond the cap and files it could not or would not read.
#[derive(Debug, Default)]
pub struct FindContentResult {
    pub hits: Vec<ContentHit>,
    /// Matches found beyond `max_hits` (counted, not returned).
    pub suppressed: u64,
    /// Files skipped: unreadable, over `max_file_size`, or a walk error.
    pub skipped_files: u64,
}

/// Where and how to scan.
pub struct ContentScope<'a> {
    /// `(repo name, workdir)` pairs, from watch.toml.
    pub repos: &'a [(String, PathBuf)],
    /// Restrict to one file extension (without the dot).
    pub ext: Option<&'a str>,
    /// Treat the pattern as a literal string instead of a regex.
    pub fixed_strings: bool,
    /// Search hidden files and directories (`.github/`, `.claude/`, dotfile
    /// configs). Off by default to match ripgrep; `.git/` stays excluded
    /// even when enabled.
    pub include_hidden: bool,
    pub max_file_size: u64,
    pub max_hits: usize,
}

/// Scan every scoped repo for `pattern` and return line-accurate hits,
/// sorted by (repo, path, line) for deterministic output. The walk itself is
/// name-sorted so the subset kept under `max_hits` is also deterministic,
/// not readdir-order roulette.
///
/// The walk respects `.gitignore` (even outside a git checkout -- watched
/// workdirs are the corpus, not arbitrary directories), skips hidden files
/// unless `include_hidden` is set (ripgrep's `--hidden` semantics; the
/// `ignore` crate does NOT skip `.git/` on its own once hidden files are
/// admitted -- verified empirically in #705's twin walk -- so `.git` is
/// excluded explicitly), and quits on binary content. Non-UTF-8 text files
/// are searched lossily (invalid bytes become U+FFFD) rather than skipped --
/// grep matches them, so we must too. Per-file errors are counted and
/// skipped, never fatal: one unreadable file must not abort orientation.
pub fn find_content(pattern: &str, scope: &ContentScope<'_>) -> Result<FindContentResult> {
    let matcher = RegexMatcherBuilder::new()
        .line_terminator(Some(b'\n'))
        .fixed_strings(scope.fixed_strings)
        .build(pattern)
        .map_err(|e| LegionError::Search(format!("invalid regex '{pattern}': {e}")))?;

    let mut searcher = SearcherBuilder::new()
        .binary_detection(BinaryDetection::quit(0))
        .line_number(true)
        .build();

    let mut result = FindContentResult::default();
    for (repo_name, workdir) in scope.repos {
        // require_git(false): honor .gitignore in the workdir whether or not
        // the walk starts inside a recognized git checkout (worktrees, tests).
        // sort_by_file_name: readdir order is filesystem-dependent; without a
        // sorted walk the subset kept under max_hits varies across machines.
        let walker = WalkBuilder::new(workdir)
            .require_git(false)
            .hidden(!scope.include_hidden)
            .filter_entry(|entry| entry.file_name() != ".git")
            .sort_by_file_name(std::ffi::OsStr::cmp)
            .build();
        for entry in walker {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => {
                    result.skipped_files += 1;
                    continue;
                }
            };
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let path = entry.path();
            if let Some(ext) = scope.ext
                && path.extension().and_then(|e| e.to_str()) != Some(ext)
            {
                continue;
            }
            match entry.metadata() {
                Ok(md) if md.len() > scope.max_file_size => {
                    result.skipped_files += 1;
                    continue;
                }
                Err(_) => {
                    result.skipped_files += 1;
                    continue;
                }
                Ok(_) => {}
            }
            let rel = relative_path(path, workdir);
            let search = searcher.search_path(
                &matcher,
                path,
                Lossy(|line_number, line| {
                    if result.hits.len() < scope.max_hits {
                        result.hits.push(ContentHit {
                            repo: repo_name.clone(),
                            path: rel.clone(),
                            line: line_number,
                            text: line.trim_end_matches(['\n', '\r']).to_string(),
                        });
                    } else {
                        result.suppressed += 1;
                    }
                    Ok(true)
                }),
            );
            if search.is_err() {
                result.skipped_files += 1;
            }
        }
    }
    result
        .hits
        .sort_by(|a, b| (&a.repo, &a.path, a.line).cmp(&(&b.repo, &b.path, b.line)));
    Ok(result)
}

fn relative_path(path: &Path, workdir: &Path) -> String {
    path.strip_prefix(workdir)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, content: &str) {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(path, content).expect("write fixture");
    }

    fn scope<'a>(repos: &'a [(String, PathBuf)]) -> ContentScope<'a> {
        ContentScope {
            repos,
            ext: None,
            fixed_strings: false,
            include_hidden: false,
            max_file_size: MAX_FILE_SIZE,
            max_hits: MAX_HITS,
        }
    }

    fn one_repo(dir: &Path) -> Vec<(String, PathBuf)> {
        vec![("test".to_string(), dir.to_path_buf())]
    }

    #[test]
    fn conflict_marker_literal_matches_with_line_number() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(
            dir.path(),
            "a.rs",
            "fn main() {}\nlet x = 1;\n<<<<<<< HEAD\n",
        );
        let repos = one_repo(dir.path());
        let mut sc = scope(&repos);
        sc.fixed_strings = true;
        let result = find_content("<<<<<<<", &sc).expect("search");
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].path, "a.rs");
        assert_eq!(result.hits[0].line, 3);
        assert_eq!(result.hits[0].text, "<<<<<<< HEAD");
    }

    #[test]
    fn fixed_strings_treats_dot_literally() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(
            dir.path(),
            "b.css",
            "--spacing-0.5: 4px;\n--spacing-0X5: 5px;\n",
        );
        let repos = one_repo(dir.path());
        let mut sc = scope(&repos);
        sc.fixed_strings = true;
        let result = find_content("--spacing-0.5", &sc).expect("search");
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].text, "--spacing-0.5: 4px;");

        // Same pattern as a regex matches both lines: '.' is a wildcard.
        sc.fixed_strings = false;
        let result = find_content("--spacing-0.5", &sc).expect("search");
        assert_eq!(result.hits.len(), 2);
    }

    #[test]
    fn regex_mode_matches_alternation() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "m.rs", "fn run_alpha() {}\nfn run_gamma() {}\n");
        let repos = one_repo(dir.path());
        let result = find_content("run_(alpha|beta)", &scope(&repos)).expect("search");
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].line, 1);
    }

    #[test]
    fn invalid_regex_is_a_loud_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repos = one_repo(dir.path());
        let err = find_content("(unclosed", &scope(&repos));
        assert!(matches!(err, Err(LegionError::Search(_))));
    }

    #[test]
    fn gitignore_is_respected() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), ".gitignore", "ignored.txt\n");
        write(dir.path(), "ignored.txt", "needle\n");
        write(dir.path(), "kept.txt", "needle\n");
        let repos = one_repo(dir.path());
        let result = find_content("needle", &scope(&repos)).expect("search");
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].path, "kept.txt");
    }

    #[test]
    fn oversized_files_are_skipped_and_counted() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "big.txt", "needle in a large file\n");
        let repos = one_repo(dir.path());
        let mut sc = scope(&repos);
        sc.max_file_size = 4;
        let result = find_content("needle", &sc).expect("search");
        assert!(result.hits.is_empty());
        assert_eq!(result.skipped_files, 1);
    }

    #[test]
    fn hit_cap_counts_suppressed_matches() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "many.txt", "hit\nhit\nhit\nhit\nhit\n");
        let repos = one_repo(dir.path());
        let mut sc = scope(&repos);
        sc.max_hits = 2;
        let result = find_content("hit", &sc).expect("search");
        assert_eq!(result.hits.len(), 2);
        assert_eq!(result.suppressed, 3);
    }

    #[test]
    fn ext_filter_scopes_the_walk() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "a.rs", "needle\n");
        write(dir.path(), "b.md", "needle\n");
        let repos = one_repo(dir.path());
        let mut sc = scope(&repos);
        sc.ext = Some("md");
        let result = find_content("needle", &sc).expect("search");
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].path, "b.md");
    }

    #[test]
    fn binary_files_are_not_searched() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("blob.bin"), b"needle\x00needle\n").expect("write fixture");
        write(dir.path(), "plain.txt", "needle\n");
        let repos = one_repo(dir.path());
        let result = find_content("needle", &scope(&repos)).expect("search");
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].path, "plain.txt");
    }

    #[test]
    fn non_utf8_text_is_searched_lossily_not_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Latin-1 "café needle": 0xE9 is invalid UTF-8. grep matches this
        // file; the lossy sink must too, instead of erroring mid-file.
        std::fs::write(dir.path().join("latin1.txt"), b"caf\xe9 needle\n").expect("write fixture");
        let repos = one_repo(dir.path());
        let result = find_content("needle", &scope(&repos)).expect("search");
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.skipped_files, 0);
        assert!(result.hits[0].text.contains("needle"));
    }

    #[test]
    fn hidden_files_are_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), ".hidden.txt", "needle\n");
        write(dir.path(), "seen.txt", "needle\n");
        let repos = one_repo(dir.path());
        let result = find_content("needle", &scope(&repos)).expect("search");
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].path, "seen.txt");
    }

    #[test]
    fn include_hidden_searches_dotfiles_but_never_git_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), ".github/workflows/ci.yml", "needle\n");
        write(dir.path(), ".git/config", "needle\n");
        write(dir.path(), "seen.txt", "needle\n");
        let repos = one_repo(dir.path());
        let mut sc = scope(&repos);
        sc.include_hidden = true;
        let result = find_content("needle", &sc).expect("search");
        let paths: Vec<&str> = result.hits.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, vec![".github/workflows/ci.yml", "seen.txt"]);
    }

    #[test]
    fn capped_subset_is_walk_order_deterministic() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "a.txt", "hit\nhit\nhit\n");
        write(dir.path(), "z.txt", "hit\nhit\nhit\n");
        let repos = one_repo(dir.path());
        let mut sc = scope(&repos);
        sc.max_hits = 3;
        // The walk is name-sorted, so the kept subset is exactly a.txt's
        // three hits regardless of readdir order.
        let result = find_content("hit", &sc).expect("search");
        assert_eq!(result.hits.len(), 3);
        assert!(result.hits.iter().all(|h| h.path == "a.txt"));
        assert_eq!(result.suppressed, 3);
    }

    #[test]
    fn cross_repo_hits_carry_repo_and_sort_deterministically() {
        let dir_a = tempfile::tempdir().expect("tempdir");
        let dir_b = tempfile::tempdir().expect("tempdir");
        write(dir_a.path(), "z.txt", "needle\n");
        write(dir_b.path(), "a.txt", "needle\n");
        let repos = vec![
            ("beta".to_string(), dir_b.path().to_path_buf()),
            ("alpha".to_string(), dir_a.path().to_path_buf()),
        ];
        let result = find_content("needle", &scope(&repos)).expect("search");
        let repos_seen: Vec<&str> = result.hits.iter().map(|h| h.repo.as_str()).collect();
        assert_eq!(repos_seen, vec!["alpha", "beta"]);
    }
}
