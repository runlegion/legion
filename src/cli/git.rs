//! `legion git` (#1335): a proxy that takes git's own arguments.
//!
//! Agents type git by habit. Translating git's arguments into `legion push` /
//! `legion commit` flags one flag at a time (the router's old job) cost an
//! issue per flag -- `-C` alone was #1301. This proxy takes the argument list
//! exactly as git would receive it and ends in one of four routes:
//!
//! 1. a `push` or `commit` the audited path can express runs through that
//!    path -- the same [`handle_push`] / [`handle_commit`] `legion push` and
//!    `legion commit` run;
//! 2. a push the rules forbid (any force variant, or one that would update
//!    `main`/`master` on the remote) is refused before anything runs;
//! 3. any other `push` or `commit` runs as real git with its arguments
//!    untouched, and writes an audit row;
//! 4. every other git subcommand runs as real git, unaudited.
//!
//! The route is chosen from git's arguments alone. Parsing is pure (the
//! `parse_*` functions); the few facts the route needs about the repository
//! git would act on -- the checked-out branch, whether a local branch or tag
//! exists, the upstream config, the worktree list -- come through
//! [`RepoFacts`], so the classifier is unit-testable without a repository.
//!
//! Every decision that could make the audited path fail on its own
//! precondition (a branch no worktree has checked out, a tag that does not
//! exist) is made BEFORE the audited path is called: once it runs, its errors
//! propagate as `legion push` would propagate them, and there is no falling
//! back to real git afterwards.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

use crate::cli::commit::handle_commit;
use crate::cli::push::handle_push;
use crate::cli::util::{audit, open_db};
use crate::{db, error};

/// The audit agent when neither `LEGION_REPO` nor the process directory
/// yields a repo name the router would accept.
const UNKNOWN_AGENT: &str = "unknown";

/// Global options (`man git`) that take a value, in their separate-argument
/// spelling. Each is followed by its value as the next argument.
const GLOBAL_VALUE_SEPARATE: [&str; 7] = [
    "-C",
    "-c",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--config-env",
    "--attr-source",
];

/// Global options that take a value in their `--name=<value>` spelling.
/// `--exec-path` and `--list-cmds` have only this form: a bare `--exec-path`
/// prints the exec path, and is in [`GLOBAL_FLAGS`].
const GLOBAL_VALUE_EQUALS: [&str; 7] = [
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--config-env",
    "--exec-path",
    "--attr-source",
    "--list-cmds",
];

/// Global options that take no value.
const GLOBAL_FLAGS: [&str; 22] = [
    "-p",
    "--paginate",
    "-P",
    "--no-pager",
    "--bare",
    "--no-replace-objects",
    "--no-lazy-fetch",
    "--no-optional-locks",
    "--no-advice",
    "--literal-pathspecs",
    "--glob-pathspecs",
    "--noglob-pathspecs",
    "--icase-pathspecs",
    "-v",
    "--version",
    "-h",
    "--help",
    "--html-path",
    "--man-path",
    "--info-path",
    "--exec-path",
    "--no-literal-pathspecs",
];

/// A global option that moves where git stands: `-C`, `--git-dir`,
/// `--work-tree`. Kept in order, so a git read made for routing stands where
/// the real git would stand.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Location {
    Dir(String),
    GitDir(String),
    WorkTree(String),
}

/// git's global options, and where its subcommand starts.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Globals {
    /// The location-moving options, in the order given.
    location: Vec<Location>,
    /// True when every global option given is `-C`.
    only_dir: bool,
    /// Index of the subcommand in the argument list; `None` when git's
    /// arguments hold no subcommand (`git --version`, `git`).
    subcommand: Option<usize>,
}

impl Globals {
    /// The composed `-C` directory: each `-C` is relative to the one before,
    /// and an empty `-C ""` is a no-op, as git composes them.
    fn dir(&self) -> Option<PathBuf> {
        let mut dir: Option<PathBuf> = None;
        for loc in &self.location {
            if let Location::Dir(value) = loc {
                if value.is_empty() {
                    continue;
                }
                let next = PathBuf::from(value);
                dir = Some(match dir {
                    Some(prev) if next.is_relative() => prev.join(next),
                    _ => next,
                });
            }
        }
        dir
    }

    /// A `git` command standing where the real git would stand.
    fn git(&self) -> Command {
        let mut cmd = Command::new("git");
        for loc in &self.location {
            match loc {
                Location::Dir(v) => cmd.arg("-C").arg(v),
                Location::GitDir(v) => cmd.arg(format!("--git-dir={v}")),
                Location::WorkTree(v) => cmd.arg(format!("--work-tree={v}")),
            };
        }
        cmd
    }
}

/// Read git's global options up to the subcommand.
fn parse_globals(args: &[String]) -> Globals {
    let mut globals = Globals {
        location: Vec::new(),
        only_dir: true,
        subcommand: None,
    };
    let mut i: usize = 0;
    while let Some(arg) = args.get(i) {
        if !arg.starts_with('-') {
            globals.subcommand = Some(i);
            break;
        }
        if GLOBAL_VALUE_SEPARATE.contains(&arg.as_str()) {
            let Some(value) = args.get(i + 1) else {
                // A value-taking option with no value: git errors, there is
                // no subcommand to find.
                break;
            };
            record_global(&mut globals, arg, value);
            i += 2;
            continue;
        }
        if GLOBAL_FLAGS.contains(&arg.as_str()) {
            globals.only_dir = false;
            i += 1;
            continue;
        }
        if let Some((name, value)) = arg.split_once('=')
            && GLOBAL_VALUE_EQUALS.contains(&name)
        {
            record_global(&mut globals, name, value);
            i += 1;
            continue;
        }
        // An option git does not know as global: git errors before running
        // anything, so there is no subcommand.
        break;
    }
    globals
}

fn record_global(globals: &mut Globals, name: &str, value: &str) {
    match name {
        "-C" => globals.location.push(Location::Dir(value.to_string())),
        "--git-dir" => {
            globals.only_dir = false;
            globals.location.push(Location::GitDir(value.to_string()));
        }
        "--work-tree" => {
            globals.only_dir = false;
            globals.location.push(Location::WorkTree(value.to_string()));
        }
        _ => globals.only_dir = false,
    }
}

/// How a `git push` long option takes its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArgKind {
    None,
    Required,
    /// Only in the `--name=<value>` form.
    Optional,
}

/// What a `git push` option means for routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PushEffect {
    Force,
    Delete,
    All,
    Mirror,
    SetUpstream,
    Other,
}

/// How one `git push` option takes its value, and what it means here.
type PushOption = (ArgKind, PushEffect);

/// `git push`'s long options (builtin/push.c), with how each takes a value
/// and what it means here. Abbreviations resolve against this table the way
/// git's parse-options resolves them, so `--force-w` is `--force-with-lease`.
const PUSH_LONG: [(&str, ArgKind, PushEffect); 29] = [
    ("verbose", ArgKind::None, PushEffect::Other),
    ("quiet", ArgKind::None, PushEffect::Other),
    ("repo", ArgKind::Required, PushEffect::Other),
    ("all", ArgKind::None, PushEffect::All),
    ("branches", ArgKind::None, PushEffect::All),
    ("mirror", ArgKind::None, PushEffect::Mirror),
    ("delete", ArgKind::None, PushEffect::Delete),
    ("tags", ArgKind::None, PushEffect::Other),
    ("dry-run", ArgKind::None, PushEffect::Other),
    ("porcelain", ArgKind::None, PushEffect::Other),
    ("force", ArgKind::None, PushEffect::Force),
    ("force-with-lease", ArgKind::Optional, PushEffect::Force),
    ("force-if-includes", ArgKind::None, PushEffect::Force),
    ("recurse-submodules", ArgKind::Required, PushEffect::Other),
    ("thin", ArgKind::None, PushEffect::Other),
    ("receive-pack", ArgKind::Required, PushEffect::Other),
    ("exec", ArgKind::Required, PushEffect::Other),
    ("set-upstream", ArgKind::None, PushEffect::SetUpstream),
    ("progress", ArgKind::None, PushEffect::Other),
    ("prune", ArgKind::None, PushEffect::Other),
    ("no-verify", ArgKind::None, PushEffect::Other),
    ("verify", ArgKind::None, PushEffect::Other),
    ("follow-tags", ArgKind::None, PushEffect::Other),
    ("signed", ArgKind::Optional, PushEffect::Other),
    ("atomic", ArgKind::None, PushEffect::Other),
    ("push-option", ArgKind::Required, PushEffect::Other),
    ("ipv4", ArgKind::None, PushEffect::Other),
    ("ipv6", ArgKind::None, PushEffect::Other),
    ("no-force-with-lease", ArgKind::None, PushEffect::Other),
];

/// `git push`'s arguments, read the way git reads them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct PushArgs {
    /// The first force variant seen, as typed.
    force: Option<String>,
    delete: bool,
    /// `--all` or `--branches`.
    all: bool,
    mirror: bool,
    /// Any option other than `-u`/`--set-upstream`, a `--`, or a malformed
    /// option: the audited path cannot express it.
    other: bool,
    /// The repository, then the refspecs.
    positionals: Vec<String>,
}

impl PushArgs {
    fn remote(&self) -> Option<&str> {
        self.positionals.first().map(String::as_str)
    }

    fn refspecs(&self) -> &[String] {
        self.positionals.get(1..).unwrap_or(&[])
    }
}

/// Resolve a long option name (without `--`, without `=value`) against
/// [`PUSH_LONG`], exact match first, then git's unique-prefix abbreviation,
/// either directly or as a `no-` negation. Returns the entry and whether it
/// was negated; `Err` carries every candidate when the name is unknown or
/// ambiguous.
fn resolve_push_long(name: &str) -> Result<(PushOption, bool), Vec<PushOption>> {
    if let Some((_, kind, effect)) = PUSH_LONG.iter().find(|(n, _, _)| *n == name) {
        return Ok(((*kind, *effect), false));
    }
    let negated: Option<&str> = name.strip_prefix("no-");
    if let Some(rest) = negated
        && let Some((_, kind, _)) = PUSH_LONG.iter().find(|(n, _, _)| *n == rest)
    {
        return Ok(((*kind, PushEffect::Other), true));
    }
    let mut candidates: Vec<(PushOption, bool)> = PUSH_LONG
        .iter()
        .filter(|(n, _, _)| n.starts_with(name))
        .map(|(_, k, e)| ((*k, *e), false))
        .collect();
    if let Some(rest) = negated {
        candidates.extend(
            PUSH_LONG
                .iter()
                .filter(|(n, _, _)| n.starts_with(rest))
                .map(|(_, k, _)| ((*k, PushEffect::Other), true)),
        );
    }
    match candidates.as_slice() {
        [only] => Ok(*only),
        _ => Err(candidates.into_iter().map(|(entry, _)| entry).collect()),
    }
}

/// Read `git push`'s arguments (everything after `push`). Options may appear
/// anywhere before a `--`, as git's parse-options allows.
fn parse_push(args: &[String]) -> PushArgs {
    let mut push = PushArgs::default();
    let mut i: usize = 0;
    let mut options_done = false;
    while let Some(arg) = args.get(i) {
        i += 1;
        if options_done || arg == "-" || !arg.starts_with('-') {
            push.positionals.push(arg.clone());
            continue;
        }
        if arg == "--" {
            options_done = true;
            push.other = true;
            continue;
        }
        if let Some(long) = arg.strip_prefix("--") {
            let (name, inline_value) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (long, None),
            };
            match resolve_push_long(name) {
                Ok(((kind, effect), negated)) => {
                    let mut malformed = false;
                    match (kind, inline_value, negated) {
                        (_, Some(_), true) | (ArgKind::None, Some(_), _) => malformed = true,
                        (ArgKind::Required, None, false) => {
                            if args.get(i).is_none() {
                                malformed = true;
                            }
                            i += 1;
                        }
                        _ => {}
                    }
                    if !malformed && effect == PushEffect::Force && push.force.is_none() {
                        push.force = Some(arg.clone());
                    }
                    apply_push_effect(&mut push, effect, malformed);
                }
                Err(candidates) => {
                    // Unknown or ambiguous: git errors before pushing. A
                    // spelling that could be a force variant is still
                    // treated as one -- refusing it costs nothing.
                    if candidates.iter().any(|(_, e)| *e == PushEffect::Force)
                        && push.force.is_none()
                    {
                        push.force = Some(arg.clone());
                    }
                    push.other = true;
                }
            }
            continue;
        }
        // A group of short options: `-uf`, `-o<value>`, `-uo <value>`.
        let cluster: &str = &arg[1..];
        for (pos, c) in cluster.char_indices() {
            match c {
                'o' => {
                    push.other = true;
                    if cluster[pos + c.len_utf8()..].is_empty() {
                        i += 1;
                    }
                    break;
                }
                'f' => {
                    if push.force.is_none() {
                        push.force = Some(arg.clone());
                    }
                    push.other = true;
                }
                'd' => apply_push_effect(&mut push, PushEffect::Delete, false),
                'u' => apply_push_effect(&mut push, PushEffect::SetUpstream, false),
                _ => push.other = true,
            }
        }
    }
    if push.force.is_none() {
        push.force = push
            .refspecs()
            .iter()
            .find(|refspec| refspec.starts_with('+'))
            .cloned();
    }
    push
}

fn apply_push_effect(push: &mut PushArgs, effect: PushEffect, malformed: bool) {
    if malformed {
        push.other = true;
        return;
    }
    match effect {
        PushEffect::SetUpstream => {}
        PushEffect::Delete => {
            push.delete = true;
            push.other = true;
        }
        PushEffect::All => {
            push.all = true;
            push.other = true;
        }
        PushEffect::Mirror => {
            push.mirror = true;
            push.other = true;
        }
        PushEffect::Force | PushEffect::Other => push.other = true,
    }
}

/// Where a `git commit`'s message comes from, when the audited path can
/// express the commit.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CommitMessage {
    /// One or more `-m`, joined as git joins them.
    Inline(String),
    /// One `-F <file>`.
    File(String),
    /// `-F -`.
    Stdin,
}

/// Read `git commit`'s arguments (everything after `commit`). `Some` only
/// when the message comes from exactly one source and nothing else -- no
/// other option, no pathspec -- is present.
fn parse_commit(args: &[String]) -> Option<CommitMessage> {
    let mut messages: Vec<String> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    let mut i: usize = 0;
    while let Some(arg) = args.get(i) {
        i += 1;
        let (is_message, value): (bool, String) = match arg.as_str() {
            "-m" | "--message" | "-F" | "--file" => {
                let value: String = args.get(i)?.clone();
                i += 1;
                (arg == "-m" || arg == "--message", value)
            }
            // The attached spellings; anything else is an option or a
            // pathspec the audited path cannot express.
            _ => [
                ("--message=", true),
                ("--file=", false),
                ("-m", true),
                ("-F", false),
            ]
            .into_iter()
            .find_map(|(prefix, is_message)| {
                arg.strip_prefix(prefix)
                    .map(|v| (is_message, v.to_string()))
            })?,
        };
        if is_message {
            messages.push(value);
        } else {
            files.push(value);
        }
    }
    match (messages.is_empty(), files.as_slice()) {
        (false, []) => Some(CommitMessage::Inline(messages.join("\n\n"))),
        (true, [file]) if file == "-" => Some(CommitMessage::Stdin),
        (true, [file]) => Some(CommitMessage::File(file.clone())),
        _ => None,
    }
}

/// What the route needs to know about the repository git would act on.
/// Every read that fails answers as "absent", which sends the command to
/// real git -- where git reports the failure itself.
trait RepoFacts {
    /// The checked-out branch; `None` for a detached HEAD or no repository.
    fn current_branch(&self) -> Option<String>;
    fn has_local_branch(&self, name: &str) -> bool;
    fn has_local_tag(&self, name: &str) -> bool;
    fn config(&self, key: &str) -> Option<String>;
    /// Whether any worktree of the repository has `branch` checked out --
    /// the precondition `legion push` resolves its checkout from.
    fn checked_out_somewhere(&self, branch: &str) -> bool;
}

/// [`RepoFacts`] read with real git, standing where the real git would.
struct GitFacts<'a> {
    globals: &'a Globals,
}

impl GitFacts<'_> {
    fn stdout(&self, args: &[&str]) -> Option<String> {
        let out = self.globals.git().args(args).output().ok()?;
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn ref_exists(&self, full: &str) -> bool {
        self.stdout(&["show-ref", "--verify", "--quiet", full])
            .is_some()
    }
}

impl RepoFacts for GitFacts<'_> {
    fn current_branch(&self) -> Option<String> {
        self.stdout(&["symbolic-ref", "--quiet", "--short", "HEAD"])
            .filter(|b| !b.is_empty())
    }

    fn has_local_branch(&self, name: &str) -> bool {
        self.ref_exists(&format!("refs/heads/{name}"))
    }

    fn has_local_tag(&self, name: &str) -> bool {
        self.ref_exists(&format!("refs/tags/{name}"))
    }

    fn config(&self, key: &str) -> Option<String> {
        self.stdout(&["config", "--get", key])
            .filter(|v| !v.is_empty())
    }

    fn checked_out_somewhere(&self, branch: &str) -> bool {
        let wanted = format!("branch refs/heads/{branch}");
        self.stdout(&["worktree", "list", "--porcelain"])
            .is_some_and(|text| text.lines().any(|line| line == wanted))
    }
}

/// The branch or tag an audited push would push.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PushTarget {
    Branch(String),
    Tag(String),
}

/// The rule a push breaks, if any. Force is checked first, then main/master.
fn push_refusal(push: &PushArgs, facts: &dyn RepoFacts) -> Option<error::LegionError> {
    if let Some(token) = &push.force {
        return Some(error::LegionError::PushRefused {
            branch: token.clone(),
            reason: "the force rule: agents never force-push through legion git (a force \
                     variant of push was given)"
                .to_string(),
        });
    }
    main_master_ref(push, facts).map(|dest| error::LegionError::PushRefused {
        branch: dest,
        reason: "the main/master rule: agents never push main or master directly -- merges \
                 happen through a reviewed PR"
            .to_string(),
    })
}

/// Whether `dest` names `main` or `master` as a remote branch.
fn is_protected(dest: &str) -> bool {
    let name: &str = dest
        .strip_prefix("refs/heads/")
        .or_else(|| dest.strip_prefix("heads/"))
        .unwrap_or(dest);
    name == "main" || name == "master"
}

/// The `main`/`master` remote ref this push would update, if any. Keyed on
/// the destination, whatever the typed arguments look like.
fn main_master_ref(push: &PushArgs, facts: &dyn RepoFacts) -> Option<String> {
    if push.all || push.mirror {
        return ["main", "master"]
            .into_iter()
            .find(|b| facts.has_local_branch(b))
            .map(str::to_string);
    }
    let refspecs: &[String] = push.refspecs();
    if refspecs.is_empty() {
        return facts.current_branch().filter(|b| is_protected(b));
    }
    let mut i: usize = 0;
    while let Some(raw) = refspecs.get(i) {
        i += 1;
        if raw == "tag" && refspecs.get(i).is_some() {
            // `tag <name>` pushes refs/tags/<name>, never a branch.
            i += 1;
            continue;
        }
        let spec: &str = raw.strip_prefix('+').unwrap_or(raw);
        let dest: String = match spec.split_once(':') {
            Some((_, dst)) if !dst.is_empty() => dst.to_string(),
            Some((src, _)) => src.to_string(),
            None if !push.delete && (spec == "HEAD" || spec == "@") => {
                match facts.current_branch() {
                    Some(b) => b,
                    None => continue,
                }
            }
            None => spec.to_string(),
        };
        if is_protected(&dest) {
            return Some(dest);
        }
    }
    None
}

/// The audited push for these arguments, when the audited path can express
/// them and its own preconditions hold; `None` sends the push to real git.
fn audited_push_target(
    globals: &Globals,
    push: &PushArgs,
    facts: &dyn RepoFacts,
) -> Option<PushTarget> {
    if !globals.only_dir || push.other || push.force.is_some() {
        return None;
    }
    if push.remote().is_some_and(|r| r != "origin") {
        return None;
    }
    let target: PushTarget = match push.refspecs() {
        [] => {
            let branch: String = facts.current_branch()?;
            if !default_push_is_origin_same_name(&branch, push.remote().is_some(), facts) {
                return None;
            }
            PushTarget::Branch(branch)
        }
        [kw, name] if kw == "tag" => PushTarget::Tag(name.clone()),
        [spec] => {
            if spec == "HEAD" {
                PushTarget::Branch(facts.current_branch()?)
            } else if let Some(tag) = spec.strip_prefix("refs/tags/") {
                PushTarget::Tag(tag.to_string())
            } else if let Some(name) = spec.strip_prefix("refs/heads/") {
                PushTarget::Branch(name.to_string())
            } else if let Some((src, dst)) = spec.split_once(':') {
                if src.is_empty() || (dst != src && dst != format!("refs/heads/{src}")) {
                    return None;
                }
                PushTarget::Branch(src.to_string())
            } else {
                PushTarget::Branch(spec.clone())
            }
        }
        _ => return None,
    };
    let ready: bool = match &target {
        PushTarget::Branch(b) => facts.has_local_branch(b) && facts.checked_out_somewhere(b),
        PushTarget::Tag(t) => facts.has_local_tag(t),
    };
    ready.then_some(target)
}

/// Whether a no-refspec push of `branch` goes to `origin` under the same
/// name: the branch has no upstream, or its upstream is `origin/<branch>`.
/// Without a named remote, a `pushRemote` / `remote.pushDefault` other than
/// `origin` also sends it elsewhere.
fn default_push_is_origin_same_name(
    branch: &str,
    remote_given: bool,
    facts: &dyn RepoFacts,
) -> bool {
    if !remote_given {
        let push_remote: Option<String> = facts
            .config(&format!("branch.{branch}.pushRemote"))
            .or_else(|| facts.config("remote.pushDefault"));
        if push_remote.is_some_and(|r| r != "origin") {
            return false;
        }
    }
    let remote: Option<String> = facts.config(&format!("branch.{branch}.remote"));
    let merge: Option<String> = facts.config(&format!("branch.{branch}.merge"));
    match (remote, merge) {
        (None, _) => true,
        (Some(r), Some(m)) => r == "origin" && m == format!("refs/heads/{branch}"),
        (Some(_), None) => false,
    }
}

/// The audit agent: `LEGION_REPO` when set, else the repo name the router
/// derives from the process directory (the main checkout's name, not a
/// worktree's). Never read from git's arguments.
fn audit_agent() -> String {
    let legion_repo: Option<String> = std::env::var(crate::cmd::hook::LEGION_REPO_ENV).ok();
    let cwd: Option<String> = std::env::current_dir()
        .ok()
        .and_then(|d| d.to_str().map(str::to_string));
    crate::cmd::hook::repo_for(legion_repo.as_deref(), cwd.as_deref())
        .unwrap_or_else(|| UNKNOWN_AGENT.to_string())
}

/// Run git with its own arguments; push and commit go through the audited
/// paths.
///
/// Returns `Err(ExitWith(code))` when real git exits non-zero, so the process
/// exits with git's code.
pub(crate) fn handle_git(args: Vec<OsString>) -> error::Result<()> {
    let text: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let all_utf8: bool = args.iter().all(|a| a.to_str().is_some());
    let globals: Globals = parse_globals(&text);
    let Some(sub_index) = globals.subcommand else {
        return run_real_git(&args, &text, &globals, None);
    };
    let rest: &[String] = text.get(sub_index + 1..).unwrap_or(&[]);
    let facts = GitFacts { globals: &globals };
    match text[sub_index].as_str() {
        "push" => {
            let push: PushArgs = parse_push(rest);
            if let Some(refusal) = push_refusal(&push, &facts) {
                return Err(refusal);
            }
            let target: Option<PushTarget> = if all_utf8 {
                audited_push_target(&globals, &push, &facts)
            } else {
                None
            };
            match target {
                Some(PushTarget::Branch(b)) => {
                    handle_push(audit_agent(), Some(b), None, false, None, globals.dir())
                }
                Some(PushTarget::Tag(t)) => {
                    handle_push(audit_agent(), None, Some(t), false, None, globals.dir())
                }
                None => run_real_git(&args, &text, &globals, Some("push")),
            }
        }
        "commit" => {
            let message: Option<CommitMessage> = if all_utf8 && globals.only_dir {
                parse_commit(rest)
            } else {
                None
            };
            match message {
                Some(CommitMessage::Inline(m)) => {
                    handle_commit(audit_agent(), Some(m), None, None, globals.dir())
                }
                Some(CommitMessage::File(f)) => {
                    handle_commit(audit_agent(), None, Some(f), None, globals.dir())
                }
                Some(CommitMessage::Stdin) => {
                    handle_commit(audit_agent(), None, None, None, globals.dir())
                }
                None => run_real_git(&args, &text, &globals, Some("commit")),
            }
        }
        _ => run_real_git(&args, &text, &globals, None),
    }
}

/// Run real git with the original arguments from the process's own
/// directory, stdio inherited, and exit with git's code. `action` is `Some`
/// for a push or commit, which writes one audit row after git exits.
fn run_real_git(
    args: &[OsString],
    text: &[String],
    globals: &Globals,
    action: Option<&str>,
) -> error::Result<()> {
    // Read before git runs, so the row names the branch git acted on.
    let target_ref: Option<String> = action.map(|_| {
        GitFacts { globals }
            .stdout(&["branch", "--show-current"])
            .map(|b| {
                if b.is_empty() {
                    "detached".to_string()
                } else {
                    b
                }
            })
            .unwrap_or_else(|| "unknown".to_string())
    });

    let status: std::io::Result<std::process::ExitStatus> = Command::new("git").args(args).status();
    let code: Option<i32> = status.as_ref().ok().map(|s| s.code().unwrap_or(1));

    if let (Some(action), Some(target_ref)) = (action, target_ref.as_deref()) {
        let details = serde_json::json!({
            "args": text,
            "real_git": true,
            "exit_code": code,
            "spawn_error": status.as_ref().err().map(|e| e.to_string()),
        })
        .to_string();
        match open_db() {
            Ok(database) => audit(
                &database,
                &db::AuditInput {
                    agent: &audit_agent(),
                    action,
                    target_type: "branch",
                    target_ref,
                    task_id: None,
                    source_type: "git",
                    details: Some(&details),
                    outcome: if code == Some(0) {
                        "success"
                    } else {
                        "failure"
                    },
                },
            ),
            Err(e) => eprintln!("[legion] warning: audit log failed: {e}"),
        }
    }

    match (status, code) {
        (Err(e), _) => Err(error::LegionError::WorkSource(format!(
            "failed to run git: {e}"
        ))),
        (Ok(_), Some(0)) => Ok(()),
        (Ok(_), c) => Err(error::LegionError::ExitWith(c.unwrap_or(1))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    /// A [`RepoFacts`] with no repository behind it.
    #[derive(Default)]
    struct FakeFacts {
        current: Option<String>,
        branches: Vec<String>,
        tags: Vec<String>,
        config: HashMap<String, String>,
        checked_out: Vec<String>,
    }

    impl RepoFacts for FakeFacts {
        fn current_branch(&self) -> Option<String> {
            self.current.clone()
        }
        fn has_local_branch(&self, name: &str) -> bool {
            self.branches.iter().any(|b| b == name)
        }
        fn has_local_tag(&self, name: &str) -> bool {
            self.tags.iter().any(|t| t == name)
        }
        fn config(&self, key: &str) -> Option<String> {
            self.config.get(key).cloned()
        }
        fn checked_out_somewhere(&self, branch: &str) -> bool {
            self.checked_out.iter().any(|b| b == branch)
        }
    }

    /// On `feat`, which is checked out; `main` exists locally; tag `v1`.
    fn on_feat() -> FakeFacts {
        FakeFacts {
            current: Some("feat".to_string()),
            branches: strings(&["feat", "main", "other"]),
            tags: strings(&["v1"]),
            checked_out: strings(&["feat"]),
            ..FakeFacts::default()
        }
    }

    fn target(args: &[&str], facts: &FakeFacts) -> Option<PushTarget> {
        let globals: Globals = parse_globals(&strings(args));
        let sub: usize = globals.subcommand.expect("subcommand");
        let push: PushArgs = parse_push(&strings(&args[sub + 1..]));
        audited_push_target(&globals, &push, facts)
    }

    fn refusal(args: &[&str], facts: &FakeFacts) -> Option<String> {
        push_refusal(&parse_push(&strings(args)), facts).map(|e| e.to_string())
    }

    #[test]
    fn globals_find_the_subcommand_after_every_value_option() {
        let cases: [&[&str]; 14] = [
            &["-C", "d", "push"],
            &["-c", "user.name=x", "push"],
            &["--git-dir", "g", "push"],
            &["--git-dir=g", "push"],
            &["--work-tree", "w", "push"],
            &["--work-tree=w", "push"],
            &["--namespace", "n", "push"],
            &["--namespace=n", "push"],
            &["--config-env", "a=B", "push"],
            &["--config-env=a=B", "push"],
            &["--exec-path=/x", "push"],
            &["--attr-source", "t", "push"],
            &["--attr-source=t", "push"],
            &["--list-cmds=main", "push"],
        ];
        for case in cases {
            let g: Globals = parse_globals(&strings(case));
            assert_eq!(g.subcommand.map(|i| case[i]), Some("push"), "case {case:?}");
        }
    }

    #[test]
    fn globals_flag_options_are_skipped_and_mark_non_dir() {
        for flag in GLOBAL_FLAGS {
            let g: Globals = parse_globals(&strings(&[flag, "push"]));
            assert_eq!(g.subcommand, Some(1), "flag {flag}");
            assert!(!g.only_dir, "flag {flag}");
        }
    }

    #[test]
    fn globals_without_a_subcommand() {
        assert_eq!(parse_globals(&strings(&["--version"])).subcommand, None);
        assert_eq!(parse_globals(&[]).subcommand, None);
        assert_eq!(parse_globals(&strings(&["-C"])).subcommand, None);
        assert_eq!(
            parse_globals(&strings(&["--bogus", "push"])).subcommand,
            None
        );
    }

    #[test]
    fn globals_dash_c_before_is_dir_after_belongs_to_the_subcommand() {
        let g: Globals = parse_globals(&strings(&["-C", "d", "commit", "-C", "HEAD"]));
        assert_eq!(g.subcommand, Some(2));
        assert!(g.only_dir);
        assert_eq!(g.dir(), Some(PathBuf::from("d")));
    }

    #[test]
    fn globals_compose_several_dash_c_like_git() {
        let g: Globals = parse_globals(&strings(&["-C", "a", "-C", "b", "-C", "", "status"]));
        assert_eq!(g.dir(), Some(PathBuf::from("a/b")));
        let g: Globals = parse_globals(&strings(&["-C", "a", "-C", "/abs", "status"]));
        assert_eq!(g.dir(), Some(PathBuf::from("/abs")));
    }

    #[test]
    fn globals_other_than_dash_c_are_not_dir_only() {
        assert!(!parse_globals(&strings(&["-c", "a=b", "push"])).only_dir);
        assert!(!parse_globals(&strings(&["--git-dir=g", "push"])).only_dir);
    }

    #[test]
    fn force_variants_are_refused_naming_the_force_rule() {
        let facts = on_feat();
        let cases: [&[&str]; 10] = [
            &["-f"],
            &["--force"],
            &["--force-with-lease"],
            &["--force-with-lease=feat"],
            &["--force-with-lease=feat:abc123"],
            &["--force-if-includes"],
            &["-uf", "origin", "feat"],
            &["origin", "+feat"],
            &["origin", "feat", "--force-w"],
            &["origin", "feat", "--forc"],
        ];
        for case in cases {
            let msg: String = refusal(case, &facts).unwrap_or_else(|| panic!("{case:?}"));
            assert!(msg.contains("force rule"), "{case:?}: {msg}");
        }
    }

    #[test]
    fn non_force_values_containing_force_are_not_refused() {
        let facts = on_feat();
        for case in [
            &["--no-force-with-lease", "origin", "feat"][..],
            &["-o", "force=1", "origin", "feat"],
            &["-oforce=1", "origin", "feat"],
            &["-uoforce", "origin", "feat"],
            &["--push-option", "force", "origin", "feat"],
            &["--push-option=force", "origin", "feat"],
            &["force-remote", "feat"],
        ] {
            assert_eq!(refusal(case, &facts), None, "{case:?}");
        }
    }

    #[test]
    fn main_master_destinations_are_refused_naming_the_rule() {
        let facts = on_feat();
        for case in [
            &["origin", "main"][..],
            &["origin", "master"],
            &["origin", "feat:main"],
            &["origin", "HEAD:refs/heads/master"],
            &["origin", ":main"],
            &["origin", "--delete", "main"],
            &["--all"],
            &["--branches", "origin"],
            &["--mirror"],
            &["origin", "--del", "main"],
        ] {
            let msg: String = refusal(case, &facts).unwrap_or_else(|| panic!("{case:?}"));
            assert!(msg.contains("main/master rule"), "{case:?}: {msg}");
        }
    }

    #[test]
    fn no_refspec_push_on_main_is_refused() {
        let facts = FakeFacts {
            current: Some("main".to_string()),
            ..FakeFacts::default()
        };
        assert!(refusal(&[], &facts).is_some());
        assert!(refusal(&["origin", "HEAD"], &facts).is_some());
    }

    #[test]
    fn all_without_a_local_main_is_not_refused() {
        let facts = FakeFacts {
            current: Some("feat".to_string()),
            branches: strings(&["feat"]),
            ..FakeFacts::default()
        };
        assert_eq!(refusal(&["--all"], &facts), None);
        assert_eq!(refusal(&["origin", "feat", "tag", "main"], &facts), None);
    }

    #[test]
    fn audited_push_forms_map_to_the_branch() {
        let facts = on_feat();
        for case in [
            &["push"][..],
            &["push", "origin"],
            &["push", "-u", "origin", "feat"],
            &["push", "--set-upstream", "origin", "feat"],
            &["push", "origin", "HEAD"],
            &["push", "origin", "feat:feat"],
            &["push", "origin", "feat:refs/heads/feat"],
            &["push", "origin", "refs/heads/feat"],
            &["-C", "dir", "push", "origin", "feat"],
        ] {
            assert_eq!(
                target(case, &facts),
                Some(PushTarget::Branch("feat".to_string())),
                "{case:?}"
            );
        }
    }

    #[test]
    fn audited_push_forms_map_to_the_tag() {
        let facts = on_feat();
        for case in [
            &["push", "origin", "tag", "v1"][..],
            &["push", "origin", "refs/tags/v1"],
        ] {
            assert_eq!(
                target(case, &facts),
                Some(PushTarget::Tag("v1".to_string())),
                "{case:?}"
            );
        }
    }

    #[test]
    fn inexpressible_pushes_go_to_real_git() {
        let facts = on_feat();
        for case in [
            &["push", "--tags"][..],
            &["push", "upstream", "feat"],
            &["push", "origin", "feat", "other"],
            &["push", "origin", "feat:other"],
            &["push", "-q", "origin", "feat"],
            &["push", "--dry-run", "origin", "feat"],
            &["-c", "a=b", "push", "origin", "feat"],
            &["--git-dir=g", "push", "origin", "feat"],
            &["push", "--", "origin", "feat"],
            // `other` exists but no worktree has it checked out: the
            // audited path would fail in resolve_checkout.
            &["push", "origin", "other"],
            &["push", "origin", "nosuch"],
            &["push", "origin", "tag", "v2"],
        ] {
            assert_eq!(target(case, &facts), None, "{case:?}");
        }
    }

    #[test]
    fn no_refspec_push_translates_only_when_it_goes_to_origin_under_the_same_name() {
        let mut facts = on_feat();
        facts
            .config
            .insert("branch.feat.remote".to_string(), "origin".to_string());
        facts.config.insert(
            "branch.feat.merge".to_string(),
            "refs/heads/feat".to_string(),
        );
        assert!(target(&["push"], &facts).is_some());

        facts.config.insert(
            "branch.feat.merge".to_string(),
            "refs/heads/elsewhere".to_string(),
        );
        assert_eq!(target(&["push"], &facts), None);

        facts.config.insert(
            "branch.feat.merge".to_string(),
            "refs/heads/feat".to_string(),
        );
        facts
            .config
            .insert("branch.feat.remote".to_string(), "fork".to_string());
        assert_eq!(target(&["push"], &facts), None);

        let mut facts = on_feat();
        facts
            .config
            .insert("remote.pushDefault".to_string(), "fork".to_string());
        assert_eq!(target(&["push"], &facts), None);
        assert!(target(&["push", "origin"], &facts).is_some());
    }

    #[test]
    fn detached_head_push_goes_to_real_git() {
        let facts = FakeFacts {
            current: None,
            ..on_feat()
        };
        assert_eq!(target(&["push"], &facts), None);
        assert_eq!(target(&["push", "origin", "HEAD"], &facts), None);
    }

    #[test]
    fn commit_message_sources() {
        let cases: [(&[&str], CommitMessage); 11] = [
            (
                &["-m", "a", "-m", "b"],
                CommitMessage::Inline("a\n\nb".to_string()),
            ),
            (&["-mfoo"], CommitMessage::Inline("foo".to_string())),
            (
                &["--message", "foo"],
                CommitMessage::Inline("foo".to_string()),
            ),
            (&["--message=foo"], CommitMessage::Inline("foo".to_string())),
            (
                &["-m", "a", "--message=b"],
                CommitMessage::Inline("a\n\nb".to_string()),
            ),
            (&["-F", "f"], CommitMessage::File("f".to_string())),
            (&["-Ff"], CommitMessage::File("f".to_string())),
            (&["--file", "f"], CommitMessage::File("f".to_string())),
            (&["--file=f"], CommitMessage::File("f".to_string())),
            (&["-F", "-"], CommitMessage::Stdin),
            (&["-m", "-m"], CommitMessage::Inline("-m".to_string())),
        ];
        for (args, want) in cases {
            assert_eq!(parse_commit(&strings(args)), Some(want), "{args:?}");
        }
    }

    #[test]
    fn inexpressible_commits_go_to_real_git() {
        for case in [
            &[][..],
            &["-am", "x"],
            &["--amend", "--no-edit"],
            &["-C", "HEAD"],
            &["-m", "x", "-F", "f"],
            &["-F", "f", "-F", "g"],
            &["-m", "x", "file.txt"],
            &["-m", "x", "--", "file.txt"],
            &["-m"],
            &["--no-verify", "-m", "x"],
        ] {
            assert_eq!(parse_commit(&strings(case)), None, "{case:?}");
        }
    }

    #[test]
    fn protected_names() {
        for dest in ["main", "master", "refs/heads/main", "heads/master"] {
            assert!(is_protected(dest), "{dest}");
        }
        for dest in ["feat", "mainline", "refs/tags/main", "refs/heads/feat"] {
            assert!(!is_protected(dest), "{dest}");
        }
    }
}
