//! Answers for the harness Grep and Glob tools, in the same response (#1338).
//!
//! A PreToolUse hook cannot turn a Grep or Glob call into another tool. It
//! can let the call run, add `additionalContext`, and change the same tool's
//! input. So when legion can answer a search fully, the hook lets the call
//! run, carries legion's answer as `additionalContext`, and for Grep sets
//! `head_limit: 1` so the tool's own result is next to nothing. When legion
//! cannot answer fully, or anything here fails or runs out of time, the call
//! runs exactly as the agent sent it. Nothing here refuses a call or names a
//! command for the agent to run.
//!
//! "Fully" is the whole risk: a wrong yes hands the agent fewer matches
//! than the tool would. The accepted set is deliberately narrow.
//!
//! - Grep (Claude Code runs `rg --hidden --glob '!.git' ...`, respecting
//!   ignore files): every input key is one sym can honour, the search root
//!   is a real directory inside a watched repo and inside git (rg reads
//!   `.gitignore` only there), no `.rgignore` applies (sym's walk does not
//!   read it), the tree holds only files and directories, and the scan
//!   reports nothing skipped, capped, or binary. `glob`, `type`,
//!   `multiline`, and context lines are declined: sym's answer would not
//!   hold what they return.
//! - Glob (Claude Code runs `rg --files --glob <pattern> --hidden
//!   --no-ignore` by default): the answer comes from the file inventory,
//!   which respects `.gitignore`. It is full only when no ignored path, and
//!   no `.git` entry, could match the pattern; it is fresh only when every
//!   file git sees under the root that matches the pattern is in the
//!   inventory, and every inventory match still exists on disk.
//!
//! An answer longer than [`MAX_ANSWER_LINES`] is not injected: the tool
//! runs as normal instead.

use std::collections::BTreeSet;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::Searcher;
use grep_searcher::sinks::UTF8;
use ignore::overrides::{Override, OverrideBuilder};
use serde_json::{Map, Value};

use crate::db::Database;
use crate::db::inventory::{FileInventoryEntry, InventoryFilter};
use crate::etc;
use crate::watch::WatchRepoConfig;

/// The most lines an injected answer may carry, recall section included.
pub(crate) const MAX_ANSWER_LINES: usize = 200;

/// How long an answer may take before the tool runs as normal. The answer
/// runs after the decision, so this is added to the decision's own time;
/// it stays well under the wrapper's kill (`plugin/hooks/legion-cmd.sh`).
pub(crate) const ANSWER_DEADLINE: Duration = Duration::from_millis(3000);

/// The Grep input keys sym's answer can honour (#1338's list). Any other
/// key -- `-o`, `offset`, `context`, one the harness adds later -- makes
/// the call the tool's.
const GREP_KEYS: [&str; 12] = [
    "pattern",
    "path",
    "glob",
    "type",
    "output_mode",
    "-i",
    "-n",
    "-A",
    "-B",
    "-C",
    "multiline",
    "head_limit",
];

/// The Glob input keys the inventory's answer can honour.
const GLOB_KEYS: [&str; 2] = ["pattern", "path"];

/// The Grep output modes; each one's result is files and lines sym's
/// answer holds.
const GREP_MODES: [&str; 3] = ["content", "files_with_matches", "count"];

/// Characters that make a glob segment a pattern rather than a literal
/// directory name.
const GLOB_META: [char; 5] = ['*', '?', '[', '{', '\\'];

/// The Claude Code setting that turns the Glob tool's `--no-ignore` off.
/// Unset, the tool reads ignored files.
const GLOB_NO_IGNORE_ENV: &str = "CLAUDE_CODE_GLOB_NO_IGNORE";

/// What legion adds to a call it answered: the answer the agent reads, and
/// for Grep the input that keeps the tool's own result to one entry.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Answer {
    pub text: String,
    pub updated_input: Option<Value>,
}

/// One harness search call, as the hook payload carried it.
#[derive(Debug, Clone)]
pub(crate) struct SearchCall {
    pub tool: String,
    pub input: Value,
    pub cwd: Option<String>,
    /// `LEGION_REPO`, which with `cwd` names the repo recall is scoped to,
    /// derived the way the decision derives it.
    pub legion_repo: Option<String>,
}

/// Answers a search call, or declines. A seam so the hook's tests can stub
/// an answer, a decline, a panic, or a slow source.
pub(crate) trait Answerer: Send + Sync {
    /// `None` means "run the tool as sent": declined, failed, or not a
    /// search legion answers.
    fn answer(&self, call: &SearchCall) -> Option<Answer>;
}

/// The production answerer: watch.toml's repos, sym's content scan, the
/// local store's inventory, and BM25 recall.
pub(crate) struct LocalAnswers;

impl Answerer for LocalAnswers {
    fn answer(&self, call: &SearchCall) -> Option<Answer> {
        let workdirs: Vec<(String, PathBuf)> = watched_workdirs()?;
        let cwd: Option<&Path> = call.cwd.as_deref().map(Path::new);
        match call.tool.as_str() {
            "Grep" => {
                let repo: Option<String> =
                    crate::cmd::hook::repo_for(call.legion_repo.as_deref(), call.cwd.as_deref());
                grep_answer(&call.input, cwd, &workdirs, &|pattern: &str| {
                    recall_texts(repo.as_deref(), pattern)
                })
            }
            "Glob" => {
                let db: Database = crate::cli::util::open_db().ok()?;
                let reads_ignored: bool =
                    glob_reads_ignored(std::env::var(GLOB_NO_IGNORE_ENV).ok().as_deref());
                glob_answer(&call.input, cwd, &workdirs, &db, reads_ignored)
            }
            _ => None,
        }
    }
}

/// Runs `answerer` on its own thread and waits at most `deadline`. A
/// panic, an overrun, or a thread that cannot start is `None`: the tool
/// runs as sent. An overrun thread is left to end with the one-shot hook
/// process.
pub(crate) fn answer_within(
    answerer: Arc<dyn Answerer>,
    call: SearchCall,
    deadline: Duration,
) -> Option<Answer> {
    let (tx, rx) = mpsc::channel::<Option<Answer>>();
    thread::Builder::new()
        .name("legion-cmd-answer".to_string())
        .spawn(move || {
            let answered: Option<Answer> =
                panic::catch_unwind(AssertUnwindSafe(|| answerer.answer(&call))).unwrap_or(None);
            let _ = tx.send(answered);
        })
        .ok()?;
    rx.recv_timeout(deadline).ok().flatten()
}

/// Every watched repo as `(name, canonical workdir)`. `None` when
/// watch.toml cannot be read; a workdir that no longer resolves is left out.
fn watched_workdirs() -> Option<Vec<(String, PathBuf)>> {
    let path: PathBuf = crate::data_dir().ok()?.join("watch.toml");
    let repos: Vec<WatchRepoConfig> = crate::watch::list_repos_in_config(&path).ok()?;
    Some(
        repos
            .into_iter()
            .filter_map(|r| {
                std::fs::canonicalize(&r.workdir)
                    .ok()
                    .map(|dir| (r.name, dir))
            })
            .collect(),
    )
}

/// The texts of the reflections in `repo` that match `pattern` (BM25, no
/// embedding model: the answer runs under a deadline). No repo is no
/// reflections; a failed lookup is `Err`, and the tool runs as sent.
fn recall_texts(repo: Option<&str>, pattern: &str) -> Result<Vec<String>, ()> {
    let Some(repo) = repo else {
        return Ok(Vec::new());
    };
    let (db, index) = crate::cli::util::open_db_and_index().map_err(|_| ())?;
    let result: crate::recall::RecallResult = crate::recall::recall_bm25(
        &db,
        &index,
        repo,
        pattern,
        5,
        crate::recall::ArchiveMode::Hot,
        &crate::timerange::TimeRange::default(),
    )
    .map_err(|_| ())?;
    Ok(result.reflections.into_iter().map(|r| r.text).collect())
}

/// Claude Code's own reading of a boolean setting: unset is `true` (the
/// Glob tool's default), and only a false-like value turns it off.
fn glob_reads_ignored(setting: Option<&str>) -> bool {
    !matches!(
        setting.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("0" | "false" | "no" | "off")
    )
}

// -- Grep -------------------------------------------------------------------

/// sym's answer to a Grep call, or `None` when it would not hold every file
/// and line the tool returns. `recall` supplies the reflection texts; its
/// failure is a decline.
fn grep_answer(
    input: &Value,
    cwd: Option<&Path>,
    workdirs: &[(String, PathBuf)],
    recall: &dyn Fn(&str) -> Result<Vec<String>, ()>,
) -> Option<Answer> {
    let fields: &Map<String, Value> = only_keys(input, &GREP_KEYS)?;
    let pattern: &str = fields.get("pattern")?.as_str()?;
    if pattern.is_empty() || pattern.contains('\n') {
        return None;
    }
    // Filters sym does not apply the tool's way, and output sym does not
    // produce (context lines, cross-line matches).
    if fields.contains_key("glob") || fields.contains_key("type") {
        return None;
    }
    if !is_absent_or(fields.get("multiline"), |v| v.as_bool() == Some(false)) {
        return None;
    }
    for key in ["-A", "-B", "-C"] {
        if !is_absent_or(fields.get(key), |v| v.as_u64() == Some(0)) {
            return None;
        }
    }
    let mode_ok = |v: &Value| v.as_str().is_some_and(|m| GREP_MODES.contains(&m));
    if !is_absent_or(fields.get("output_mode"), mode_ok)
        || !is_absent_or(fields.get("-n"), Value::is_boolean)
        || !is_absent_or(fields.get("-i"), Value::is_boolean)
        || !is_absent_or(fields.get("head_limit"), Value::is_u64)
    {
        return None;
    }
    let root: PathBuf = search_root(fields, cwd, workdirs)?.1;
    if !rg_reads_ignores_like_sym(&root) {
        return None;
    }
    let ignore_case: bool = fields.get("-i").and_then(Value::as_bool) == Some(true);
    let matcher: String = if ignore_case {
        format!("(?i){pattern}")
    } else {
        pattern.to_string()
    };
    let repos: Vec<(String, PathBuf)> = vec![(String::new(), root.clone())];
    let scope = etc::ContentScope {
        repos: &repos,
        ext: None,
        fixed_strings: false,
        // The tool passes --hidden and respects ignore files.
        include_hidden: true,
        no_ignore: false,
        max_file_size: etc::MAX_FILE_SIZE,
        max_hits: etc::MAX_HITS,
    };
    let result: etc::FindContentResult = etc::find_content(&matcher, &scope).ok()?;
    if result.suppressed > 0
        || result.skipped_files > 0
        || result.binary_skipped > 0
        || !result.failed_repos.is_empty()
    {
        return None;
    }
    let lines: Vec<String> = result
        .hits
        .iter()
        .map(|hit| {
            format!(
                "{}:{}:{}",
                root.join(&hit.path).display(),
                hit.line,
                hit.text
            )
        })
        .collect();
    // Recall ranks by terms; only a reflection the pattern itself matches,
    // the way the tool would match a line, is added.
    let line_matcher: RegexMatcher = RegexMatcherBuilder::new()
        .line_terminator(Some(b'\n'))
        .build(&matcher)
        .ok()?;
    let reflections: Vec<String> = recall(pattern)
        .ok()?
        .into_iter()
        .filter(|text| text_matches(&line_matcher, text))
        .collect();

    let mut text: String = if lines.is_empty() {
        "legion answered this search in full: no line under the search path matches.".to_string()
    } else {
        format!(
            "legion answered this search in full: all {} matching lines, as path:line:text. \
             The search tool's own result below is limited to one entry; this list is complete.\n{}",
            lines.len(),
            lines.join("\n")
        )
    };
    if !reflections.is_empty() {
        text.push_str("\n\nReflections in this repo that match the pattern:");
        for reflection in &reflections {
            text.push_str("\n- ");
            text.push_str(&one_line(reflection));
        }
    }
    if text.lines().count() > MAX_ANSWER_LINES {
        return None;
    }
    let mut updated: Map<String, Value> = fields.clone();
    updated.insert("head_limit".to_string(), Value::from(1));
    Some(Answer {
        text,
        updated_input: Some(Value::Object(updated)),
    })
}

/// True when rg's ignore handling under `root` is the one sym's walk
/// applies: `root` is inside git (rg reads `.gitignore` only there), no
/// `.rgignore` applies at or above it or anywhere in the tree (sym's walk
/// does not read one), and the tree holds only files and directories
/// (symlinks and special files are walked differently).
fn rg_reads_ignores_like_sym(root: &Path) -> bool {
    let in_git: bool = root.ancestors().any(|a| a.join(".git").exists());
    let rgignore_above: bool = root.ancestors().any(|a| a.join(".rgignore").exists());
    if !in_git || rgignore_above {
        return false;
    }
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .filter_entry(|entry| entry.file_name() != ".git")
        .build();
    for entry in walker {
        let Ok(entry) = entry else {
            return false;
        };
        let Some(file_type) = entry.file_type() else {
            return false;
        };
        if !(file_type.is_file() || file_type.is_dir()) {
            return false;
        }
        if file_type.is_dir() && entry.path().join(".rgignore").exists() {
            return false;
        }
    }
    true
}

// -- Glob -------------------------------------------------------------------

/// The inventory's answer to a Glob call, or `None` when it is not both
/// full and fresh. `reads_ignored` is whether the tool reads ignored files
/// (its default).
fn glob_answer(
    input: &Value,
    cwd: Option<&Path>,
    workdirs: &[(String, PathBuf)],
    db: &Database,
    reads_ignored: bool,
) -> Option<Answer> {
    let fields: &Map<String, Value> = only_keys(input, &GLOB_KEYS)?;
    let pattern: &str = fields.get("pattern")?.as_str()?;
    // The tool rewrites an absolute or home-relative pattern into another
    // search root; a leading `!` is a negation, not a match.
    if pattern.is_empty()
        || pattern.starts_with(['/', '~', '!'])
        || pattern.split('/').any(|segment| segment == "..")
    {
        return None;
    }
    let ((repo, workdir), root) = search_root(fields, cwd, workdirs)?;
    // The inventory walks the repo's own checkout; a root in another
    // checkout nested inside it (a worktree) is not in the inventory.
    let toplevel: PathBuf = crate::inventory::current_toplevel(&root)?;
    if std::fs::canonicalize(toplevel).ok()? != workdir {
        return None;
    }
    // An ignored root: the tool lists what is under it, the inventory never
    // walked it.
    if !root_is_tracked_ground(&root)? {
        return None;
    }
    // Never indexed: no inventory to answer from.
    db.get_inventory_snapshot(&repo).ok()??;
    let matcher: Override = OverrideBuilder::new(&root)
        .add(pattern)
        .ok()?
        .build()
        .ok()?;
    let matches = |path: &Path| matcher.matched(path, false).is_whitelist();

    let rows: Vec<FileInventoryEntry> = db
        .list_file_inventory(&InventoryFilter {
            repo: Some(&repo),
            ..InventoryFilter::default()
        })
        .ok()?;
    let mut found: BTreeSet<PathBuf> = BTreeSet::new();
    for row in &rows {
        let path: PathBuf = workdir.join(&row.path);
        if !path.starts_with(&root) || !matches(&path) {
            continue;
        }
        // A match deleted since the index is a stale answer.
        if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
            return None;
        }
        found.insert(path);
    }

    // Fresh: every file git sees under the root that matches is in the
    // inventory. A nested checkout (a directory entry) is not walked here.
    for rel in git_paths(&root, &["--cached", "--others", "--exclude-standard"])? {
        let path: PathBuf = root.join(&rel);
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            return None;
        }
        if meta.is_file() && matches(&path) && !found.contains(&path) {
            return None;
        }
    }

    // Full: the tool also reads ignored files and `.git`, which the
    // inventory never holds. Any of them that could match is a decline.
    if reads_ignored {
        let mut ignored: Vec<String> = git_paths(
            &root,
            &["--others", "--ignored", "--exclude-standard", "--directory"],
        )?;
        match std::fs::symlink_metadata(root.join(".git")) {
            Ok(meta) if meta.is_dir() => ignored.push(".git/".to_string()),
            Ok(_) => ignored.push(".git".to_string()),
            Err(_) => {}
        }
        for rel in &ignored {
            let blocked: bool = match rel.strip_suffix('/') {
                Some(dir) => could_match_under(pattern, dir),
                None => matches(&root.join(rel)),
            };
            if blocked {
                return None;
            }
        }
    }

    let text: String = if found.is_empty() {
        "legion answered this search in full from its file inventory: no file under the search path matches.".to_string()
    } else {
        let paths: Vec<String> = found.iter().map(|p| p.display().to_string()).collect();
        format!(
            "legion answered this search in full from its file inventory: all {} matching files. \
             The search tool's own result below may be cut short; this list is complete.\n{}",
            paths.len(),
            paths.join("\n")
        )
    };
    if text.lines().count() > MAX_ANSWER_LINES {
        return None;
    }
    Some(Answer {
        text,
        updated_input: None,
    })
}

/// Whether `pattern` could match a path under the directory `dir` (both
/// relative to the search root). Conservative: only a literal leading
/// directory of the pattern that differs from `dir`'s rules it out. A
/// pattern with no `/` matches a name at any depth.
fn could_match_under(pattern: &str, dir: &str) -> bool {
    if !pattern.contains('/') {
        return true;
    }
    let literal: Vec<&str> = pattern
        .split('/')
        .take_while(|segment| !segment.is_empty() && !segment.contains(GLOB_META))
        .collect();
    literal
        .iter()
        .zip(dir.split('/'))
        .all(|(want, have)| want == &have)
}

/// Whether `root` is outside every ignore rule (`git check-ignore`): `true`
/// not ignored, `false` ignored, `None` when git cannot say.
fn root_is_tracked_ground(root: &Path) -> Option<bool> {
    let status = Command::new("git")
        .args(["check-ignore", "-q", "."])
        .current_dir(root)
        .status()
        .ok()?;
    match status.code() {
        Some(0) => Some(false),
        Some(1) => Some(true),
        _ => None,
    }
}

/// `git ls-files -z <args>` run in `root`: paths relative to `root`.
/// `None` when git fails.
fn git_paths(root: &Path, args: &[&str]) -> Option<Vec<String>> {
    let output = Command::new("git")
        .arg("ls-files")
        .arg("-z")
        .args(args)
        .current_dir(root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text: String = String::from_utf8(output.stdout).ok()?;
    Some(
        text.split('\0')
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

// -- shared -----------------------------------------------------------------

/// `input` as an object whose every key is in `allowed`, else `None`.
fn only_keys<'a>(input: &'a Value, allowed: &[&str]) -> Option<&'a Map<String, Value>> {
    let fields: &Map<String, Value> = input.as_object()?;
    fields
        .keys()
        .all(|key| allowed.contains(&key.as_str()))
        .then_some(fields)
}

/// True when `value` is absent (or null) or passes `ok`.
fn is_absent_or(value: Option<&Value>, ok: impl Fn(&Value) -> bool) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(value) => ok(value),
    }
}

/// The directory a search runs over, canonical, with the watched repo it is
/// inside: `path` resolved against `cwd`, or `cwd` itself. `None` when it
/// is not a real directory (a file, a symlink, missing) or is outside every
/// watched repo.
fn search_root(
    fields: &Map<String, Value>,
    cwd: Option<&Path>,
    workdirs: &[(String, PathBuf)],
) -> Option<((String, PathBuf), PathBuf)> {
    let given: PathBuf = match fields.get("path") {
        None | Some(Value::Null) => cwd?.to_path_buf(),
        Some(path) => {
            let path: &Path = Path::new(path.as_str()?);
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                cwd?.join(path)
            }
        }
    };
    if !given.is_absolute() || given.components().any(|c| c == Component::ParentDir) {
        return None;
    }
    if !std::fs::symlink_metadata(&given).ok()?.is_dir() {
        return None;
    }
    let root: PathBuf = std::fs::canonicalize(&given).ok()?;
    let repo: &(String, PathBuf) = workdirs
        .iter()
        .filter(|(_, workdir)| root.starts_with(workdir))
        .max_by_key(|(_, workdir)| workdir.components().count())?;
    Some((repo.clone(), root))
}

/// True when some line of `text` matches: the Grep tool's own test.
fn text_matches(matcher: &RegexMatcher, text: &str) -> bool {
    let mut found = false;
    let searched = Searcher::new().search_slice(
        matcher,
        text.as_bytes(),
        UTF8(|_, _| {
            found = true;
            Ok(false)
        }),
    );
    searched.is_ok() && found
}

/// A reflection on one line, so each counts once against the line cap.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<&str>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil::test_db;
    use serde_json::json;
    use std::fs;
    use tempfile::TempDir;

    /// A git checkout in a tempdir, registered as the watched repo `demo`.
    struct Repo {
        _dir: TempDir,
        root: PathBuf,
    }

    impl Repo {
        fn new() -> Self {
            let dir: TempDir = tempfile::tempdir().expect("tempdir");
            let root: PathBuf = fs::canonicalize(dir.path()).expect("canonical");
            let status = Command::new("git")
                .args(["init", "-q"])
                .current_dir(&root)
                .status()
                .expect("git init");
            assert!(status.success());
            Self { _dir: dir, root }
        }

        fn write(&self, rel: &str, content: &str) {
            let path: PathBuf = self.root.join(rel);
            fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            fs::write(path, content).expect("write");
        }

        fn workdirs(&self) -> Vec<(String, PathBuf)> {
            vec![("demo".to_string(), self.root.clone())]
        }

        fn cwd(&self) -> &Path {
            &self.root
        }

        /// Runs the inventory walk into `db`, as `legion index` does.
        fn index(&self, db: &Database) {
            let outcome = crate::inventory::walk_repo("demo", &self.root);
            db.upsert_file_inventory(&outcome.entries).expect("upsert");
            db.upsert_inventory_snapshot("demo", "2026-09-28T00:00:00+00:00", None)
                .expect("snapshot");
        }
    }

    fn no_recall(_: &str) -> Result<Vec<String>, ()> {
        Ok(Vec::new())
    }

    fn grep(repo: &Repo, input: Value) -> Option<Answer> {
        grep_answer(&input, Some(repo.cwd()), &repo.workdirs(), &no_recall)
    }

    #[test]
    fn a_grep_sym_answers_fully_returns_each_line_with_path_and_number() {
        let repo = Repo::new();
        repo.write("src/a.rs", "fn main() {}\nlet x = 1;\n");
        repo.write("src/b.rs", "// fn main is elsewhere\n");
        let answer = grep(
            &repo,
            json!({"pattern": "fn main", "output_mode": "content", "-n": true}),
        )
        .expect("sym answers");
        let a = repo.root.join("src/a.rs");
        let b = repo.root.join("src/b.rs");
        assert!(
            answer
                .text
                .contains(&format!("{}:1:fn main() {{}}", a.display()))
        );
        assert!(
            answer
                .text
                .contains(&format!("{}:1:// fn main is elsewhere", b.display()))
        );
        assert!(!answer.text.contains("let x"));
        let updated = answer.updated_input.expect("updatedInput");
        assert_eq!(updated["head_limit"], 1);
        assert_eq!(updated["pattern"], "fn main");
        assert_eq!(updated["output_mode"], "content");
    }

    #[test]
    fn a_grep_reads_hidden_files_and_skips_ignored_ones_like_the_tool() {
        let repo = Repo::new();
        repo.write(".gitignore", "target/\n");
        repo.write(".github/ci.yml", "needle\n");
        repo.write("target/out.rs", "needle\n");
        let answer = grep(&repo, json!({"pattern": "needle"})).expect("sym answers");
        assert!(answer.text.contains(".github/ci.yml:1:needle"));
        assert!(!answer.text.contains("target/out.rs"));
    }

    #[test]
    fn a_grep_with_i_matches_case_insensitively() {
        let repo = Repo::new();
        repo.write("a.txt", "NEEDLE\n");
        let answer = grep(&repo, json!({"pattern": "needle", "-i": true})).expect("answers");
        assert!(answer.text.contains("a.txt:1:NEEDLE"));
        let sensitive = grep(&repo, json!({"pattern": "needle"})).expect("answers");
        assert!(sensitive.text.contains("no line"));
    }

    #[test]
    fn a_grep_with_a_parameter_sym_cannot_honour_is_declined() {
        let repo = Repo::new();
        repo.write("a.txt", "needle\n");
        for input in [
            json!({"pattern": "needle", "glob": "*.txt"}),
            json!({"pattern": "needle", "type": "rust"}),
            json!({"pattern": "needle", "multiline": true}),
            json!({"pattern": "needle", "-A": 2}),
            json!({"pattern": "needle", "-C": 1}),
            json!({"pattern": "needle", "-o": true}),
            json!({"pattern": "needle", "offset": 5}),
            json!({"pattern": "needle", "context": 3}),
            json!({"pattern": "needle", "output_mode": "lines"}),
            json!({"pattern": ""}),
            json!({"pattern": "("}),
            json!({"path": "."}),
        ] {
            assert_eq!(grep(&repo, input.clone()), None, "{input}");
        }
        // Zero context lines and an explicit false multiline are the
        // tool's defaults, and sym answers them.
        assert!(
            grep(
                &repo,
                json!({"pattern": "needle", "-C": 0, "multiline": false})
            )
            .is_some()
        );
    }

    #[test]
    fn a_grep_whose_path_sym_cannot_stand_in_for_is_declined() {
        let repo = Repo::new();
        repo.write("a.txt", "needle\n");
        repo.write("sub/b.txt", "needle\n");
        // A file operand: rg reads it even when it is ignored.
        assert_eq!(
            grep(&repo, json!({"pattern": "needle", "path": "a.txt"})),
            None
        );
        // Outside every watched repo.
        let elsewhere = tempfile::tempdir().expect("tempdir");
        let input = json!({"pattern": "needle", "path": elsewhere.path().to_str().expect("utf-8")});
        assert_eq!(grep(&repo, input), None);
        // A missing directory, and no cwd for a relative path.
        assert_eq!(
            grep(&repo, json!({"pattern": "needle", "path": "gone"})),
            None
        );
        assert_eq!(
            grep_answer(
                &json!({"pattern": "needle"}),
                None,
                &repo.workdirs(),
                &no_recall
            ),
            None
        );
        // A subdirectory is answered, and only its lines.
        let answer = grep(&repo, json!({"pattern": "needle", "path": "sub"})).expect("answers");
        assert!(answer.text.contains("sub/b.txt:1:needle"));
        assert!(!answer.text.contains("a.txt"));
    }

    #[test]
    fn a_grep_outside_git_or_under_an_rgignore_is_declined() {
        let plain = tempfile::tempdir().expect("tempdir");
        let root = fs::canonicalize(plain.path()).expect("canonical");
        fs::write(root.join("a.txt"), "needle\n").expect("write");
        let workdirs = vec![("plain".to_string(), root.clone())];
        assert_eq!(
            grep_answer(
                &json!({"pattern": "needle"}),
                Some(&root),
                &workdirs,
                &no_recall
            ),
            None
        );

        let repo = Repo::new();
        repo.write("a.txt", "needle\n");
        repo.write(".hidden/.rgignore", "x\n");
        assert_eq!(grep(&repo, json!({"pattern": "needle"})), None);
    }

    #[test]
    fn a_grep_answer_over_the_line_cap_is_not_injected() {
        let repo = Repo::new();
        let body: String = "needle\n".repeat(MAX_ANSWER_LINES + 1);
        repo.write("a.txt", &body);
        assert_eq!(grep(&repo, json!({"pattern": "needle"})), None);
    }

    #[test]
    fn recall_is_added_only_when_reflections_match_and_its_failure_declines() {
        let repo = Repo::new();
        repo.write("a.txt", "needle\n");
        // Recall ranks by terms; a reflection the pattern does not match
        // (only its other terms) is left out.
        let found = |_: &str| {
            Ok(vec![
                "needles live\nin haystacks".to_string(),
                "haystacks are hay".to_string(),
            ])
        };
        let answer = grep_answer(
            &json!({"pattern": "needle"}),
            Some(repo.cwd()),
            &repo.workdirs(),
            &found,
        )
        .expect("answers");
        assert!(answer.text.contains("Reflections in this repo"));
        assert!(answer.text.contains("- needles live in haystacks"));
        assert!(!answer.text.contains("haystacks are hay"));

        let unmatched = |_: &str| Ok(vec!["haystacks are hay".to_string()]);
        let answer = grep_answer(
            &json!({"pattern": "needle"}),
            Some(repo.cwd()),
            &repo.workdirs(),
            &unmatched,
        )
        .expect("answers");
        assert!(!answer.text.contains("Reflections"));

        let none = grep(&repo, json!({"pattern": "needle"})).expect("answers");
        assert!(!none.text.contains("Reflections"));

        let failing = |_: &str| Err(());
        assert_eq!(
            grep_answer(
                &json!({"pattern": "needle"}),
                Some(repo.cwd()),
                &repo.workdirs(),
                &failing
            ),
            None
        );
    }

    fn glob(repo: &Repo, db: &Database, input: Value, reads_ignored: bool) -> Option<Answer> {
        glob_answer(
            &input,
            Some(repo.cwd()),
            &repo.workdirs(),
            db,
            reads_ignored,
        )
    }

    #[test]
    fn a_glob_the_inventory_answers_fully_and_fresh_returns_its_paths() {
        let repo = Repo::new();
        repo.write("src/a.rs", "");
        repo.write("src/nested/b.rs", "");
        repo.write("README.md", "");
        let db = test_db();
        repo.index(&db);
        let answer = glob(&repo, &db, json!({"pattern": "src/**/*.rs"}), true).expect("answers");
        assert!(
            answer
                .text
                .contains(&repo.root.join("src/a.rs").display().to_string())
        );
        assert!(
            answer
                .text
                .contains(&repo.root.join("src/nested/b.rs").display().to_string())
        );
        assert!(!answer.text.contains("README.md"));
        assert_eq!(answer.updated_input, None);
    }

    #[test]
    fn a_file_created_after_the_index_makes_the_glob_the_tools() {
        let repo = Repo::new();
        repo.write("src/a.rs", "");
        let db = test_db();
        repo.index(&db);
        assert!(glob(&repo, &db, json!({"pattern": "src/*.rs"}), true).is_some());
        repo.write("src/new.rs", "");
        assert_eq!(glob(&repo, &db, json!({"pattern": "src/*.rs"}), true), None);
        // Once indexed again, the new file is in the answer.
        repo.index(&db);
        let answer = glob(&repo, &db, json!({"pattern": "src/*.rs"}), true).expect("answers");
        assert!(answer.text.contains("src/new.rs"));
    }

    #[test]
    fn a_file_deleted_after_the_index_makes_the_glob_the_tools() {
        let repo = Repo::new();
        repo.write("src/a.rs", "");
        repo.write("src/b.rs", "");
        let db = test_db();
        repo.index(&db);
        fs::remove_file(repo.root.join("src/b.rs")).expect("remove");
        assert_eq!(glob(&repo, &db, json!({"pattern": "src/*.rs"}), true), None);
    }

    #[test]
    fn a_glob_that_could_match_an_ignored_path_is_the_tools_when_it_reads_ignored_files() {
        let repo = Repo::new();
        repo.write(".gitignore", "target/\n");
        repo.write("src/a.rs", "");
        repo.write("target/gen.rs", "");
        let db = test_db();
        repo.index(&db);
        // `**/*.rs` could match under target/, which the tool reads.
        assert_eq!(glob(&repo, &db, json!({"pattern": "**/*.rs"}), true), None);
        // A literal leading directory rules target/ and .git/ out.
        assert!(glob(&repo, &db, json!({"pattern": "src/**/*.rs"}), true).is_some());
        // With the tool set to respect ignore files, the inventory is full.
        let answer = glob(&repo, &db, json!({"pattern": "**/*.rs"}), false).expect("answers");
        assert!(answer.text.contains("src/a.rs"));
        assert!(!answer.text.contains("target/gen.rs"));
        // An ignored search root: the tool lists what is under it either
        // way, and the inventory never walked it.
        for reads_ignored in [true, false] {
            let input = json!({"pattern": "*.rs", "path": "target"});
            assert_eq!(glob(&repo, &db, input, reads_ignored), None);
        }
    }

    #[test]
    fn a_glob_the_inventory_cannot_answer_is_declined() {
        let repo = Repo::new();
        repo.write("src/a.rs", "");
        let db = test_db();
        // Not indexed yet.
        assert_eq!(
            glob(&repo, &db, json!({"pattern": "src/*.rs"}), false),
            None
        );
        repo.index(&db);
        for input in [
            json!({"pattern": "/abs/*.rs"}),
            json!({"pattern": "!src/*.rs"}),
            json!({"pattern": "../*.rs"}),
            json!({"pattern": "src/*.rs", "limit": 5}),
            json!({"pattern": "src/*.rs", "path": "src/a.rs"}),
            json!({"path": "src"}),
        ] {
            assert_eq!(glob(&repo, &db, input.clone(), false), None, "{input}");
        }
    }

    #[test]
    fn could_match_under_rules_out_only_a_differing_literal_directory() {
        assert!(could_match_under("*.rs", "target"));
        assert!(could_match_under("**/*.rs", "target"));
        assert!(could_match_under("src/**/*.rs", "src/gen"));
        assert!(!could_match_under("src/**/*.rs", "target"));
        assert!(!could_match_under("src/*.rs", ".git"));
        assert!(could_match_under("sr?/*.rs", "target"));
    }

    #[test]
    fn the_glob_setting_reads_ignored_files_unless_turned_off() {
        assert!(glob_reads_ignored(None));
        assert!(glob_reads_ignored(Some("true")));
        assert!(glob_reads_ignored(Some("1")));
        assert!(!glob_reads_ignored(Some("false")));
        assert!(!glob_reads_ignored(Some("0")));
        assert!(!glob_reads_ignored(Some(" OFF ")));
    }

    struct Slow;
    impl Answerer for Slow {
        fn answer(&self, _call: &SearchCall) -> Option<Answer> {
            thread::sleep(Duration::from_millis(500));
            Some(Answer {
                text: "late".to_string(),
                updated_input: None,
            })
        }
    }

    struct Panics;
    impl Answerer for Panics {
        fn answer(&self, _call: &SearchCall) -> Option<Answer> {
            panic!("answer blew up")
        }
    }

    fn call() -> SearchCall {
        SearchCall {
            tool: "Grep".to_string(),
            input: json!({"pattern": "x"}),
            cwd: None,
            legion_repo: None,
        }
    }

    #[test]
    fn an_answer_past_its_deadline_or_one_that_panics_is_none() {
        assert_eq!(
            answer_within(Arc::new(Slow), call(), Duration::from_millis(20)),
            None
        );
        assert_eq!(
            answer_within(Arc::new(Panics), call(), Duration::from_secs(5)),
            None
        );
        assert_eq!(
            answer_within(Arc::new(Slow), call(), Duration::from_secs(5))
                .map(|a| a.text)
                .as_deref(),
            Some("late")
        );
    }
}
