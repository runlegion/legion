//! `sym etc` -- the non-symbol answer surface (#704).
//!
//! find-content (#707) is an exact, line-accurate content search over watched
//! repos, built on the ripgrep engine crates (grep-searcher + grep-regex +
//! ignore). It is a direct disk scan at query time, deliberately NOT an
//! index: a tokenized index returns nothing on punctuation-heavy literals
//! (`<<<<<<<`, `--spacing-0.5`), and any content corpus goes stale on git
//! operations that fire no edit hooks. Scanning the working tree gives grep
//! parity and freshness by construction.
//!
//! extract (#708) is the complementary shape: return the specific bytes an
//! agent wants -- a config value, a frontmatter field -- without reading the
//! whole file. json/toml/yaml, plus the YAML frontmatter block in
//! `.md`/`.mdx`/`.astro`, all convert into one `serde_json::Value` tree so a
//! single dotted-path walker serves every format.

use std::path::{Path, PathBuf};

use grep_regex::RegexMatcherBuilder;
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkFinish, SinkMatch};
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
    /// Files skipped as binary (NUL byte found). Counted separately because
    /// a binary quit returns Ok with no hits -- without this counter a text
    /// file with an embedded NUL would silently vanish from the result.
    pub binary_skipped: u64,
    /// Repos whose root could not be walked at all: `(repo name, error)`.
    /// A dead watch.toml workdir is a whole-corpus gap, not "one file
    /// skipped" -- it must surface by name or a zero-hit scan lies.
    pub failed_repos: Vec<(String, String)>,
}

/// Streaming sink that collects capped hits and, unlike the closure sinks in
/// `grep_searcher::sinks`, observes `SinkFinish::binary_byte_offset` -- the
/// only signal that a search quit early on binary content (the search itself
/// returns Ok in that case).
struct CollectSink<'a> {
    repo: &'a str,
    rel: &'a str,
    max_hits: usize,
    hits: &'a mut Vec<ContentHit>,
    suppressed: &'a mut u64,
    binary: bool,
}

impl Sink for CollectSink<'_> {
    type Error = std::io::Error;

    fn matched(
        &mut self,
        _searcher: &Searcher,
        mat: &SinkMatch<'_>,
    ) -> std::result::Result<bool, Self::Error> {
        if self.hits.len() < self.max_hits {
            let text = String::from_utf8_lossy(mat.bytes());
            self.hits.push(ContentHit {
                repo: self.repo.to_string(),
                path: self.rel.to_string(),
                // line_number(true) is set on the searcher, so this is
                // always Some; 0 would mean a searcher misconfiguration.
                line: mat.line_number().unwrap_or(0),
                text: text.trim_end_matches(['\n', '\r']).to_string(),
            });
        } else {
            *self.suppressed += 1;
        }
        Ok(true)
    }

    fn finish(
        &mut self,
        _searcher: &Searcher,
        finish: &SinkFinish,
    ) -> std::result::Result<(), Self::Error> {
        self.binary = finish.binary_byte_offset().is_some();
        Ok(())
    }
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
/// excluded explicitly), and quits on binary content (a NUL byte), counting
/// the file in `binary_skipped` -- the quit itself returns Ok, so without
/// that counter a text file with an embedded NUL would vanish silently.
/// Non-UTF-8 text files are searched lossily (invalid bytes become U+FFFD)
/// rather than skipped -- grep matches them, so we must too. Per-file errors
/// are counted and skipped, never fatal: one unreadable file must not abort
/// orientation; a repo root that cannot be walked at all is reported by name
/// in `failed_repos`.
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
    // Scan repos in name order: the hit cap truncates in scan order while
    // output is sorted by (repo, path, line), so an unsorted scan could
    // leave an alphabetically-earlier repo looking empty when a later one
    // filled the cap first. The per-repo walk is already name-sorted, so
    // sorting the (small) repo list makes scan order equal output order.
    let mut repos: Vec<&(String, PathBuf)> = scope.repos.iter().collect();
    repos.sort_by(|a, b| a.0.cmp(&b.0));
    for (repo_name, workdir) in repos {
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
                Err(err) => {
                    // depth 0 = the repo root itself failed (moved, deleted,
                    // unreadable): a whole-corpus gap reported by name, not
                    // folded into the per-file skip counter.
                    if err.depth() == Some(0) {
                        result
                            .failed_repos
                            .push((repo_name.clone(), err.to_string()));
                    } else {
                        result.skipped_files += 1;
                    }
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
            let hits_before = result.hits.len();
            let (search_failed, binary) = {
                let mut sink = CollectSink {
                    repo: repo_name,
                    rel: &rel,
                    max_hits: scope.max_hits,
                    hits: &mut result.hits,
                    suppressed: &mut result.suppressed,
                    binary: false,
                };
                let search = searcher.search_path(&matcher, path, &mut sink);
                (search.is_err(), sink.binary)
            };
            if binary {
                result.binary_skipped += 1;
            }
            // A mid-file error after hits were already emitted (mutating
            // workdir, dropped mount) must not count the file as "skipped"
            // -- its partial matches are in the output. Only a file that
            // produced nothing before erroring was truly skipped.
            if search_failed && result.hits.len() == hits_before {
                result.skipped_files += 1;
            }
        }
    }
    result
        .hits
        .sort_by(|a, b| (&a.repo, &a.path, a.line).cmp(&(&b.repo, &b.path, b.line)));
    Ok(result)
}

/// Repo-relative path with forward-slash separators on every platform,
/// matching the inventory walk's convention (#705) so the two surfaces
/// never disagree about the same file.
fn relative_path(path: &Path, workdir: &Path) -> String {
    path.strip_prefix(workdir)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Structured formats `extract` understands, detected from the file
/// extension. `.md`/`.mdx`/`.astro` route to the leading `---`-delimited
/// block, not the whole file -- extract answers "what does this doc's
/// frontmatter say", not "search the prose" (that is find-content's job,
/// #707). Per #708's spec this block is parsed as YAML for all three
/// extensions; note that a real `.astro` file's fence more often holds a
/// JS/TS component script than YAML, so `.astro` support here only covers
/// files whose fence happens to be YAML-shaped (e.g. plain frontmatter
/// metadata above the component script).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceFormat {
    Json,
    Toml,
    Yaml,
    Frontmatter,
}

impl SourceFormat {
    /// Stable lowercase name, used as the `format` field in `extract`'s
    /// usage telemetry.
    pub fn as_str(self) -> &'static str {
        match self {
            SourceFormat::Json => "json",
            SourceFormat::Toml => "toml",
            SourceFormat::Yaml => "yaml",
            SourceFormat::Frontmatter => "frontmatter",
        }
    }
}

/// Detect the structured format from `path`'s extension. Unsupported or
/// missing extensions are a loud, named error -- extract never guesses at a
/// format from content sniffing.
pub fn detect_format(path: &Path) -> Result<SourceFormat> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("json") => Ok(SourceFormat::Json),
        Some("toml") => Ok(SourceFormat::Toml),
        Some("yaml") | Some("yml") => Ok(SourceFormat::Yaml),
        Some("md") | Some("mdx") | Some("astro") => Ok(SourceFormat::Frontmatter),
        Some(other) => Err(LegionError::Etc(format!(
            "unsupported format '.{other}' for extract -- handles json, toml, yaml, and YAML \
             frontmatter in .md/.mdx/.astro"
        ))),
        None => Err(LegionError::Etc(format!(
            "'{}' has no file extension -- cannot detect a format for extract",
            path.display()
        ))),
    }
}

/// Extract one field from `path` by a jq-style dotted path (#708): `.`
/// separated segments walk objects, and a purely numeric segment indexes an
/// array (`scripts.build`, `keywords.0`, `workspaces.packages.1`). The
/// format is detected from the extension -- json, toml, yaml, or the YAML
/// frontmatter block of a `.md`/`.mdx`/`.astro` file. A missing field names
/// the deepest segment that DID resolve, so the caller can correct the path
/// without opening the file. Keys that themselves contain a literal `.` are
/// out of scope for v1.
pub fn extract_field(path: &Path, field: &str) -> Result<serde_json::Value> {
    if field.trim().is_empty() {
        return Err(LegionError::Etc("--field must not be empty".to_string()));
    }
    let format = detect_format(path)?;
    let root = parse_source(path, format)?;
    walk_field(&root, field, path)
}

/// Read and parse `path` into a generic JSON value per its detected format.
/// TOML and YAML each parse into their own `Value` type first, then convert
/// into `serde_json::Value` -- one dotted-path walker (`walk_field`) then
/// serves every format identically.
fn parse_source(path: &Path, format: SourceFormat) -> Result<serde_json::Value> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| LegionError::Etc(format!("cannot read '{}': {e}", path.display())))?;
    match format {
        SourceFormat::Json => serde_json::from_str(&content)
            .map_err(|e| LegionError::Etc(format!("invalid JSON in '{}': {e}", path.display()))),
        SourceFormat::Toml => {
            let value: toml::Value = toml::from_str(&content).map_err(|e| {
                LegionError::Etc(format!("invalid TOML in '{}': {e}", path.display()))
            })?;
            serde_json::to_value(value).map_err(|e| {
                LegionError::Etc(format!(
                    "cannot convert TOML to JSON in '{}': {e}",
                    path.display()
                ))
            })
        }
        SourceFormat::Yaml => parse_yaml(&content, path),
        SourceFormat::Frontmatter => {
            let yaml_text = extract_frontmatter(&content, path)?;
            parse_yaml(&yaml_text, path)
        }
    }
}

/// Parse a YAML document (or a frontmatter block's text) into JSON. YAML is
/// handled by `serde_yaml_ng`, a maintained fork of the archived and
/// unmaintained `serde_yaml` (RUSTSEC-2024-0320) -- API-compatible, so the
/// rest of the pipeline (deserialize to a `Value`, convert to
/// `serde_json::Value`) is identical to the TOML path.
fn parse_yaml(text: &str, path: &Path) -> Result<serde_json::Value> {
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(text)
        .map_err(|e| LegionError::Etc(format!("invalid YAML in '{}': {e}", path.display())))?;
    serde_json::to_value(value).map_err(|e| {
        LegionError::Etc(format!(
            "cannot convert YAML to JSON in '{}': {e}",
            path.display()
        ))
    })
}

/// Pull the YAML frontmatter block out of a `.md`/`.mdx`/`.astro` file: the
/// leading `---` ... `---` (or `...`) delimited section. The prose/component
/// body after it is out of scope for extract -- that is find-content's job
/// (#707).
fn extract_frontmatter(content: &str, path: &Path) -> Result<String> {
    let mut lines = content.lines();
    match lines.next() {
        Some(first) if first.trim_end() == "---" => {}
        _ => {
            return Err(LegionError::Etc(format!(
                "no YAML frontmatter in '{}': file must start with a '---' line",
                path.display()
            )));
        }
    }
    let mut yaml_lines = Vec::new();
    for line in lines {
        let trimmed = line.trim_end();
        if trimmed == "---" || trimmed == "..." {
            return Ok(yaml_lines.join("\n"));
        }
        yaml_lines.push(line);
    }
    Err(LegionError::Etc(format!(
        "YAML frontmatter in '{}' has no closing '---' delimiter",
        path.display()
    )))
}

/// Walk `root` along the `.`-separated segments of `field`. A segment that
/// parses as a plain non-negative integer indexes an array; any other
/// segment looks up an object key. On failure the error names the deepest
/// segment that DID resolve (`<root>` if none did), so the caller can
/// correct the path without opening the file.
///
/// A numeric segment always tries array-indexing first, so an object with a
/// literal numeric key (e.g. `{"0": "x"}`) is unreachable by that key --
/// same v1 scope limitation as keys containing a literal `.`.
fn walk_field(root: &serde_json::Value, field: &str, path: &Path) -> Result<serde_json::Value> {
    let mut current = root;
    let mut resolved: Vec<&str> = Vec::new();
    for segment in field.split('.') {
        let next = match segment.parse::<usize>() {
            Ok(idx) => current.as_array().and_then(|arr| arr.get(idx)),
            Err(_) => current.as_object().and_then(|obj| obj.get(segment)),
        };
        match next {
            Some(value) => {
                current = value;
                resolved.push(segment);
            }
            None => {
                let deepest = if resolved.is_empty() {
                    "<root>".to_string()
                } else {
                    resolved.join(".")
                };
                return Err(LegionError::Etc(format!(
                    "field '{field}' not found in '{}': segment '{segment}' does not resolve \
                     after '{deepest}' (keys containing a literal '.' are out of scope for extract)",
                    path.display()
                )));
            }
        }
    }
    Ok(current.clone())
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
    fn binary_files_are_not_searched_but_are_counted() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("blob.bin"), b"needle\x00needle\n").expect("write fixture");
        write(dir.path(), "plain.txt", "needle\n");
        let repos = one_repo(dir.path());
        let result = find_content("needle", &scope(&repos)).expect("search");
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.hits[0].path, "plain.txt");
        assert_eq!(result.binary_skipped, 1);
    }

    /// Regression guard (#719 review, HIGH): a text file whose MATCHING lines
    /// precede an embedded NUL is treated as binary -- the search quits with
    /// Ok and emits nothing. Without binary_skipped that file would vanish
    /// from hits, skipped_files, and suppressed alike: an undetectable false
    /// negative.
    #[test]
    fn matches_before_embedded_nul_are_not_silently_lost() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("mixed.txt"),
            b"needle on line one\nfiller\n\x00 trailing binary blob",
        )
        .expect("write fixture");
        let repos = one_repo(dir.path());
        let result = find_content("needle", &scope(&repos)).expect("search");
        // The match is quit away by binary detection -- that is grep parity
        // (rg skips binary files in a directory walk too). What must NOT
        // happen is silence: the file is accounted for in binary_skipped.
        assert!(result.hits.is_empty());
        assert_eq!(result.binary_skipped, 1);
        assert_eq!(result.skipped_files, 0);
    }

    /// Regression guard (#719 review, MED): a repo whose workdir is gone is a
    /// whole-corpus gap reported by name, not "1 files skipped".
    #[test]
    fn dead_repo_root_is_reported_by_name_not_as_a_skipped_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "ok.txt", "needle\n");
        let repos = vec![
            ("alive".to_string(), dir.path().to_path_buf()),
            (
                "ghost".to_string(),
                PathBuf::from("/nonexistent/legion-test-ghost-repo"),
            ),
        ];
        let result = find_content("needle", &scope(&repos)).expect("search");
        assert_eq!(result.hits.len(), 1);
        assert_eq!(result.skipped_files, 0);
        assert_eq!(result.failed_repos.len(), 1);
        assert_eq!(result.failed_repos[0].0, "ghost");
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

    // -- extract (#708) --

    #[test]
    fn extract_json_nested_string_field() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(
            dir.path(),
            "package.json",
            r#"{"scripts": {"build": "tsc -p ."}}"#,
        );
        let value = extract_field(&dir.path().join("package.json"), "scripts.build")
            .expect("field resolves");
        assert_eq!(value, serde_json::json!("tsc -p ."));
    }

    #[test]
    fn extract_numeric_segment_indexes_array() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(
            dir.path(),
            "package.json",
            r#"{"keywords": ["cli", "agents", "memory"]}"#,
        );
        let value = extract_field(&dir.path().join("package.json"), "keywords.1")
            .expect("array index resolves");
        assert_eq!(value, serde_json::json!("agents"));
    }

    #[test]
    fn extract_nested_numeric_segment_indexes_array() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(
            dir.path(),
            "pnpm-workspace.json",
            r#"{"workspaces": {"packages": ["apps/web", "packages/core"]}}"#,
        );
        let value = extract_field(
            &dir.path().join("pnpm-workspace.json"),
            "workspaces.packages.1",
        )
        .expect("nested array index resolves");
        assert_eq!(value, serde_json::json!("packages/core"));
    }

    #[test]
    fn extract_missing_field_names_deepest_resolved_segment() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(
            dir.path(),
            "package.json",
            r#"{"scripts": {"build": "tsc"}}"#,
        );
        let err = extract_field(&dir.path().join("package.json"), "scripts.test")
            .expect_err("field is missing");
        let msg = err.to_string();
        assert!(msg.contains("'test'"), "message was: {msg}");
        assert!(msg.contains("'scripts'"), "message was: {msg}");
    }

    #[test]
    fn extract_missing_top_level_field_names_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "package.json", r#"{"scripts": {}}"#);
        let err = extract_field(&dir.path().join("package.json"), "dependencies")
            .expect_err("field is missing");
        assert!(err.to_string().contains("<root>"));
    }

    #[test]
    fn extract_empty_field_is_a_loud_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "package.json", r#"{"scripts": {}}"#);
        let err =
            extract_field(&dir.path().join("package.json"), "").expect_err("empty field rejected");
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn extract_toml_field() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(
            dir.path(),
            "Cargo.toml",
            "[package]\nname = \"legion\"\nversion = \"0.18.8\"\n",
        );
        let value = extract_field(&dir.path().join("Cargo.toml"), "package.name")
            .expect("toml field resolves");
        assert_eq!(value, serde_json::json!("legion"));
    }

    #[test]
    fn extract_yaml_field() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(
            dir.path(),
            "config.yaml",
            "database:\n  host: localhost\n  port: 5432\n",
        );
        let value = extract_field(&dir.path().join("config.yaml"), "database.port")
            .expect("yaml field resolves");
        assert_eq!(value, serde_json::json!(5432));
    }

    #[test]
    fn extract_yml_extension_also_parses_as_yaml() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "config.yml", "name: legion\n");
        let value =
            extract_field(&dir.path().join("config.yml"), "name").expect("yml field resolves");
        assert_eq!(value, serde_json::json!("legion"));
    }

    #[test]
    fn extract_md_frontmatter_field() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(
            dir.path(),
            "post.md",
            "---\ntitle: Hello World\ntags:\n  - rust\n  - cli\n---\n\n# Body\n\nNot YAML.\n",
        );
        let value =
            extract_field(&dir.path().join("post.md"), "title").expect("frontmatter resolves");
        assert_eq!(value, serde_json::json!("Hello World"));
        let tag = extract_field(&dir.path().join("post.md"), "tags.0")
            .expect("frontmatter array resolves");
        assert_eq!(tag, serde_json::json!("rust"));
    }

    #[test]
    fn extract_mdx_frontmatter_field() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(
            dir.path(),
            "doc.mdx",
            "---\ntitle: MDX Doc\n---\nimport { Foo } from './foo';\n\n<Foo />\n",
        );
        let value =
            extract_field(&dir.path().join("doc.mdx"), "title").expect("frontmatter resolves");
        assert_eq!(value, serde_json::json!("MDX Doc"));
    }

    #[test]
    fn extract_astro_frontmatter_field() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(
            dir.path(),
            "page.astro",
            "---\ntitle: Astro Page\ndraft: false\n---\n<h1>{title}</h1>\n",
        );
        let value =
            extract_field(&dir.path().join("page.astro"), "draft").expect("frontmatter resolves");
        assert_eq!(value, serde_json::json!(false));
    }

    #[test]
    fn extract_frontmatter_can_close_with_ellipsis() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(
            dir.path(),
            "post.md",
            "---\ntitle: Ellipsis Close\n...\nBody.\n",
        );
        let value =
            extract_field(&dir.path().join("post.md"), "title").expect("frontmatter resolves");
        assert_eq!(value, serde_json::json!("Ellipsis Close"));
    }

    #[test]
    fn extract_missing_frontmatter_delimiter_is_a_loud_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(
            dir.path(),
            "post.md",
            "# Just a heading\n\nNo frontmatter.\n",
        );
        let err = extract_field(&dir.path().join("post.md"), "title")
            .expect_err("no frontmatter present");
        assert!(err.to_string().contains("frontmatter"));
    }

    #[test]
    fn extract_unclosed_frontmatter_is_a_loud_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(
            dir.path(),
            "post.md",
            "---\ntitle: Unclosed\nBody with no close.\n",
        );
        let err = extract_field(&dir.path().join("post.md"), "title")
            .expect_err("frontmatter never closes");
        assert!(err.to_string().contains("closing"));
    }

    #[test]
    fn extract_unsupported_extension_is_a_loud_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "notes.txt", "not structured\n");
        let err = extract_field(&dir.path().join("notes.txt"), "anything")
            .expect_err("unsupported format rejected");
        assert!(err.to_string().contains("unsupported format"));
    }

    #[test]
    fn extract_no_extension_is_a_loud_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "Makefile", "build:\n\tcargo build\n");
        let err = extract_field(&dir.path().join("Makefile"), "anything")
            .expect_err("no extension rejected");
        assert!(err.to_string().contains("no file extension"));
    }

    #[test]
    fn extract_invalid_json_is_a_loud_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), "bad.json", "{not valid json");
        let err = extract_field(&dir.path().join("bad.json"), "anything")
            .expect_err("invalid json rejected");
        assert!(err.to_string().contains("invalid JSON"));
    }

    #[test]
    fn detect_format_maps_every_supported_extension() {
        assert_eq!(
            detect_format(Path::new("x.json")).unwrap(),
            SourceFormat::Json
        );
        assert_eq!(
            detect_format(Path::new("x.toml")).unwrap(),
            SourceFormat::Toml
        );
        assert_eq!(
            detect_format(Path::new("x.yaml")).unwrap(),
            SourceFormat::Yaml
        );
        assert_eq!(
            detect_format(Path::new("x.yml")).unwrap(),
            SourceFormat::Yaml
        );
        assert_eq!(
            detect_format(Path::new("x.md")).unwrap(),
            SourceFormat::Frontmatter
        );
        assert_eq!(
            detect_format(Path::new("x.mdx")).unwrap(),
            SourceFormat::Frontmatter
        );
        assert_eq!(
            detect_format(Path::new("x.astro")).unwrap(),
            SourceFormat::Frontmatter
        );
    }
}
