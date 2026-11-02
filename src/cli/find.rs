//! `legion find` (#1472): find's arguments, answered from sym's file
//! inventory, printed the way find prints them, so the rest of a pipeline
//! runs.
//!
//! A name query goes through `sym etc find-file` (`scan_find_file`), a
//! listing through `sym tree` (`scan_tree`). The expression is find's own
//! (`-name`, `-iname`, `-path`, `-type f|d`, `-maxdepth`, `-mindepth`, `-not`,
//! `-o`, parentheses, `-prune`, `-print`) and is judged against each candidate
//! path in find's order, so a pruned directory hides what is below it.
//!
//! Where `legion grep` falls back to the real tool, this never does:
//! `Bash(find:*)` stays in the operator's `permissions.deny`, so a form sym
//! cannot answer says what to use instead on stderr, naming the command, and
//! exits 1. That covers every other predicate and every operand the inventory
//! does not hold: outside a watched repo, or a gitignored tree such as `dist` or
//! `node_modules`, which would otherwise answer with an empty list that looks
//! like "nothing there". A refusal never goes to stdout, even when stderr is
//! discarded: in `find ... | xargs rm` its words would become arguments.
//!
//! The inventory is of a repo's registered checkout, so a linked worktree of it
//! is refused too and pointed at `git ls-files`: the registered checkout's list
//! is the wrong answer for another branch, and an empty one looks like nothing
//! there. An empty answer to a question that names a gitignored tree
//! (`-path '*/node_modules/*'`) is refused for the same reason.
//!
//! The inventory is also as of the last index, so each answer is checked against
//! the walk that builds it, run over the operand. A path on disk that the query
//! would print and the inventory does not hold makes the list short: that is
//! said on stderr, and refused outright when stderr is discarded, where saying
//! it would leave a short list with no warning. A new file the query would not
//! print changes nothing. A path gone from disk is simply not printed.
//!
//! Directories are not in the inventory: they are derived from the file
//! paths, so an empty directory, or one holding only gitignored files, is not
//! listed.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::cli::grep::print_answer;
use crate::cli::index_cmd;
use crate::cli::util::open_db;
use crate::db::inventory::{InventoryFilter, glob_match};
use crate::error::{LegionError, Result};
use crate::{data_dir, db, inventory, watch};

/// What every refusal lists: the part of find sym answers.
const ANSWERED: &str = "sym answers -name, -iname, -path, -type f|d, -maxdepth, -mindepth, -not, -o, parentheses, -prune and -print";

/// Predicates that act on a match or print it some other way.
const ACTIONS: [&str; 10] = [
    "-exec", "-execdir", "-ok", "-okdir", "-delete", "-print0", "-printf", "-fprint", "-fprintf",
    "-ls",
];

/// Predicates that read a file's metadata, which `ls -l` prints.
const METADATA: [&str; 12] = [
    "-mtime", "-mmin", "-atime", "-amin", "-ctime", "-cmin", "-size", "-empty", "-perm", "-user",
    "-group", "-newer",
];

/// Directories the inventory leaves out wherever a repo gitignores them, as
/// they usually do: a question naming one is not answered by an empty list.
const IGNORED_TREES: [&str; 5] = ["node_modules", "dist", "build", "target", ".git"];

/// Whether a candidate is a file or a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    File,
    Dir,
}

/// find's expression, as the part of it sym can judge.
#[derive(Debug, PartialEq, Eq)]
enum Expr {
    True,
    /// `-name` / `-iname`: the glob against the path's last component.
    Name {
        glob: String,
        fold: bool,
    },
    /// `-path` / `-ipath`: the glob against the whole path as printed.
    Path {
        glob: String,
        fold: bool,
    },
    Type(Kind),
    /// True, and a directory it is reached on is not descended into.
    Prune,
    /// True, and the path is printed. With one in the expression, nothing else
    /// prints.
    Print,
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
}

/// What evaluating the expression did to one candidate besides answer.
#[derive(Default)]
struct Effects {
    printed: bool,
    prune: bool,
}

/// One path the expression is judged against.
struct Candidate {
    /// The path as find prints it: the operand as typed, then the rest.
    shown: String,
    /// The path relative to the operand; the operand itself is empty.
    rel: String,
    /// Path segments below the operand; the operand itself is 0.
    depth: usize,
    kind: Kind,
}

impl Expr {
    fn and(left: Expr, right: Expr) -> Expr {
        match (left, right) {
            (Expr::True, other) | (other, Expr::True) => other,
            (left, right) => Expr::And(Box::new(left), Box::new(right)),
        }
    }

    /// The expression's answer for `candidate`, short-circuiting as find does:
    /// a `-prune` or `-print` not reached has no effect.
    fn eval(&self, candidate: &Candidate, effects: &mut Effects) -> bool {
        match self {
            Expr::True => true,
            Expr::Name { glob, fold } => glob_is(glob, *fold, last_component(&candidate.shown)),
            Expr::Path { glob, fold } => glob_is(glob, *fold, &candidate.shown),
            Expr::Type(kind) => candidate.kind == *kind,
            Expr::Prune => {
                effects.prune = true;
                true
            }
            Expr::Print => {
                effects.printed = true;
                true
            }
            Expr::Not(inner) => !inner.eval(candidate, effects),
            Expr::And(left, right) => {
                left.eval(candidate, effects) && right.eval(candidate, effects)
            }
            Expr::Or(left, right) => {
                left.eval(candidate, effects) || right.eval(candidate, effects)
            }
        }
    }

    /// Whether this expression, or any part of it, passes `test`.
    fn any(&self, test: &dyn Fn(&Expr) -> bool) -> bool {
        test(self)
            || match self {
                Expr::Not(inner) => inner.any(test),
                Expr::And(left, right) | Expr::Or(left, right) => left.any(test) || right.any(test),
                _ => false,
            }
    }

    /// Whether the question asks for a path inside a tree the inventory usually
    /// leaves out. Only what it asks for counts: `-not -path '*/dist/*'` and
    /// `-name dist -prune` are the agent leaving that tree out too.
    fn names_ignored_tree(&self) -> bool {
        match self {
            Expr::Name { glob, .. } | Expr::Path { glob, .. } => glob
                .split('/')
                .any(|part| IGNORED_TREES.contains(&part.trim_matches(['*', '?']))),
            Expr::And(..) if self.any(&|e| *e == Expr::Prune) => false,
            Expr::And(left, right) | Expr::Or(left, right) => {
                left.names_ignored_tree() || right.names_ignored_tree()
            }
            _ => false,
        }
    }

    /// The expressions a chain of `-a` joins, in order.
    fn conjuncts(&self) -> Vec<&Expr> {
        match self {
            Expr::And(left, right) => {
                let mut all: Vec<&Expr> = left.conjuncts();
                all.extend(right.conjuncts());
                all
            }
            other => vec![other],
        }
    }

    /// The glob when the whole question is "files by this name": `-name G`,
    /// with `-type f` or a closing `-print` or neither. That question is `sym
    /// etc find-file`'s. A glob holding `/` never matches a last component, but
    /// find-file would read it as a whole-path query, so it stays with the
    /// listing. A `-print` before the name prints whatever the name says, so
    /// that stays with the listing too.
    fn sole_name(&self) -> Option<&str> {
        let parts: Vec<&Expr> = self.conjuncts();
        let parts: &[&Expr] = match parts.split_last() {
            Some((Expr::Print, rest)) => rest,
            _ => &parts,
        };
        let mut name: Option<&str> = None;
        for part in parts {
            match part {
                Expr::Type(Kind::File) => {}
                Expr::Name { glob, fold: false } if name.is_none() && !glob.contains('/') => {
                    name = Some(glob);
                }
                _ => return None,
            }
        }
        name
    }
}

/// `glob` against `text`, folding case when `fold`.
fn glob_is(glob: &str, fold: bool, text: &str) -> bool {
    let chars = |s: &str| -> Vec<char> {
        if fold {
            s.to_lowercase().chars().collect()
        } else {
            s.chars().collect()
        }
    };
    glob_match(&chars(glob), &chars(text))
}

/// The last component of a printed path: `src/` and `src` are both `src`, and
/// `/` is itself.
fn last_component(shown: &str) -> &str {
    let trimmed: &str = shown.trim_end_matches('/');
    if trimmed.is_empty() {
        return shown;
    }
    trimmed.rsplit('/').next().unwrap_or(trimmed)
}

/// find's argv, read: the operands as typed, the expression and the depth
/// bounds, which find applies however the expression comes out.
#[derive(Debug, PartialEq, Eq)]
struct Query {
    operands: Vec<String>,
    expr: Expr,
    max_depth: Option<usize>,
    min_depth: usize,
}

/// The expression parser: find's grammar, with `-o` binding loosest, then
/// `-a` (spelled or implied), then `!`.
struct Parser<'a> {
    words: &'a [&'a str],
    at: usize,
    max_depth: Option<usize>,
    min_depth: usize,
}

/// `Err` is the word sym cannot answer.
impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&'a str> {
        self.words.get(self.at).copied()
    }

    fn next_value(&mut self, predicate: &str) -> std::result::Result<&'a str, String> {
        let value: Option<&'a str> = self.peek();
        self.at += 1;
        value.ok_or_else(|| predicate.to_string())
    }

    fn or(&mut self) -> std::result::Result<Expr, String> {
        let mut left: Expr = self.and()?;
        while matches!(self.peek(), Some("-o" | "-or")) {
            self.at += 1;
            let right: Expr = self.and()?;
            left = Expr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn and(&mut self) -> std::result::Result<Expr, String> {
        let mut left: Expr = self.not()?;
        loop {
            match self.peek() {
                None | Some("-o" | "-or" | ")") => return Ok(left),
                Some("-a" | "-and") => self.at += 1,
                Some(_) => {}
            }
            left = Expr::and(left, self.not()?);
        }
    }

    fn not(&mut self) -> std::result::Result<Expr, String> {
        if matches!(self.peek(), Some("!" | "-not")) {
            self.at += 1;
            return Ok(Expr::Not(Box::new(self.not()?)));
        }
        self.primary()
    }

    fn primary(&mut self) -> std::result::Result<Expr, String> {
        let word: &str = self.next_value("the end of the expression")?;
        match word {
            "(" => {
                let inner: Expr = self.or()?;
                match self.next_value("an unclosed `(`")? {
                    ")" => Ok(inner),
                    other => Err(other.to_string()),
                }
            }
            "-name" | "-iname" => Ok(Expr::Name {
                glob: self.glob(word)?,
                fold: word == "-iname",
            }),
            "-path" | "-ipath" | "-wholename" | "-iwholename" => Ok(Expr::Path {
                glob: self.glob(word)?,
                fold: word.starts_with("-i"),
            }),
            "-type" => match self.next_value(word)? {
                "f" => Ok(Expr::Type(Kind::File)),
                "d" => Ok(Expr::Type(Kind::Dir)),
                other => Err(format!("-type {other}")),
            },
            "-prune" => Ok(Expr::Prune),
            "-print" => Ok(Expr::Print),
            "-maxdepth" => {
                self.max_depth = Some(self.depth(word)?);
                Ok(Expr::True)
            }
            "-mindepth" => {
                self.min_depth = self.depth(word)?;
                Ok(Expr::True)
            }
            other => Err(other.to_string()),
        }
    }

    /// A glob's value. Only `*` and `?` are wildcards to the matcher
    /// find-file uses; a bracket or an escape means something else to find.
    fn glob(&mut self, predicate: &str) -> std::result::Result<String, String> {
        let value: &str = self.next_value(predicate)?;
        if value.contains(['[', ']', '\\']) {
            return Err(format!("{predicate} '{value}'"));
        }
        Ok(value.to_string())
    }

    fn depth(&mut self, predicate: &str) -> std::result::Result<usize, String> {
        let value: &str = self.next_value(predicate)?;
        value
            .parse::<usize>()
            .map_err(|_| format!("{predicate} {value}"))
    }
}

/// Read find's argv. `Err` is the refusal, already naming what to use.
fn parse(args: &[OsString]) -> std::result::Result<Query, String> {
    let words: Vec<&str> = args
        .iter()
        .map(|a| a.to_str())
        .collect::<Option<_>>()
        .ok_or_else(|| refusal_for_word("a word that is not UTF-8", &["<path>".to_string()]))?;
    let split: usize = words
        .iter()
        .position(|w| w.starts_with('-') || *w == "(" || *w == "!")
        .unwrap_or(words.len());
    let (typed, expression): (&[&str], &[&str]) = words.split_at(split);
    let operands: Vec<String> = if typed.is_empty() {
        vec![".".to_string()]
    } else {
        typed.iter().map(|w| (*w).to_string()).collect()
    };
    let mut parser = Parser {
        words: expression,
        at: 0,
        max_depth: None,
        min_depth: 0,
    };
    let expr: std::result::Result<Expr, String> = if expression.is_empty() {
        Ok(Expr::True)
    } else {
        parser.or().and_then(|expr| match parser.peek() {
            Some(left_over) => Err(left_over.to_string()),
            None => Ok(expr),
        })
    };
    match expr {
        Ok(expr) => Ok(Query {
            operands,
            expr,
            max_depth: parser.max_depth,
            min_depth: parser.min_depth,
        }),
        Err(word) => Err(refusal_for_word(&word, &operands)),
    }
}

/// The refusal for a word sym cannot answer: what to use instead for that
/// kind of predicate, each a command that works as written.
fn refusal_for_word(word: &str, operands: &[String]) -> String {
    let paths: String = operands.join(" ");
    let predicate: &str = word.split(' ').next().unwrap_or(word);
    let instead: String = if ACTIONS.contains(&predicate) {
        format!(
            "select the paths and hand them on: `legion find {paths} -name '<glob>' | xargs <command>`"
        )
    } else if METADATA.contains(&predicate) || predicate.starts_with("-newer") {
        format!("`ls -lR {paths}` prints size, times, owner and mode")
    } else if matches!(predicate, "-regex" | "-iregex") {
        "`legion sym tree --repo <repo> | legion grep -E '<regex>'`".to_string()
    } else if word.contains(['[', ']', '\\']) {
        format!(
            "spell each alternative with `*` and `?`: `legion find {paths} -name 'a*' -o -name 'b*'`"
        )
    } else {
        format!("`ls -R {paths}` lists it")
    };
    format!("legion find: sym cannot answer `{word}`. {ANSWERED}. Instead: {instead}")
}

/// Where an operand stands relative to the watched repos.
#[derive(Debug, PartialEq, Eq)]
enum Standing {
    /// In a repo's registered checkout, whose inventory answers for it.
    Registered {
        repo: String,
        workdir: PathBuf,
    },
    /// In a linked worktree of a repo: a different branch's files.
    Worktree {
        repo: String,
        workdir: PathBuf,
    },
    Outside,
}

/// Where `root` (canonical) stands. A worktree can live inside its repo's
/// workdir (`.claude/worktrees/...`), so a checkout other than the deepest
/// watched workdir above `root` is looked at first: when it shares a watched
/// repo's git common dir it is that repo's worktree. A nested checkout that is
/// no watched repo's stays with the workdir above it.
fn standing(root: &Path, repos: &[watch::WatchRepoConfig]) -> Standing {
    let canonical: Vec<(&str, PathBuf)> = repos
        .iter()
        .filter_map(|r| {
            std::fs::canonicalize(&r.workdir)
                .ok()
                .map(|w| (r.name.as_str(), w))
        })
        .collect();
    let deepest: Option<&(&str, PathBuf)> = canonical
        .iter()
        .filter(|(_, w)| root.starts_with(w))
        .max_by_key(|(_, w)| w.as_os_str().len());
    let top: Option<PathBuf> =
        inventory::current_toplevel(root).and_then(|top| std::fs::canonicalize(top).ok());
    if let Some(top) = top.filter(|top| deepest.is_none_or(|(_, w)| w != top)) {
        let common: Option<PathBuf> = inventory::git_common_dir(&top);
        let shared = canonical
            .iter()
            .find(|(_, w)| common.is_some() && inventory::git_common_dir(w) == common);
        if let Some((repo, workdir)) = shared {
            return Standing::Worktree {
                repo: (*repo).to_string(),
                workdir: workdir.clone(),
            };
        }
    }
    match deepest {
        Some((repo, workdir)) => Standing::Registered {
            repo: (*repo).to_string(),
            workdir: workdir.clone(),
        },
        None => Standing::Outside,
    }
}

/// The ancestors of `rel` below the operand, as `/`-separated paths.
fn ancestors(rel: &str) -> impl Iterator<Item = &str> {
    rel.char_indices()
        .filter(|(_, c)| *c == '/')
        .map(|(at, _)| &rel[..at])
}

/// `operand` with `rel` below it, the way find prints it.
fn shown(operand: &str, rel: &str) -> String {
    if rel.is_empty() {
        operand.to_string()
    } else if operand.ends_with('/') {
        format!("{operand}{rel}")
    } else {
        format!("{operand}/{rel}")
    }
}

/// `path` relative to `under`, or the whole of it at the checkout's top.
fn below<'a>(path: &'a str, under: Option<&str>) -> &'a str {
    match under.and_then(|u| path.strip_prefix(u)) {
        Some(rest) => rest.trim_start_matches('/'),
        None => path,
    }
}

/// The paths find would judge under one operand: the files in `files`, and the
/// operand and every directory the `tree` listing implies. An operand that is
/// itself a file is the one candidate, at depth 0.
fn candidates(
    operand: &str,
    under: Option<&str>,
    files: &[String],
    tree: &[String],
) -> Vec<Candidate> {
    let mut dirs: BTreeSet<&str> = BTreeSet::new();
    let mut is_a_file: bool = false;
    for path in tree {
        let rel: &str = below(path, under);
        is_a_file |= rel.is_empty();
        dirs.extend(ancestors(rel));
    }
    if !is_a_file {
        dirs.insert("");
    }
    let depth_of = |rel: &str| -> usize {
        if rel.is_empty() {
            0
        } else {
            rel.split('/').count()
        }
    };
    let mut all: Vec<Candidate> = dirs
        .into_iter()
        .map(|rel| Candidate {
            shown: shown(operand, rel),
            rel: rel.to_string(),
            depth: depth_of(rel),
            kind: Kind::Dir,
        })
        .chain(files.iter().map(|path| {
            let rel: &str = below(path, under);
            Candidate {
                shown: shown(operand, rel),
                rel: rel.to_string(),
                depth: depth_of(rel),
                kind: Kind::File,
            }
        }))
        .collect();
    all.sort_by(|a, b| a.shown.cmp(&b.shown));
    all
}

/// Whether `rel` is below the directory `dir`, both relative to the operand.
fn is_below(rel: &str, dir: &str) -> bool {
    if dir.is_empty() {
        !rel.is_empty()
    } else {
        rel.strip_prefix(dir).is_some_and(|r| r.starts_with('/'))
    }
}

/// What find prints of `all`, which holds a directory before what is below it:
/// walked in that order, past what a `-prune` closed, within the depth bounds,
/// which find checks before it evaluates anything.
fn select(query: &Query, all: Vec<Candidate>) -> Vec<Candidate> {
    let explicit: bool = query.expr.any(&|e| *e == Expr::Print);
    let mut pruned: Vec<String> = Vec::new();
    let mut kept: Vec<Candidate> = Vec::new();
    for candidate in all {
        if pruned.iter().any(|dir| is_below(&candidate.rel, dir))
            || candidate.depth < query.min_depth
            || query.max_depth.is_some_and(|max| candidate.depth > max)
        {
            continue;
        }
        let mut effects = Effects::default();
        let answer: bool = query.expr.eval(&candidate, &mut effects);
        if effects.prune && candidate.kind == Kind::Dir {
            pruned.push(candidate.rel.clone());
        }
        if if explicit { effects.printed } else { answer } {
            kept.push(candidate);
        }
    }
    kept
}

/// The paths on disk that `query` would print under `root` and `answer` does
/// not hold, from the walk that builds the inventory, with how many entries the
/// walk could not read. Respects the same gitignore rules as the index, so a
/// build output never counts.
fn missing_from_answer(
    query: &Query,
    repo: &str,
    operand: &str,
    root: &Path,
    answer: &[String],
) -> (Vec<String>, u64) {
    let walked: inventory::WalkOutcome = inventory::walk_repo(repo, root);
    let files: Vec<String> = walked.entries.into_iter().map(|e| e.path).collect();
    let held: BTreeSet<&str> = answer.iter().map(String::as_str).collect();
    let missing: Vec<String> = select(query, candidates(operand, None, &files, &files))
        .into_iter()
        .map(|c| c.shown)
        .filter(|shown| !held.contains(shown.as_str()))
        .collect();
    (missing, walked.walk_errors)
}

/// The lines for one operand, or the refusal that names what to use instead.
fn answer_operand(
    database: &db::Database,
    query: &Query,
    operand: &str,
    repos: &[watch::WatchRepoConfig],
) -> std::result::Result<Vec<String>, String> {
    if std::fs::symlink_metadata(operand).is_err() {
        return Err(format!(
            "legion find: '{operand}': No such file or directory"
        ));
    }
    let on_disk = || format!("`ls -R {operand}` lists it on disk");
    let root: PathBuf = std::fs::canonicalize(operand).map_err(|e| {
        format!(
            "legion find: cannot resolve '{operand}': {e}. Instead: {}",
            on_disk()
        )
    })?;
    let (repo, workdir): (String, PathBuf) = match standing(&root, repos) {
        Standing::Registered { repo, workdir } => (repo, workdir),
        Standing::Worktree { repo, workdir } => {
            return Err(format!(
                "legion find: '{operand}' is in a linked worktree of {repo}, and sym's inventory is of the registered checkout {}, not this branch. Instead: `git -C {operand} ls-files` lists the tracked files here (add a pathspec such as '*.rs'), or {}",
                workdir.display(),
                on_disk()
            ));
        }
        Standing::Outside => {
            return Err(format!(
                "legion find: '{operand}' is outside every watched repo, so sym holds no inventory of it. Instead: {}, or `legion sym etc find-file '<glob>'` searches the watched repos",
                on_disk()
            ));
        }
    };
    let unread = |e: LegionError| {
        format!(
            "legion find: could not read the inventory of {}: {e}. Instead: {}",
            repo,
            on_disk()
        )
    };
    let rel: Option<String> = root
        .strip_prefix(&workdir)
        .ok()
        .and_then(Path::to_str)
        .map(|r| r.replace('\\', "/"));
    let Some(rel) = rel else {
        return Err(format!(
            "legion find: '{operand}' is not a path sym can name. Instead: {}",
            on_disk()
        ));
    };
    let under: Option<&str> = (!rel.is_empty()).then_some(rel.as_str());

    let tree: Vec<String> = index_cmd::scan_tree(database, Some(&repo), None, under, None)
        .map_err(unread)?
        .entries
        .into_iter()
        .map(|e| e.path)
        .collect();
    if tree.is_empty() {
        let filter = InventoryFilter {
            repo: Some(&repo),
            ext: None,
            lang: None,
        };
        let indexed: bool = !database
            .list_file_inventory(&filter)
            .map_err(unread)?
            .is_empty();
        return Err(if indexed {
            format!(
                "legion find: '{operand}' holds no indexed file; the inventory leaves out gitignored trees such as dist and node_modules. Instead: {}",
                on_disk()
            )
        } else {
            format!(
                "legion find: {} has no inventory yet. Instead: `legion index {}`, then find again, or {}",
                repo,
                repo,
                on_disk()
            )
        });
    }
    // The tree is read either way: it says the operand is covered and which
    // directories exist. The files by name are find-file's answer.
    let files: Vec<String> = match query.expr.sole_name() {
        Some(glob) => index_cmd::scan_find_file(database, Some(&repo), glob, None)
            .map_err(unread)?
            .entries
            .into_iter()
            .map(|e| e.path)
            .filter(|path| under.is_none_or(|u| index_cmd::under_matches(path, u)))
            .collect(),
        None => tree.clone(),
    };

    // The inventory is as of the last index: a file gone since then is not
    // printed for the next command to trip on.
    let base: PathBuf = under.map_or_else(|| workdir.clone(), |u| workdir.join(u));
    let lines: Vec<String> = select(query, candidates(operand, under, &files, &tree))
        .into_iter()
        // Joining an empty `rel` would add a `/` a file operand cannot take.
        .filter(|c| c.rel.is_empty() || base.join(&c.rel).symlink_metadata().is_ok())
        .map(|c| c.shown)
        .collect();

    let (missing, walk_errors): (Vec<String>, u64) =
        missing_from_answer(query, &repo, operand, &root, &lines);
    if !missing.is_empty() || walk_errors > 0 {
        let shown: String = missing
            .iter()
            .take(3)
            .map(String::as_str)
            .collect::<Vec<&str>>()
            .join(", ");
        let behind: String = if missing.is_empty() {
            format!(
                "{walk_errors} entries under '{operand}' could not be read, so this may be short"
            )
        } else {
            format!(
                "{} path(s) on disk match and are not in this answer, such as {shown}",
                missing.len()
            )
        };
        let warning: String = format!(
            "legion find: the inventory of {repo} is behind the disk: {behind}. Instead: `legion index {repo}`, then find again, or {}",
            on_disk()
        );
        if stderr_discarded() {
            return Err(warning);
        }
        eprintln!("{warning}");
        return Ok(lines);
    }
    if lines.is_empty() {
        if query.expr.names_ignored_tree() {
            return Err(format!(
                "legion find: this asks for a tree the inventory of {repo} leaves out when gitignored, such as dist or node_modules, and sym holds nothing there. Instead: {}",
                on_disk()
            ));
        }
        eprintln!(
            "[legion] find: nothing in the inventory of {repo} matches; it leaves out gitignored trees such as dist and node_modules, and `ls -R {operand}` looks on disk"
        );
    }
    Ok(lines)
}

/// Whether stderr goes nowhere, as `2>/dev/null` sends it: a warning said there
/// is not said at all.
fn stderr_discarded() -> bool {
    #[cfg(unix)]
    {
        use std::os::fd::AsFd;
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let Ok(fd) = std::io::stderr().as_fd().try_clone_to_owned() else {
            return false;
        };
        let (Ok(stderr), Ok(null)) = (
            std::fs::File::from(fd).metadata(),
            std::fs::metadata("/dev/null"),
        ) else {
            return false;
        };
        stderr.file_type().is_char_device() && stderr.rdev() == null.rdev()
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Answer `args`: find's lines for what sym holds, a refusal for what it does
/// not, and exit 1 if any operand was refused, as find does when it could not
/// read one and went on with the rest.
fn run(args: &[OsString]) -> Result<()> {
    let query: Query = match parse(args) {
        Ok(query) => query,
        Err(refusal) => {
            eprintln!("{refusal}");
            return Err(LegionError::ExitWith(1));
        }
    };
    let database: db::Database = match open_db() {
        Ok(database) => database,
        Err(e) => {
            eprintln!(
                "legion find: could not open the legion database: {e}. Instead: `ls -R {}` lists it on disk",
                query.operands.join(" ")
            );
            return Err(LegionError::ExitWith(1));
        }
    };
    let repos: Vec<watch::WatchRepoConfig> =
        watch::list_repos_in_config(&data_dir()?.join("watch.toml")).unwrap_or_default();
    let mut lines: Vec<String> = Vec::new();
    let mut refused: bool = false;
    for operand in &query.operands {
        match answer_operand(&database, &query, operand, &repos) {
            Ok(mut answered) => lines.append(&mut answered),
            Err(refusal) => {
                eprintln!("{refusal}");
                refused = true;
            }
        }
    }
    print_answer(&lines, true)?;
    if refused {
        return Err(LegionError::ExitWith(1));
    }
    Ok(())
}

/// Dispatch `legion find ...` from the raw process argv, before clap sees it:
/// clap would read `-name` as legion's own flags. `None` when argv[1] is not
/// `find`, so the caller parses normally.
pub(crate) fn run_from_argv(argv: &[OsString]) -> Option<Result<()>> {
    if argv.get(1).is_none_or(|first| first != "find") {
        return None;
    }
    Some(run(argv.get(2..).unwrap_or(&[])))
}

/// Handle `legion find` reached through clap (behind a leading legion flag).
/// Same behavior as the raw-argv path.
pub(crate) fn handle_find(args: Vec<OsString>) -> Result<()> {
    run(&args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    fn query(args: &[&str]) -> Query {
        parse(&os(args)).expect("answerable")
    }

    fn file(shown: &str, depth: usize) -> Candidate {
        Candidate {
            shown: shown.to_string(),
            rel: String::new(),
            depth,
            kind: Kind::File,
        }
    }

    fn dir(shown: &str, depth: usize) -> Candidate {
        Candidate {
            shown: shown.to_string(),
            rel: String::new(),
            depth,
            kind: Kind::Dir,
        }
    }

    /// The expression's answer for one candidate.
    fn judge(query: &Query, candidate: &Candidate) -> bool {
        query.expr.eval(candidate, &mut Effects::default())
    }

    #[test]
    fn run_from_argv_claims_only_find() {
        assert!(run_from_argv(&os(&["legion", "recall"])).is_none());
        assert!(run_from_argv(&os(&["legion"])).is_none());
        assert!(run_from_argv(&os(&["legion", "-v", "find"])).is_none());
    }

    #[test]
    fn operands_stop_at_the_expression_and_default_to_the_working_directory() {
        assert_eq!(query(&[]).operands, vec!["."]);
        assert_eq!(query(&["-name", "x"]).operands, vec!["."]);
        assert_eq!(query(&["a", "b", "-type", "f"]).operands, vec!["a", "b"]);
        assert_eq!(query(&["a", "(", "-name", "x", ")"]).operands, vec!["a"]);
        assert_eq!(query(&["a", "!", "-name", "x"]).operands, vec!["a"]);
    }

    #[test]
    fn depth_bounds_are_read_wherever_they_stand() {
        let q: Query = query(&[".", "-name", "x", "-maxdepth", "2", "-mindepth", "1"]);
        assert_eq!(q.max_depth, Some(2));
        assert_eq!(q.min_depth, 1);
        assert_eq!(q.expr.sole_name(), Some("x"));
    }

    #[test]
    fn minus_o_binds_looser_than_and() {
        let q: Query = query(&["-type", "f", "-name", "a", "-o", "-name", "b"]);
        // (f and a) or b: a directory named b matches, a directory named a
        // does not.
        assert!(judge(&q, &dir("x/b", 1)));
        assert!(!judge(&q, &dir("x/a", 1)));
        assert!(judge(&q, &file("x/a", 1)));

        let grouped: Query = query(&["-type", "f", "(", "-name", "a", "-o", "-name", "b", ")"]);
        assert!(!judge(&grouped, &dir("x/b", 1)));
        assert!(judge(&grouped, &file("x/b", 1)));
    }

    #[test]
    fn not_and_path_judge_the_path_as_printed() {
        let q: Query = query(&[".", "-type", "f", "-not", "-path", "*/gen/*"]);
        assert!(judge(&q, &file("./src/a.rs", 2)));
        assert!(!judge(&q, &file("./gen/a.rs", 2)));
        let bang: Query = query(&[".", "!", "-path", "./gen/*"]);
        assert!(!judge(&bang, &file("./gen/a.rs", 2)));
        assert!(judge(&bang, &file("gen/a.rs", 2)));
    }

    #[test]
    fn iname_folds_case_and_name_does_not() {
        let fold: Query = query(&["-iname", "*READ*"]);
        assert!(judge(&fold, &file("./readme.md", 1)));
        let exact: Query = query(&["-name", "*READ*"]);
        assert!(!judge(&exact, &file("./readme.md", 1)));
    }

    #[test]
    fn name_reads_the_last_component_of_an_operand_too() {
        let q: Query = query(&["-name", "src"]);
        assert!(judge(&q, &dir("src/", 0)));
        assert!(judge(&q, &dir("a/src", 1)));
        assert!(!judge(&q, &dir("src/a", 1)));
        assert_eq!(last_component("/"), "/");
        assert_eq!(last_component("."), ".");
    }

    #[test]
    fn naming_an_ignored_tree_counts_only_when_it_is_asked_for() {
        for args in [
            &["-path", "*/node_modules/zod/package.json"][..],
            &["-name", "dist"],
            &["-type", "d", "-name", "target"],
            &["-name", "x", "-o", "-path", "./build/*"],
            &["-path", "*node_modules/zod/package.json"],
        ] {
            assert!(query(args).expr.names_ignored_tree(), "{args:?}");
        }
        for args in [
            &["-not", "-path", "*/node_modules/*"][..],
            &["-name", "*.rs", "!", "-path", "*/target/*"],
            &["-name", "distant"],
            &["-name", "dist", "-prune", "-o", "-type", "f"],
            &["-type", "f"],
        ] {
            assert!(!query(args).expr.names_ignored_tree(), "{args:?}");
        }
    }

    #[test]
    fn only_a_plain_name_is_find_files_question() {
        assert_eq!(query(&["-name", "*.rs"]).expr.sole_name(), Some("*.rs"));
        assert_eq!(
            query(&["-type", "f", "-name", "*.rs"]).expr.sole_name(),
            Some("*.rs")
        );
        assert_eq!(
            query(&["-name", "*.rs", "-type", "f"]).expr.sole_name(),
            Some("*.rs")
        );
        for args in [
            &["-iname", "x"][..],
            &["-name", "a/b"],
            &["-type", "d", "-name", "x"],
            &["-name", "x", "-o", "-name", "y"],
            &["-not", "-name", "x"],
            &["-type", "f"],
            &[],
        ] {
            assert_eq!(query(args).expr.sole_name(), None, "{args:?}");
        }
    }

    #[test]
    fn a_closing_print_keeps_a_name_find_files_question() {
        assert_eq!(query(&["-name", "x", "-print"]).expr.sole_name(), Some("x"));
        assert_eq!(query(&["-print"]).expr.sole_name(), None);
        // A -print first prints everything, whatever the name says.
        assert_eq!(query(&["-print", "-name", "x"]).expr.sole_name(), None);
        assert_eq!(
            query(&["-name", "x", "-o", "-name", "y", "-print"])
                .expr
                .sole_name(),
            None
        );
    }

    /// `select` over a small tree, as shown paths.
    fn selected(args: &[&str]) -> Vec<String> {
        let tree: Vec<String> = ["a.rs", "gen/b.rs", "gen/deep/c.rs", "src/d.rs"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        select(&query(args), candidates(".", None, &tree, &tree))
            .into_iter()
            .map(|c| c.shown)
            .collect()
    }

    #[test]
    fn a_pruned_directory_hides_what_is_below_it_and_is_itself_judged() {
        // No -print: the expression is true on the pruned directory, so find
        // prints it.
        assert_eq!(
            selected(&[".", "-name", "gen", "-prune", "-o", "-type", "f"]),
            vec!["./a.rs", "./gen", "./src/d.rs"]
        );
        // With -print, only what reaches it prints.
        assert_eq!(
            selected(&[".", "-name", "gen", "-prune", "-o", "-type", "f", "-print"]),
            vec!["./a.rs", "./src/d.rs"]
        );
        assert_eq!(
            selected(&[
                ".",
                "-path",
                "./gen/deep",
                "-prune",
                "-o",
                "-name",
                "*.rs",
                "-print"
            ]),
            vec!["./a.rs", "./gen/b.rs", "./src/d.rs"]
        );
        // Pruning the operand leaves the operand.
        assert_eq!(selected(&[".", "-prune"]), vec!["."]);
        // -prune on a file does nothing but answer true.
        assert_eq!(selected(&["-name", "a.rs", "-prune"]), vec!["./a.rs"]);
        // Not reached, no effect: -a stops at the false -name.
        assert_eq!(
            selected(&["-name", "absent", "-prune", "-o", "-name", "c.rs"]),
            vec!["./gen/deep/c.rs"]
        );
        // Below -mindepth the expression is not evaluated, so nothing prunes.
        assert_eq!(
            selected(&[".", "-mindepth", "1", "-maxdepth", "1", "-prune"]),
            vec!["./a.rs", "./gen", "./src"]
        );
    }

    #[test]
    fn is_below_respects_the_path_boundary() {
        assert!(is_below("gen/a", "gen"));
        assert!(!is_below("gen", "gen"));
        assert!(!is_below("generated/a", "gen"));
        assert!(is_below("a", ""));
        assert!(!is_below("", ""));
    }

    #[test]
    fn every_refusal_names_a_command_to_use() {
        for (args, command) in [
            (
                &["src", "-name", "x", "-exec", "cat", "{}", ";"][..],
                "legion find src -name",
            ),
            (&["src", "-delete"], "| xargs"),
            (&["src", "-print0"], "| xargs"),
            (&["src", "-mtime", "-1"], "ls -lR src"),
            (&["src", "-newermt", "2026-01-01"], "ls -lR src"),
            (&["src", "-size", "+1M"], "ls -lR src"),
            (&["src", "-regex", ".*"], "legion sym tree"),
            (&["src", "-name", "*.[ch]"], "legion find src -name 'a*' -o"),
            (&["src", "-name", "a\\*"], "legion find src -name 'a*' -o"),
            (&["src", "-type", "l"], "ls -R src"),
            (&["src", "-maxdepth", "two"], "ls -R src"),
            (&["-L", "src"], "ls -R ."),
            (&["--help"], "ls -R ."),
            (&["src", "-name"], "ls -R src"),
            (&["src", "(", "-name", "x"], "ls -R src"),
            (&["src", "-name", "x", "stray"], "ls -R src"),
        ] {
            let refusal: String = parse(&os(args)).expect_err("refused");
            assert!(refusal.contains(command), "{args:?}: {refusal}");
            assert!(
                refusal.contains("legion find: sym cannot answer"),
                "{refusal}"
            );
            assert!(refusal.contains(ANSWERED), "{refusal}");
        }
    }

    fn lines(operand: &str, under: Option<&str>, tree: &[&str]) -> Vec<(String, usize, Kind)> {
        let tree: Vec<String> = tree.iter().map(|s| s.to_string()).collect();
        candidates(operand, under, &tree, &tree)
            .into_iter()
            .map(|c| (c.shown, c.depth, c.kind))
            .collect()
    }

    #[test]
    fn candidates_are_the_operand_its_directories_and_its_files() {
        assert_eq!(
            lines(".", None, &["a.rs", "src/b/c.rs"]),
            vec![
                (".".to_string(), 0, Kind::Dir),
                ("./a.rs".to_string(), 1, Kind::File),
                ("./src".to_string(), 1, Kind::Dir),
                ("./src/b".to_string(), 2, Kind::Dir),
                ("./src/b/c.rs".to_string(), 3, Kind::File),
            ]
        );
    }

    #[test]
    fn candidates_are_relative_to_the_operand_as_typed() {
        assert_eq!(
            lines("src/", Some("src"), &["src/b/c.rs"]),
            vec![
                ("src/".to_string(), 0, Kind::Dir),
                ("src/b".to_string(), 1, Kind::Dir),
                ("src/b/c.rs".to_string(), 2, Kind::File),
            ]
        );
    }

    #[test]
    fn an_operand_that_is_a_file_is_its_own_one_candidate() {
        assert_eq!(
            lines("src/a.rs", Some("src/a.rs"), &["src/a.rs"]),
            vec![("src/a.rs".to_string(), 0, Kind::File)]
        );
    }
}
