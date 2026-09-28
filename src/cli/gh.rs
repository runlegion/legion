//! `legion gh` (#1336): a proxy that takes gh's own arguments.
//!
//! A write whose arguments translate losslessly runs through the existing
//! `legion pr` / `legion issue` / `legion comment` handlers, so their gates
//! hold. A read runs the real gh untouched and records nothing. Any other
//! write runs the real gh after the gate its legion verb would enforce, and
//! records one `gh-passthrough` audit row.
//!
//! `plan` is pure: it classifies from argv alone. Everything that touches
//! disk, network, or config happens in `handle`, after `plan` has decided.

use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};

use crate::cli::issue::{self, IssueAction};
use crate::cli::pr::{self, PrAction};
use crate::cli::util::{audit, git_head_commit_and_branch, open_db};
use crate::verify::GateResult;
use crate::{db, error, worksource};

/// What `legion gh` does with an argv, decided from the argv alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GhPlan {
    /// Run real gh; no audit row.
    Read,
    /// Translate to a legion verb, after resolution.
    Write(GhWrite),
    /// Run real gh, then write one audit row.
    Passthrough,
}

/// Which PR a `pr` subcommand acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PrRef {
    Number(u64),
    CurrentBranch,
}

/// Where a body comes from. `-F -` is `Stdin`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BodySource {
    Inline(String),
    File(PathBuf),
    Stdin,
}

/// A gh write whose every argument has a legion equivalent.
///
/// `repo` is gh's -R/--repo value (owner/name), None when absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GhWrite {
    PrCreate {
        repo: Option<String>,
        title: String,
        body: BodySource,
        base: Option<String>,
        head: Option<String>,
        draft: bool,
        labels: Vec<String>,
        reviewer: Option<String>,
    },
    PrEdit {
        repo: Option<String>,
        pr: PrRef,
        title: String,
    },
    PrMerge {
        repo: Option<String>,
        pr: PrRef,
        strategy: String,
        delete_branch: bool,
    },
    PrClose {
        repo: Option<String>,
        pr: PrRef,
        comment: Option<String>,
        delete_branch: bool,
    },
    PrReview {
        repo: Option<String>,
        pr: PrRef,
        event: ReviewEvent,
        body: Option<BodySource>,
    },
    PrComment {
        repo: Option<String>,
        pr: PrRef,
        body: BodySource,
    },
    IssueCreate {
        repo: Option<String>,
        title: String,
        body: BodySource,
        labels: Vec<String>,
        assignee: Option<String>,
    },
    IssueEdit {
        repo: Option<String>,
        number: u64,
        title: Option<String>,
        body: Option<BodySource>,
    },
    IssueClose {
        repo: Option<String>,
        number: u64,
        comment: Option<String>,
    },
    IssueReopen {
        repo: Option<String>,
        number: u64,
        comment: Option<String>,
    },
    IssueComment {
        repo: Option<String>,
        number: u64,
        body: BodySource,
    },
}

/// The review a `pr review` submits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReviewEvent {
    Approve,
    RequestChanges,
    Comment,
}

// ---------------------------------------------------------------------------
// Flag tables. gh reuses short flags with different meanings per subcommand
// (-r is reviewer, rebase, request-changes, or reason), so each subcommand
// carries its own table. Each lists gh's full flag set for that subcommand,
// not only the translatable flags, so a recognized-but-untranslatable flag
// still parses (its value is not mistaken for a positional) and the command
// lands in Passthrough by the row check, not by a misparse.
// ---------------------------------------------------------------------------

/// `(long, short, takes_value)`.
type Flag = (&'static str, Option<char>, bool);

/// gh's -R/--repo, accepted by every table below.
const REPO: Flag = ("repo", Some('R'), true);

const PR_CREATE: &[Flag] = &[
    ("assignee", Some('a'), true),
    ("base", Some('B'), true),
    ("body", Some('b'), true),
    ("body-file", Some('F'), true),
    ("draft", Some('d'), false),
    ("dry-run", None, false),
    ("editor", Some('e'), false),
    ("fill", Some('f'), false),
    ("fill-first", None, false),
    ("fill-verbose", None, false),
    ("head", Some('H'), true),
    ("label", Some('l'), true),
    ("milestone", Some('m'), true),
    ("no-maintainer-edit", None, false),
    ("project", Some('p'), true),
    ("recover", None, true),
    ("reviewer", Some('r'), true),
    ("template", Some('T'), true),
    ("title", Some('t'), true),
    ("web", Some('w'), false),
];

const PR_EDIT: &[Flag] = &[
    ("add-assignee", None, true),
    ("add-label", None, true),
    ("add-project", None, true),
    ("add-reviewer", None, true),
    ("base", Some('B'), true),
    ("body", Some('b'), true),
    ("body-file", Some('F'), true),
    ("milestone", Some('m'), true),
    ("remove-assignee", None, true),
    ("remove-label", None, true),
    ("remove-milestone", None, false),
    ("remove-project", None, true),
    ("remove-reviewer", None, true),
    ("title", Some('t'), true),
];

const PR_MERGE: &[Flag] = &[
    ("admin", None, false),
    ("author-email", Some('A'), true),
    ("auto", None, false),
    ("body", Some('b'), true),
    ("body-file", Some('F'), true),
    ("delete-branch", Some('d'), false),
    ("disable-auto", None, false),
    ("match-head-commit", None, true),
    ("merge", Some('m'), false),
    ("rebase", Some('r'), false),
    ("squash", Some('s'), false),
    ("subject", Some('t'), true),
];

const PR_CLOSE: &[Flag] = &[
    ("comment", Some('c'), true),
    ("delete-branch", Some('d'), false),
];

const PR_REVIEW: &[Flag] = &[
    ("approve", Some('a'), false),
    ("body", Some('b'), true),
    ("body-file", Some('F'), true),
    ("comment", Some('c'), false),
    ("request-changes", Some('r'), false),
];

/// `pr comment` and `issue comment` share gh's flag set.
const COMMENT: &[Flag] = &[
    ("body", Some('b'), true),
    ("body-file", Some('F'), true),
    ("create-if-none", None, false),
    ("delete-last", None, false),
    ("edit-last", None, false),
    ("editor", Some('e'), false),
    ("web", Some('w'), false),
    ("yes", None, false),
];

const ISSUE_CREATE: &[Flag] = &[
    ("assignee", Some('a'), true),
    ("body", Some('b'), true),
    ("body-file", Some('F'), true),
    ("editor", Some('e'), false),
    ("label", Some('l'), true),
    ("milestone", Some('m'), true),
    ("project", Some('p'), true),
    ("recover", None, true),
    ("template", Some('T'), true),
    ("title", Some('t'), true),
    ("web", Some('w'), false),
];

const ISSUE_EDIT: &[Flag] = &[
    ("add-assignee", None, true),
    ("add-label", None, true),
    ("add-project", None, true),
    ("body", Some('b'), true),
    ("body-file", Some('F'), true),
    ("milestone", Some('m'), true),
    ("remove-assignee", None, true),
    ("remove-label", None, true),
    ("remove-milestone", None, false),
    ("remove-project", None, true),
    ("title", Some('t'), true),
];

const ISSUE_CLOSE: &[Flag] = &[
    ("comment", Some('c'), true),
    ("duplicate-of", None, true),
    ("reason", Some('r'), true),
];

const ISSUE_REOPEN: &[Flag] = &[("comment", Some('c'), true)];

const API: &[Flag] = &[
    ("cache", None, true),
    ("field", Some('F'), true),
    ("header", Some('H'), true),
    ("hostname", None, true),
    ("include", Some('i'), false),
    ("input", None, true),
    ("jq", Some('q'), true),
    ("method", Some('X'), true),
    ("paginate", None, false),
    ("preview", Some('p'), true),
    ("raw-field", Some('f'), true),
    ("silent", None, false),
    ("slurp", None, false),
    ("template", Some('t'), true),
    ("verbose", None, false),
];

/// The flag table for a gh write subcommand this proxy knows, or None.
fn table_for(group: &str, sub: &str) -> Option<&'static [Flag]> {
    Some(match (group, sub) {
        ("pr", "create") => PR_CREATE,
        ("pr", "edit") => PR_EDIT,
        ("pr", "merge") => PR_MERGE,
        ("pr", "close") => PR_CLOSE,
        ("pr", "review") => PR_REVIEW,
        ("pr", "comment") | ("issue", "comment") => COMMENT,
        ("issue", "create") => ISSUE_CREATE,
        ("issue", "edit") => ISSUE_EDIT,
        ("issue", "close") => ISSUE_CLOSE,
        ("issue", "reopen") => ISSUE_REOPEN,
        _ => return None,
    })
}

/// An argv parsed against one subcommand's flag table, pflag-style.
#[derive(Debug, Default)]
struct Parsed {
    /// `(long name, value)` in argv order; bool flags carry `None`.
    flags: Vec<(&'static str, Option<String>)>,
    positionals: Vec<String>,
    /// An argument the table does not recognize, a bool flag given a value,
    /// or a value flag with no value. Any of these makes a write Passthrough.
    unknown: bool,
}

impl Parsed {
    fn count(&self, long: &str) -> usize {
        self.flags.iter().filter(|(l, _)| *l == long).count()
    }

    fn has(&self, long: &str) -> bool {
        self.count(long) > 0
    }

    fn values(&self, long: &str) -> Vec<&str> {
        self.flags
            .iter()
            .filter(|(l, _)| *l == long)
            .filter_map(|(_, v)| v.as_deref())
            .collect()
    }

    /// The last value of `long`: pflag's last-wins for a repeated flag.
    fn last(&self, long: &str) -> Option<String> {
        self.values(long).last().map(|v| (*v).to_owned())
    }

    /// True when every flag present is in `allowed`.
    fn only(&self, allowed: &[&str]) -> bool {
        self.flags.iter().all(|(l, _)| allowed.contains(l))
    }
}

/// Look up a flag by long name or short char in `table`, plus -R/--repo.
fn find_flag(table: &[Flag], long: Option<&str>, short: Option<char>) -> Option<Flag> {
    table
        .iter()
        .chain(std::iter::once(&REPO))
        .find(|(l, s, _)| match (long, short) {
            (Some(name), _) => *l == name,
            (None, Some(c)) => *s == Some(c),
            (None, None) => false,
        })
        .copied()
}

/// Parse `args` against `table` with pflag's spellings: `--flag value`,
/// `--flag=value`, `-f value`, `-fvalue`, `-f=value`, clustered bool shorts
/// (`-sd`), and a `--` terminator after which everything is positional.
fn parse(args: &[String], table: &[Flag]) -> Parsed {
    let mut parsed = Parsed::default();
    let mut i: usize = 0;
    let mut terminated = false;
    while i < args.len() {
        let arg: &str = &args[i];
        i += 1;
        if terminated || arg == "-" || !arg.starts_with('-') {
            parsed.positionals.push(arg.to_owned());
            continue;
        }
        if arg == "--" {
            terminated = true;
            continue;
        }
        if let Some(body) = arg.strip_prefix("--") {
            let (name, inline): (&str, Option<&str>) = match body.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (body, None),
            };
            match find_flag(table, Some(name), None) {
                None => parsed.unknown = true,
                Some((long, _, true)) => match inline {
                    Some(v) => parsed.flags.push((long, Some(v.to_owned()))),
                    None => match args.get(i) {
                        Some(v) => {
                            parsed.flags.push((long, Some(v.clone())));
                            i += 1;
                        }
                        None => parsed.unknown = true,
                    },
                },
                Some((long, _, false)) => {
                    if inline.is_some() {
                        parsed.unknown = true;
                    }
                    parsed.flags.push((long, None));
                }
            }
            continue;
        }
        // A short cluster: bool shorts may combine; a value short takes the
        // rest of the cluster (minus a leading '=') or the next argument.
        let chars: Vec<char> = arg[1..].chars().collect();
        let mut j: usize = 0;
        while j < chars.len() {
            match find_flag(table, None, Some(chars[j])) {
                None => {
                    parsed.unknown = true;
                    break;
                }
                Some((long, _, true)) => {
                    let rest: String = chars[j + 1..].iter().collect();
                    if !rest.is_empty() {
                        let value: &str = rest.strip_prefix('=').unwrap_or(&rest);
                        parsed.flags.push((long, Some(value.to_owned())));
                    } else if let Some(v) = args.get(i) {
                        parsed.flags.push((long, Some(v.clone())));
                        i += 1;
                    } else {
                        parsed.unknown = true;
                    }
                    break;
                }
                Some((long, _, false)) => {
                    parsed.flags.push((long, None));
                    if chars.get(j + 1) == Some(&'=') {
                        parsed.unknown = true;
                        break;
                    }
                }
            }
            j += 1;
        }
    }
    parsed
}

/// A positional that is all digits, as a number. gh also takes URLs and
/// branch names; those are not numbers and translate to nothing.
fn digits(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// The PR a `pr` subcommand names: none means the current branch.
fn pr_ref(p: &Parsed) -> Option<PrRef> {
    match p.positionals.as_slice() {
        [] => Some(PrRef::CurrentBranch),
        [n] => digits(n).map(PrRef::Number),
        _ => None,
    }
}

/// The single issue number an `issue` subcommand requires.
fn issue_number(p: &Parsed) -> Option<u64> {
    match p.positionals.as_slice() {
        [n] => digits(n),
        _ => None,
    }
}

/// A body from `-b` or `-F`, when at most one was given. `Ok(None)` is no
/// body; `Err(())` is more than one.
fn body_source(p: &Parsed) -> Result<Option<BodySource>, ()> {
    match (p.count("body"), p.count("body-file")) {
        (0, 0) => Ok(None),
        (1, 0) => Ok(p.last("body").map(BodySource::Inline)),
        (0, 1) => Ok(p.last("body-file").map(|f| {
            if f == "-" {
                BodySource::Stdin
            } else {
                BodySource::File(PathBuf::from(f))
            }
        })),
        _ => Err(()),
    }
}

/// Exactly one body, as the rows that require one demand.
fn required_body(p: &Parsed) -> Option<BodySource> {
    body_source(p).ok().flatten()
}

/// `-l` values, repeatable or comma-separated, flattened.
fn labels(p: &Parsed) -> Vec<String> {
    p.values("label")
        .iter()
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// A flag naming exactly one login: at most one occurrence, no comma.
/// `Err(())` is anything else.
fn single_login(p: &Parsed, long: &str) -> Result<Option<String>, ()> {
    match p.values(long).as_slice() {
        [] => Ok(None),
        [v] if !v.contains(',') && !v.is_empty() => Ok(Some((*v).to_owned())),
        _ => Err(()),
    }
}

/// Translate one write subcommand's parsed argv, or None when any argument
/// falls outside its row in the issue's table.
fn translate(group: &str, sub: &str, p: &Parsed) -> Option<GhWrite> {
    if p.unknown {
        return None;
    }
    let repo: Option<String> = p.last("repo");
    match (group, sub) {
        ("pr", "create") => {
            let allowed = [
                "repo",
                "title",
                "body",
                "body-file",
                "base",
                "head",
                "draft",
                "label",
                "reviewer",
            ];
            if !p.only(&allowed) || !p.positionals.is_empty() {
                return None;
            }
            Some(GhWrite::PrCreate {
                repo,
                title: p.last("title")?,
                body: required_body(p)?,
                base: p.last("base"),
                head: p.last("head"),
                draft: p.has("draft"),
                labels: labels(p),
                reviewer: single_login(p, "reviewer").ok()?,
            })
        }
        ("pr", "edit") => {
            if !p.only(&["repo", "title"]) {
                return None;
            }
            Some(GhWrite::PrEdit {
                repo,
                pr: pr_ref(p)?,
                title: p.last("title")?,
            })
        }
        ("pr", "merge") => {
            if !p.only(&["repo", "squash", "merge", "rebase", "delete-branch"]) {
                return None;
            }
            let chosen: Vec<&str> = ["squash", "merge", "rebase"]
                .into_iter()
                .filter(|s| p.has(s))
                .collect();
            let [strategy] = chosen.as_slice() else {
                return None;
            };
            Some(GhWrite::PrMerge {
                repo,
                pr: pr_ref(p)?,
                strategy: (*strategy).to_owned(),
                delete_branch: p.has("delete-branch"),
            })
        }
        ("pr", "close") => {
            if !p.only(&["repo", "comment", "delete-branch"]) {
                return None;
            }
            Some(GhWrite::PrClose {
                repo,
                pr: pr_ref(p)?,
                comment: p.last("comment"),
                delete_branch: p.has("delete-branch"),
            })
        }
        ("pr", "review") => {
            let allowed = [
                "repo",
                "approve",
                "request-changes",
                "comment",
                "body",
                "body-file",
            ];
            if !p.only(&allowed) {
                return None;
            }
            let events: Vec<ReviewEvent> = [
                ("approve", ReviewEvent::Approve),
                ("request-changes", ReviewEvent::RequestChanges),
                ("comment", ReviewEvent::Comment),
            ]
            .into_iter()
            .filter(|(flag, _)| p.has(flag))
            .map(|(_, e)| e)
            .collect();
            let [event] = events.as_slice() else {
                return None;
            };
            let body: Option<BodySource> = body_source(p).ok()?;
            if body.is_none() && *event != ReviewEvent::Approve {
                return None;
            }
            Some(GhWrite::PrReview {
                repo,
                pr: pr_ref(p)?,
                event: *event,
                body,
            })
        }
        ("pr", "comment") => {
            if !p.only(&["repo", "body", "body-file"]) {
                return None;
            }
            Some(GhWrite::PrComment {
                repo,
                pr: pr_ref(p)?,
                body: required_body(p)?,
            })
        }
        ("issue", "create") => {
            let allowed = ["repo", "title", "body", "body-file", "label", "assignee"];
            if !p.only(&allowed) || !p.positionals.is_empty() {
                return None;
            }
            Some(GhWrite::IssueCreate {
                repo,
                title: p.last("title")?,
                body: required_body(p)?,
                labels: labels(p),
                assignee: single_login(p, "assignee").ok()?,
            })
        }
        ("issue", "edit") => {
            if !p.only(&["repo", "title", "body", "body-file"]) {
                return None;
            }
            let title: Option<String> = p.last("title");
            let body: Option<BodySource> = body_source(p).ok()?;
            if title.is_none() && body.is_none() {
                return None;
            }
            Some(GhWrite::IssueEdit {
                repo,
                number: issue_number(p)?,
                title,
                body,
            })
        }
        ("issue", "close") | ("issue", "reopen") => {
            if !p.only(&["repo", "comment"]) {
                return None;
            }
            let number: u64 = issue_number(p)?;
            let comment: Option<String> = p.last("comment");
            Some(if sub == "close" {
                GhWrite::IssueClose {
                    repo,
                    number,
                    comment,
                }
            } else {
                GhWrite::IssueReopen {
                    repo,
                    number,
                    comment,
                }
            })
        }
        ("issue", "comment") => {
            if !p.only(&["repo", "body", "body-file"]) {
                return None;
            }
            Some(GhWrite::IssueComment {
                repo,
                number: issue_number(p)?,
                body: required_body(p)?,
            })
        }
        _ => None,
    }
}

/// `gh api`: a read only when the effective method is GET and the endpoint
/// is not `graphql`. Method: `-X`/`--method` (case-insensitive), else POST
/// when any field or input flag is present, else GET.
fn plan_api(rest: &[String]) -> GhPlan {
    let p: Parsed = parse(rest, API);
    if p.unknown || p.positionals.first().map(String::as_str) == Some("graphql") {
        return GhPlan::Passthrough;
    }
    let method: String = match p.last("method") {
        Some(m) => m.to_ascii_uppercase(),
        None if ["raw-field", "field", "input"].iter().any(|f| p.has(f)) => "POST".to_owned(),
        None => "GET".to_owned(),
    };
    if method == "GET" {
        GhPlan::Read
    } else {
        GhPlan::Passthrough
    }
}

/// Classify a gh argv. Pure: no disk, no network, no environment.
pub(crate) fn plan(args: &[String]) -> GhPlan {
    let before_terminator = args.iter().take_while(|a| a.as_str() != "--");
    if args.is_empty()
        || before_terminator
            .clone()
            .any(|a| a == "-h" || a == "--help")
    {
        return GhPlan::Read;
    }
    let group: &str = &args[0];
    match group {
        "--version" | "version" | "status" | "search" => return GhPlan::Read,
        "api" => return plan_api(&args[1..]),
        _ => {}
    }
    let Some(sub) = args.get(1).map(String::as_str) else {
        return GhPlan::Passthrough;
    };
    match (group, sub) {
        (_, "view" | "list" | "status")
        | ("pr", "checks" | "diff")
        | ("run", "watch" | "download")
        | ("release", "download") => GhPlan::Read,
        _ => match table_for(group, sub) {
            Some(table) => translate(group, sub, &parse(&args[2..], table))
                .map_or(GhPlan::Passthrough, GhPlan::Write),
            None => GhPlan::Passthrough,
        },
    }
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

/// Run a gh command: translated writes through legion's verbs, everything
/// else through the real gh with its output and exit code kept.
pub(crate) fn handle(args: Vec<String>) -> error::Result<()> {
    match plan(&args) {
        GhPlan::Read => exit_with(run_gh(&args)?),
        GhPlan::Write(write) => {
            let target: Option<String> = gh_target(write_repo(&write));
            let legion_repo: Option<String> =
                target.as_deref().and_then(worksource::repo_for_github);
            if let Some(r) = legion_repo.as_deref()
                && run_write(write, r)?
            {
                return Ok(());
            }
            passthrough(&args, target, legion_repo)
        }
        GhPlan::Passthrough => {
            let target: Option<String> = gh_target(repo_flag(&args).as_deref());
            let legion_repo: Option<String> =
                target.as_deref().and_then(worksource::repo_for_github);
            passthrough(&args, target, legion_repo)
        }
    }
}

/// Map gh's exit code onto legion's: zero is success, anything else exits
/// with the same code and prints nothing more.
fn exit_with(code: i32) -> error::Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(error::LegionError::ExitWith(code))
    }
}

/// Run the `gh` on PATH with the argv byte for byte, inheriting stdio.
fn run_gh(args: &[String]) -> error::Result<i32> {
    let status: ExitStatus = Command::new("gh").args(args).status().map_err(|e| {
        error::LegionError::WorkSource(format!("failed to run gh (is gh on PATH?): {e}"))
    })?;
    Ok(exit_code(status))
}

/// gh's exit code; a gh killed by a signal is 128 plus the signal number,
/// as a shell reports it.
fn exit_code(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        if let Some(sig) = status.signal() {
            return 128 + sig;
        }
    }
    1
}

/// The -R/--repo a write carries.
fn write_repo(write: &GhWrite) -> Option<&str> {
    match write {
        GhWrite::PrCreate { repo, .. }
        | GhWrite::PrEdit { repo, .. }
        | GhWrite::PrMerge { repo, .. }
        | GhWrite::PrClose { repo, .. }
        | GhWrite::PrReview { repo, .. }
        | GhWrite::PrComment { repo, .. }
        | GhWrite::IssueCreate { repo, .. }
        | GhWrite::IssueEdit { repo, .. }
        | GhWrite::IssueClose { repo, .. }
        | GhWrite::IssueReopen { repo, .. }
        | GhWrite::IssueComment { repo, .. } => repo.as_deref(),
    }
}

/// The -R/--repo value in any pflag spelling, before a `--`, for an argv
/// `plan` did not translate. Last one wins, as in pflag.
fn repo_flag(args: &[String]) -> Option<String> {
    let mut found: Option<String> = None;
    let mut iter = args.iter().take_while(|a| a.as_str() != "--");
    while let Some(arg) = iter.next() {
        let inline: Option<&str> = arg
            .strip_prefix("--repo=")
            .or_else(|| arg.strip_prefix("-R="))
            .or_else(|| arg.strip_prefix("-R").filter(|v| !v.is_empty()));
        if let Some(v) = inline {
            found = Some(v.to_owned());
        } else if arg == "-R" || arg == "--repo" {
            found = iter.next().cloned();
        }
    }
    found
}

/// The gh target: the -R value, else `GH_REPO`, else what
/// `gh repo view --json nameWithOwner` reports from the current directory.
fn gh_target(explicit: Option<&str>) -> Option<String> {
    if let Some(r) = explicit {
        return Some(r.to_owned());
    }
    if let Ok(r) = std::env::var("GH_REPO")
        && !r.is_empty()
    {
        return Some(r);
    }
    let out = Command::new("gh")
        .args(["repo", "view", "--json", "nameWithOwner"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    value
        .get("nameWithOwner")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// The number of the one open PR whose head is the current branch.
fn current_branch_pr(plugin: &str, source_repo: &str) -> Option<u64> {
    let out = Command::new("git")
        .args(["branch", "--show-current"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let branch: String = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if branch.is_empty() {
        return None;
    }
    let prs: Vec<worksource::ExternalPR> = worksource::list_prs(plugin, source_repo).ok()?;
    let matching: Vec<u64> = prs
        .iter()
        .filter(|p| p.head_ref_name == branch)
        .map(|p| p.number)
        .collect();
    match matching.as_slice() {
        [n] => Some(*n),
        _ => None,
    }
}

/// Resolve a `PrRef` to a number, or None when it cannot be.
fn resolve_pr(pr: &PrRef, plugin: &str, source_repo: &str) -> Option<u64> {
    match pr {
        PrRef::Number(n) => Some(*n),
        PrRef::CurrentBranch => current_branch_pr(plugin, source_repo),
    }
}

/// Read a body. `Ok(None)` is an unreadable file, which sends the command
/// to passthrough so gh reports its own error; an unreadable stdin is an
/// error, since gh could not read it either once legion has tried.
fn read_body(body: &BodySource) -> error::Result<Option<String>> {
    match body {
        BodySource::Inline(text) => Ok(Some(text.clone())),
        BodySource::File(path) => Ok(std::fs::read_to_string(path).ok()),
        BodySource::Stdin => {
            use std::io::Read as _;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf).map_err(|e| {
                error::LegionError::WorkSource(format!("failed to read -F - from stdin: {e}"))
            })?;
            Ok(Some(buf))
        }
    }
}

/// Read an optional body; `Ok(None)` means passthrough, as in `read_body`.
fn read_optional_body(body: &Option<BodySource>) -> error::Result<Option<Option<String>>> {
    match body {
        None => Ok(Some(None)),
        Some(b) => Ok(read_body(b)?.map(Some)),
    }
}

/// Run a translated write through its legion verb. `Ok(false)` means a
/// resolution step failed and the caller runs gh instead; once a verb runs,
/// its result is final.
fn run_write(write: GhWrite, legion_repo: &str) -> error::Result<bool> {
    let Some((plugin, source_repo, _workdir)) = worksource::resolve_config(legion_repo) else {
        return Ok(false);
    };
    let repo: String = legion_repo.to_owned();
    match write {
        GhWrite::PrCreate {
            title,
            body,
            base,
            head,
            draft,
            labels,
            reviewer,
            ..
        } => {
            let Some(body) = read_body(&body)? else {
                return Ok(false);
            };
            pr::handle(PrAction::Create {
                repo,
                title,
                body: Some(body),
                base,
                head,
                draft,
                labels: (!labels.is_empty()).then(|| labels.join(",")),
                reviewer,
                task: None,
                skip_gates: false,
                closes: Vec::new(),
            })?;
        }
        GhWrite::PrEdit { pr, title, .. } => {
            let Some(number) = resolve_pr(&pr, &plugin, &source_repo) else {
                return Ok(false);
            };
            pr::handle(PrAction::Edit {
                repo,
                number,
                title: Some(title),
                body_file: None,
                issue: None,
            })?;
        }
        GhWrite::PrMerge {
            pr,
            strategy,
            delete_branch,
            ..
        } => {
            let Some(number) = resolve_pr(&pr, &plugin, &source_repo) else {
                return Ok(false);
            };
            pr::handle(PrAction::Merge {
                repo,
                number,
                strategy,
                keep_branch: !delete_branch,
                task: None,
                merge_despite_failures: false,
            })?;
        }
        GhWrite::PrClose {
            pr,
            comment,
            delete_branch,
            ..
        } => {
            let Some(number) = resolve_pr(&pr, &plugin, &source_repo) else {
                return Ok(false);
            };
            pr::handle(PrAction::Close {
                repo,
                number,
                reason: comment,
                delete_branch,
            })?;
        }
        GhWrite::PrReview {
            pr, event, body, ..
        } => {
            let Some(number) = resolve_pr(&pr, &plugin, &source_repo) else {
                return Ok(false);
            };
            let Some(body) = read_optional_body(&body)? else {
                return Ok(false);
            };
            pr::handle(PrAction::Review {
                repo,
                number,
                approve: event == ReviewEvent::Approve,
                request_changes: event == ReviewEvent::RequestChanges,
                body,
                comments: None,
            })?;
        }
        GhWrite::PrComment { pr, body, .. } => {
            let Some(number) = resolve_pr(&pr, &plugin, &source_repo) else {
                return Ok(false);
            };
            let Some(body) = read_body(&body)? else {
                return Ok(false);
            };
            issue::handle_comment(repo, number, Some(body))?;
        }
        GhWrite::IssueCreate {
            title,
            body,
            labels,
            assignee,
            ..
        } => {
            let Some(body) = read_body(&body)? else {
                return Ok(false);
            };
            issue::handle(IssueAction::Create {
                repo,
                title,
                body: Some(body),
                labels: (!labels.is_empty()).then(|| labels.join(",")),
                assignee,
            })?;
        }
        GhWrite::IssueEdit {
            number,
            title,
            body,
            ..
        } => {
            let Some(body) = read_optional_body(&body)? else {
                return Ok(false);
            };
            issue::handle(IssueAction::Edit {
                repo,
                number,
                title,
                body,
            })?;
        }
        GhWrite::IssueClose {
            number, comment, ..
        } => {
            issue::handle(IssueAction::Close {
                repo,
                number,
                comment,
                force: false,
                force_reason: None,
            })?;
        }
        GhWrite::IssueReopen {
            number, comment, ..
        } => {
            issue::handle(IssueAction::Reopen {
                repo,
                number,
                comment,
            })?;
        }
        GhWrite::IssueComment { number, body, .. } => {
            let Some(body) = read_body(&body)? else {
                return Ok(false);
            };
            issue::handle_comment(repo, number, Some(body))?;
        }
    }
    Ok(true)
}

/// The positionals of an untranslated argv, parsed against its subcommand's
/// table when known (so a flag's value is not mistaken for a positional).
/// `api` positionals start after `api`; others after `<group> <sub>`.
fn passthrough_positionals(args: &[String]) -> Vec<String> {
    let group: &str = args.first().map(String::as_str).unwrap_or("");
    if group == "api" {
        return parse(&args[1..], API).positionals;
    }
    let sub: &str = args.get(1).map(String::as_str).unwrap_or("");
    let table: &[Flag] = table_for(group, sub).unwrap_or(&[]);
    parse(args.get(2..).unwrap_or(&[]), table).positionals
}

/// Run real gh for an untranslated write: the legion gate first, then gh,
/// then one `gh-passthrough` audit row.
fn passthrough(
    args: &[String],
    target: Option<String>,
    legion_repo: Option<String>,
) -> error::Result<()> {
    let positionals: Vec<String> = passthrough_positionals(args);
    gate_passthrough(args, &positionals, legion_repo.as_deref())?;

    let code: i32 = run_gh(args)?;

    let group: &str = args.first().map(String::as_str).unwrap_or("");
    let first: Option<&str> = positionals.first().map(String::as_str);
    let target_ref: String = match first {
        Some(n) if digits(n).is_some() => n.to_owned(),
        Some(endpoint) if group == "api" => endpoint.to_owned(),
        _ => target.clone().unwrap_or_default(),
    };
    let agent: String = legion_repo.or(target).unwrap_or_else(|| "gh".to_owned());
    let details: String = serde_json::to_string(args)?;
    let outcome: String = if code == 0 {
        "success".to_owned()
    } else {
        format!("exit {code}")
    };
    match open_db() {
        Ok(database) => audit(
            &database,
            &db::AuditInput {
                agent: &agent,
                action: "gh-passthrough",
                target_type: group,
                target_ref: &target_ref,
                task_id: None,
                source_type: "github",
                details: Some(&details),
                outcome: &outcome,
            },
        ),
        Err(e) => eprintln!("[legion] warning: audit log failed: {e}"),
    }
    exit_with(code)
}

/// The gate a gated write's legion verb enforces, applied before real gh
/// runs: `pr create` needs clean simplify and pr-write gates on HEAD; `pr
/// merge` needs check runs, none failing, and no changes-requested review;
/// `issue close` needs the verify verdict. The merge and close gates read
/// the work source, so they apply only when a legion repo resolved; with
/// one resolved but no number to check, the gate refuses rather than guess.
fn gate_passthrough(
    args: &[String],
    positionals: &[String],
    legion_repo: Option<&str>,
) -> error::Result<()> {
    let group: &str = args.first().map(String::as_str).unwrap_or("");
    let sub: &str = args.get(1).map(String::as_str).unwrap_or("");
    match (group, sub) {
        ("pr", "create") => gate_pr_create(),
        ("pr", "merge") | ("issue", "close") => {
            let Some(repo) = legion_repo else {
                return Ok(());
            };
            let (plugin, source_repo, _workdir) = worksource::require_worksource(repo)?;
            let number: Option<u64> = match positionals {
                [] if group == "pr" => current_branch_pr(&plugin, &source_repo),
                [n] => digits(n),
                _ => None,
            };
            let Some(number) = number else {
                return Err(error::LegionError::WorkSource(format!(
                    "cannot evaluate the legion gate for gh {group} {sub}: no {group} number \
                     could be determined from the arguments"
                )));
            };
            if group == "pr" {
                gate_pr_merge(&plugin, &source_repo, number)
            } else {
                let database = open_db()?;
                issue::check_verify_before_close(
                    &database,
                    &plugin,
                    &source_repo,
                    number,
                    false,
                    None,
                )
                .map(|_| ())
            }
        }
        _ => Ok(()),
    }
}

/// `legion pr create`'s gate: clean legion-simplify and legion-pr-write on
/// HEAD, with the same refusals.
fn gate_pr_create() -> error::Result<()> {
    let database = open_db()?;
    let (commit_hash, _branch) = git_head_commit_and_branch()?;
    let short_hash: &str = &commit_hash[..commit_hash.len().min(8)];
    for (skill, run_hint) in [
        ("legion-simplify", "/legion:legion-simplify"),
        ("legion-pr-write", "/legion:legion-pr-write"),
    ] {
        match database.get_quality_gate(&commit_hash, skill)? {
            None => {
                eprintln!(
                    "[legion] error: no clean {skill} gate on HEAD ({short_hash}). \
                     Run {run_hint} before creating the PR."
                );
                return Err(error::LegionError::ExitWith(1));
            }
            Some(gate) if gate.result != GateResult::Clean => {
                eprintln!(
                    "[legion] error: {skill} recorded issues on HEAD ({short_hash}), \
                     {} findings. Fix them and re-run the skill before creating the PR.",
                    gate.findings_count
                );
                return Err(error::LegionError::ExitWith(1));
            }
            Some(_) => {}
        }
    }
    Ok(())
}

/// `legion pr merge`'s gate: the zero-runs and failing-checks refusals with
/// the verb's wording, then the plugin's changes-requested review refusal.
fn gate_pr_merge(plugin: &str, source_repo: &str, number: u64) -> error::Result<()> {
    let checks = worksource::pr_checks(plugin, source_repo, number)?;
    if checks.checks.is_empty() {
        return Err(error::LegionError::WorkSource(format!(
            "no runs for head {}: PR #{number} on {source_repo} has zero check runs \
             for its head commit (not the branch's last suite)",
            checks.head_sha
        )));
    }
    let failed: Vec<&str> = checks
        .checks
        .iter()
        .filter(|c| c.is_failing())
        .map(|c| c.name.as_str())
        .collect();
    if !failed.is_empty() {
        return Err(error::LegionError::WorkSource(format!(
            "{} check(s) failed on PR #{}: {}",
            failed.len(),
            number,
            failed.join(", ")
        )));
    }
    let details = worksource::view_pr(plugin, source_repo, number)?;
    if details.review_decision.as_deref() == Some("CHANGES_REQUESTED") {
        return Err(error::LegionError::WorkSource(format!(
            "PR #{number} has changes requested -- address the review before merging"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(s: &[&str]) -> Vec<String> {
        s.iter().map(|a| (*a).to_owned()).collect()
    }

    fn plan_of(s: &[&str]) -> GhPlan {
        plan(&argv(s))
    }

    #[test]
    fn plan_classifies_the_measured_commands() {
        let reads: &[&[&str]] = &[
            &["pr", "view", "12", "--json", "title,body,headRefName"],
            &["issue", "view", "150", "-R", "runlegion/legion"],
            &["issue", "list", "--state", "open"],
            &["run", "view", "99", "--log-failed"],
            &["pr", "checks", "5"],
            &["pr", "diff", "5"],
            &["release", "view", "v0.42.1"],
            &["repo", "view"],
            &["api", "repos/o/n/pulls/5"],
            &[],
            &["--version"],
            &["version"],
            &["status"],
            &["pr", "status"],
            &["search", "issues", "foo"],
            &["run", "watch", "1"],
            &["run", "download", "1"],
            &["release", "download", "v1"],
            &["pr", "merge", "8", "-s", "--help"],
            &["issue", "close", "-h"],
        ];
        for r in reads {
            assert_eq!(plan_of(r), GhPlan::Read, "expected Read for {r:?}");
        }

        match plan_of(&["pr", "merge", "8", "-s"]) {
            GhPlan::Write(GhWrite::PrMerge {
                repo: None,
                pr: PrRef::Number(8),
                strategy,
                delete_branch: false,
            }) => assert_eq!(strategy, "squash"),
            other => panic!("pr merge 8 -s: {other:?}"),
        }
        match plan_of(&["pr", "merge", "8", "--squash", "-d"]) {
            GhPlan::Write(GhWrite::PrMerge {
                delete_branch: true,
                strategy,
                ..
            }) => assert_eq!(strategy, "squash"),
            other => panic!("pr merge 8 --squash -d: {other:?}"),
        }
        assert_eq!(
            plan_of(&["pr", "edit", "4", "--title", "x"]),
            GhPlan::Write(GhWrite::PrEdit {
                repo: None,
                pr: PrRef::Number(4),
                title: "x".to_owned(),
            })
        );
        assert_eq!(
            plan_of(&[
                "issue", "create", "-t", "x", "-F", "body.md", "-l", "a", "-l", "b,c"
            ]),
            GhPlan::Write(GhWrite::IssueCreate {
                repo: None,
                title: "x".to_owned(),
                body: BodySource::File(PathBuf::from("body.md")),
                labels: argv(&["a", "b", "c"]),
                assignee: None,
            })
        );
        assert_eq!(
            plan_of(&["pr", "comment", "-b", "hi"]),
            GhPlan::Write(GhWrite::PrComment {
                repo: None,
                pr: PrRef::CurrentBranch,
                body: BodySource::Inline("hi".to_owned()),
            })
        );
        assert_eq!(
            plan_of(&["pr", "review", "3", "-a"]),
            GhPlan::Write(GhWrite::PrReview {
                repo: None,
                pr: PrRef::Number(3),
                event: ReviewEvent::Approve,
                body: None,
            })
        );
        assert_eq!(
            plan_of(&["issue", "comment", "9", "-F", "-"]),
            GhPlan::Write(GhWrite::IssueComment {
                repo: None,
                number: 9,
                body: BodySource::Stdin,
            })
        );

        let passthroughs: &[&[&str]] = &[
            &["pr", "merge", "8"],
            &["pr", "merge", "8", "-s", "--auto"],
            &["pr", "merge", "8", "-s", "-m"],
            &["pr", "edit", "4", "--body", "x"],
            &["issue", "create", "-t", "x"],
            &[
                "pr", "create", "-t", "x", "-b", "y", "-r", "alice", "-r", "bob",
            ],
            &["pr", "create", "-t", "x", "-b", "y", "-r", "alice,bob"],
            &["pr", "review", "3", "-c"],
            &["issue", "close", "7", "-r", "not planned"],
            &["pr", "comment", "2", "--web"],
            &["release", "create", "v1"],
            &["pr", "merge", "https://github.com/o/n/pull/8", "-s"],
            &["issue", "close"],
            &["pr", "merge", "8", "-s", "--bogus"],
            &["pr", "create", "-t"],
            &["pr"],
        ];
        for p in passthroughs {
            assert_eq!(
                plan_of(p),
                GhPlan::Passthrough,
                "expected Passthrough for {p:?}"
            );
        }
    }

    #[test]
    fn plan_reads_gh_api_method() {
        assert_eq!(plan_of(&["api", "x"]), GhPlan::Read);
        assert_eq!(
            plan_of(&["api", "-X", "GET", "x", "-f", "q=1"]),
            GhPlan::Read
        );
        assert_eq!(plan_of(&["api", "--method=get", "x"]), GhPlan::Read);
        assert_eq!(plan_of(&["api", "x", "-f", "a=1"]), GhPlan::Passthrough);
        assert_eq!(
            plan_of(&["api", "x", "--input", "f.json"]),
            GhPlan::Passthrough
        );
        assert_eq!(plan_of(&["api", "-XPATCH", "x"]), GhPlan::Passthrough);
        assert_eq!(
            plan_of(&["api", "graphql", "-f", "query={viewer{login}}"]),
            GhPlan::Passthrough
        );
        assert_eq!(plan_of(&["api", "graphql"]), GhPlan::Passthrough);
        assert_eq!(
            plan_of(&["api", "--method", "POST", "x"]),
            GhPlan::Passthrough
        );
        assert_eq!(plan_of(&["api", "x", "-F", "n=1"]), GhPlan::Passthrough);
    }

    #[test]
    fn plan_accepts_pflag_spellings() {
        let pairs: &[(&[&str], &[&str])] = &[
            (
                &["pr", "edit", "4", "--title=x"],
                &["pr", "edit", "4", "--title", "x"],
            ),
            (&["pr", "edit", "4", "-tx"], &["pr", "edit", "4", "-t", "x"]),
            (
                &["pr", "merge", "8", "-s", "-R=o/n"],
                &["pr", "merge", "8", "-s", "-R", "o/n"],
            ),
            (
                &["pr", "merge", "-s", "--", "8"],
                &["pr", "merge", "8", "-s"],
            ),
            (
                &["issue", "close", "-R", "o/n", "--", "7"],
                &["issue", "close", "7", "-R", "o/n"],
            ),
            (
                &["pr", "merge", "8", "-sd"],
                &["pr", "merge", "8", "-s", "-d"],
            ),
        ];
        for (spelled, spaced) in pairs {
            let a: GhPlan = plan_of(spelled);
            assert!(
                matches!(a, GhPlan::Write(_)),
                "{spelled:?} should translate, got {a:?}"
            );
            assert_eq!(a, plan_of(spaced), "{spelled:?} vs {spaced:?}");
        }
        match plan_of(&["pr", "merge", "8", "-s", "-R=o/n"]) {
            GhPlan::Write(GhWrite::PrMerge { repo, .. }) => {
                assert_eq!(repo.as_deref(), Some("o/n"))
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn repo_flag_reads_every_spelling() {
        for spelled in [
            &["issue", "close", "7", "-R", "o/n"][..],
            &["issue", "close", "7", "-Ro/n"],
            &["issue", "close", "7", "-R=o/n"],
            &["issue", "close", "7", "--repo", "o/n"],
            &["issue", "close", "7", "--repo=o/n"],
        ] {
            assert_eq!(repo_flag(&argv(spelled)).as_deref(), Some("o/n"));
        }
        assert_eq!(repo_flag(&argv(&["api", "x", "--", "-R", "o/n"])), None);
    }

    #[test]
    fn passthrough_positionals_skip_flag_values() {
        assert_eq!(
            passthrough_positionals(&argv(&["pr", "merge", "--subject", "12", "5", "-s"])),
            argv(&["5"])
        );
        assert_eq!(
            passthrough_positionals(&argv(&["api", "-H", "a: b", "repos/o/n", "-f", "x=1"])),
            argv(&["repos/o/n"])
        );
    }
}
