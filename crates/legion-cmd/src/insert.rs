//! Proxy insertion (#1337): `legion ` before each proxied command name.
//!
//! The router puts `legion ` in front of every simple command whose name
//! word equals a proxy name exactly as typed, and leaves every other byte of
//! the command as it was. It works in three separate steps, so each can be
//! checked on its own:
//!
//! 1. [`plan`] parses the command and finds the byte offset of every proxied
//!    name at each position the parser exposes as a simple command -- top
//!    level, pipelines, lists, subshells and brace groups, loop, `if` and
//!    `case` bodies, function bodies, and `$( )`, backtick and process
//!    substitutions, behind any assignment prefix. It also renders the
//!    command's parse, with each found name already written as `legion
//!    <name>`, into a location-free [`Placement::expected`] text.
//! 2. [`insert_at`] writes `legion ` into the command text at those offsets.
//! 3. [`verify`] parses the rewritten text and renders it the same way, with
//!    nothing marked. The two renderings match only when the rewritten
//!    command parses exactly as the original plus one `legion` word before
//!    each proxied name. Anything else -- an offset a few bytes off that
//!    lands inside another word, a quote, or the wrong command -- is a
//!    mismatch, and the rewritten command never runs.
//!
//! # Where a name starts
//!
//! The pinned parser records some command words' start a few bytes early:
//! a word that follows a backslash line continuation, or leading
//! whitespace, spans from where its token began. The end of the span is
//! exact, and the word's raw value never carries the continuation, so a
//! word's source start is recovered by aligning its value against the
//! source text between the span's start and end with continuations
//! removed ([`locate`]). A word that cannot be aligned is not placeable,
//! and nothing inside it is changed.
//!
//! # What is never touched
//!
//! Only a simple command's name word is a site. A path (`/usr/bin/git`), an
//! argument (`xargs grep`, `echo git`), a name already behind `legion`, and
//! text inside quotes, a heredoc, a shell payload (`sh -c '...'`) or an
//! interpreter body are never a name word, so they are left alone.

use std::collections::BTreeSet;
use std::io::Cursor;

use brush_parser::ast::{self, Command, CommandPrefixOrSuffixItem, CompoundCommand, CompoundList};
use brush_parser::word::{self, WordPiece};
use brush_parser::{Parser, ParserOptions};

use crate::splitter::{MAX_DEPTH, too_deep_to_parse};

/// The word inserted before each proxied name, with its separating space.
const INSERTED: &str = "legion ";

/// Why an insertion was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InsertError {
    /// The rewritten command did not parse as the original plus the added
    /// words. `offset` is the first insertion site, in the original.
    #[error("legion could not place `legion` before the name at byte {offset}")]
    Misplaced { offset: usize },
}

/// Where `legion ` goes and what the rewritten command must parse to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    /// Byte offsets into the original command, ascending: each is the first
    /// byte of a proxied name word.
    pub sites: Vec<usize>,
    /// The original's parse, rendered without source locations, with each
    /// name at a site written as `legion <name>`.
    expected: String,
}

/// Finds every proxied name in `command` and renders the parse the
/// rewritten command must have. `None` when the command does not parse or
/// is nested past the depth guard: nothing can be placed in it.
pub fn plan(command: &str, proxy: &[String]) -> Option<Placement> {
    let mut walk = Walk {
        proxy,
        marking: true,
        sites: BTreeSet::new(),
    };
    let expected: String = walk.program(command, Some(0), 0)?;
    Some(Placement {
        sites: walk.sites.into_iter().collect(),
        expected,
    })
}

/// `command` with `legion ` written at each offset in `sites`. An offset
/// past the end or inside a character is skipped rather than panicking; the
/// verification step then refuses the result.
pub fn insert_at(command: &str, sites: &[usize]) -> String {
    let mut out = String::with_capacity(command.len() + sites.len() * INSERTED.len());
    let mut from: usize = 0;
    for &site in sites {
        if site < from || site > command.len() || !command.is_char_boundary(site) {
            continue;
        }
        out.push_str(&command[from..site]);
        out.push_str(INSERTED);
        from = site;
    }
    out.push_str(&command[from..]);
    out
}

/// Checks that `rewritten` parses exactly as the original `placement` was
/// planned from, plus one `legion` word before each proxied name.
pub fn verify(placement: &Placement, rewritten: &str) -> Result<(), InsertError> {
    let mut walk = Walk {
        proxy: &[],
        marking: false,
        sites: BTreeSet::new(),
    };
    let offset: usize = placement.sites.first().copied().unwrap_or(0);
    match walk.program(rewritten, Some(0), 0) {
        Some(actual) if actual == placement.expected => Ok(()),
        _ => Err(InsertError::Misplaced { offset }),
    }
}

/// The command with `legion ` before each proxied name, or `None` when there
/// is nothing to insert. A rewrite that does not verify is an error: it
/// never runs.
pub fn proxy_insertion(command: &str, proxy: &[String]) -> Result<Option<String>, InsertError> {
    if proxy.is_empty() {
        return Ok(None);
    }
    let Some(placement) = plan(command, proxy) else {
        return Ok(None);
    };
    if placement.sites.is_empty() {
        return Ok(None);
    }
    let rewritten: String = insert_at(command, &placement.sites);
    verify(&placement, &rewritten)?;
    Ok(Some(rewritten))
}

/// One walk over a command's parse: when `marking`, it records each proxied
/// name's offset and writes the name as `legion <name>` in the rendering.
struct Walk<'a> {
    proxy: &'a [String],
    marking: bool,
    sites: BTreeSet<usize>,
}

impl Walk<'_> {
    /// Parses `text` and renders it. `base` is the byte offset of `text[0]`
    /// in the original command, or `None` when `text` does not come from it
    /// byte for byte (nothing in it can be placed). `None` when `text` is
    /// too deep to parse safely or does not parse.
    fn program(&mut self, text: &str, base: Option<usize>, depth: u8) -> Option<String> {
        if depth >= MAX_DEPTH || too_deep_to_parse(text, depth) {
            return None;
        }
        let options = ParserOptions::default();
        let mut parser = Parser::new(Cursor::new(text.as_bytes()), &options);
        let mut program: ast::Program = parser.parse_program().ok()?;
        for list in &mut program.complete_commands {
            self.list(list, text, base, depth);
        }
        Some(strip_locations(&format!("{program:?}")))
    }

    fn list(&mut self, list: &mut CompoundList, text: &str, base: Option<usize>, depth: u8) {
        for item in &mut list.0 {
            let and_or: &mut ast::AndOrList = &mut item.0;
            self.pipeline(&mut and_or.first, text, base, depth);
            for next in &mut and_or.additional {
                match next {
                    ast::AndOr::And(pipeline) | ast::AndOr::Or(pipeline) => {
                        self.pipeline(pipeline, text, base, depth)
                    }
                }
            }
        }
    }

    fn pipeline(
        &mut self,
        pipeline: &mut ast::Pipeline,
        text: &str,
        base: Option<usize>,
        depth: u8,
    ) {
        for command in &mut pipeline.seq {
            self.command(command, text, base, depth);
        }
    }

    fn command(&mut self, command: &mut Command, text: &str, base: Option<usize>, depth: u8) {
        match command {
            Command::Simple(simple) => self.simple(simple, text, base, depth),
            Command::Compound(compound, _) => self.compound(compound, text, base, depth),
            Command::Function(function) => self.compound(&mut function.body.0, text, base, depth),
            // A `[[ ]]` test runs no simple command of its own.
            Command::ExtendedTest(..) => {}
        }
    }

    fn compound(
        &mut self,
        compound: &mut CompoundCommand,
        text: &str,
        base: Option<usize>,
        depth: u8,
    ) {
        match compound {
            CompoundCommand::BraceGroup(group) => self.list(&mut group.list, text, base, depth),
            CompoundCommand::Subshell(subshell) => self.list(&mut subshell.list, text, base, depth),
            CompoundCommand::ForClause(clause) => {
                // `for f in $(git ls-files)`: a substitution in the values.
                if let Some(values) = &mut clause.values {
                    for value in values {
                        self.word(value, text, base, depth);
                    }
                }
                self.list(&mut clause.body.list, text, base, depth)
            }
            CompoundCommand::ArithmeticForClause(clause) => {
                self.list(&mut clause.body.list, text, base, depth)
            }
            CompoundCommand::CaseClause(clause) => {
                self.word(&mut clause.value, text, base, depth);
                for item in &mut clause.cases {
                    if let Some(body) = &mut item.cmd {
                        self.list(body, text, base, depth);
                    }
                }
            }
            CompoundCommand::IfClause(clause) => {
                self.list(&mut clause.condition, text, base, depth);
                self.list(&mut clause.then, text, base, depth);
                if let Some(elses) = &mut clause.elses {
                    for other in elses {
                        if let Some(condition) = &mut other.condition {
                            self.list(condition, text, base, depth);
                        }
                        self.list(&mut other.body, text, base, depth);
                    }
                }
            }
            CompoundCommand::WhileClause(clause) | CompoundCommand::UntilClause(clause) => {
                self.list(&mut clause.0, text, base, depth);
                self.list(&mut clause.1.list, text, base, depth);
            }
            CompoundCommand::Coprocess(coprocess) => {
                self.command(&mut coprocess.body, text, base, depth)
            }
            CompoundCommand::Arithmetic(_) => {}
        }
    }

    fn simple(
        &mut self,
        simple: &mut ast::SimpleCommand,
        text: &str,
        base: Option<usize>,
        depth: u8,
    ) {
        let items = simple
            .prefix
            .iter_mut()
            .flat_map(|prefix| prefix.0.iter_mut())
            .chain(
                simple
                    .suffix
                    .iter_mut()
                    .flat_map(|suffix| suffix.0.iter_mut()),
            );
        for item in items {
            match item {
                CommandPrefixOrSuffixItem::Word(word) => self.word(word, text, base, depth),
                CommandPrefixOrSuffixItem::AssignmentWord(assignment, word) => {
                    // The assignment repeats the word's value with no source
                    // location; the word carries the same text and is the one
                    // rendered, so the copy is blanked on both sides.
                    assignment.value = ast::AssignmentValue::Scalar(ast::Word {
                        value: String::new(),
                        loc: None,
                    });
                    self.word(word, text, base, depth);
                }
                CommandPrefixOrSuffixItem::ProcessSubstitution(_, subshell) => {
                    self.list(&mut subshell.list, text, base, depth)
                }
                // `> "$(git rev-parse --show-toplevel)/out"`, `<<< "$(gh ...)"`.
                CommandPrefixOrSuffixItem::IoRedirect(
                    ast::IoRedirect::File(_, _, ast::IoFileRedirectTarget::Filename(word))
                    | ast::IoRedirect::HereString(_, word),
                ) => self.word(word, text, base, depth),
                CommandPrefixOrSuffixItem::IoRedirect(_) => {}
            }
        }

        let Some(name) = &mut simple.word_or_name else {
            return;
        };
        if self.marking && self.proxy.contains(&name.value) {
            let site: Option<usize> =
                base.and_then(|b| locate(text, name).map(|(start, _)| b + start));
            if let Some(site) = site {
                self.sites.insert(site);
                let moved = ast::Word {
                    value: std::mem::replace(&mut name.value, "legion".to_string()),
                    loc: name.loc.clone(),
                };
                simple
                    .suffix
                    .get_or_insert_with(|| ast::CommandSuffix(Vec::new()))
                    .0
                    .insert(0, CommandPrefixOrSuffixItem::Word(moved));
                return;
            }
        }
        self.word(name, text, base, depth);
    }

    /// Renders a word whose value holds a command or backtick substitution:
    /// each substitution's text is replaced by the rendering of its own
    /// parse, so a proxied name inside it is compared by structure, not by
    /// raw text. A word with no substitution is left as it is.
    fn word(&mut self, word: &mut ast::Word, text: &str, base: Option<usize>, depth: u8) {
        let Ok(pieces) = word::parse(&word.value, &ParserOptions::default()) else {
            return;
        };
        let mut substitutions: Vec<(usize, usize, usize, String)> = Vec::new();
        collect_substitutions(&pieces, &mut substitutions);
        if substitutions.is_empty() {
            return;
        }
        let located: Option<(usize, Vec<usize>)> = base.and_then(|_| locate(text, word));
        let value: String = word.value.clone();
        let mut rendered = String::with_capacity(value.len());
        let mut from: usize = 0;
        for (start, end, open, inner) in substitutions {
            let (Some(before), true) = (value.get(from..start), end <= value.len()) else {
                return;
            };
            rendered.push_str(before);
            // The substitution's text as it sits in the source, when the word
            // aligns with it; otherwise its value, which nothing is placed in.
            let source: Option<(usize, &str)> = match (&located, base) {
                (Some((_, map)), Some(b)) => {
                    let inner_start: Option<usize> = map.get(start + open).copied();
                    let inner_end: Option<usize> = map.get(end - 1).copied();
                    match (inner_start, inner_end) {
                        (Some(s), Some(e)) if s <= e => text
                            .get(s..e)
                            .filter(|slice| remove_continuations(slice) == inner)
                            .map(|slice| (b + s, slice)),
                        _ => None,
                    }
                }
                _ => None,
            };
            let next_depth: u8 = depth.saturating_add(1);
            let inner_render: String = match source {
                Some((inner_base, slice)) => self.program(slice, Some(inner_base), next_depth),
                None => self.program(&inner, None, next_depth),
            }
            .unwrap_or_else(|| format!("UNPARSED {inner:?}"));
            rendered.push_str(&format!("<<SUB {inner_render} SUB>>"));
            from = end;
        }
        if let Some(after) = value.get(from..) {
            rendered.push_str(after);
        }
        word.value = rendered;
    }
}

/// Every command or backtick substitution among `pieces`, including those
/// inside double quotes, as (start, end, opener length, inner text), with
/// start and end indexing the word's value. Sorted by start.
fn collect_substitutions(
    pieces: &[word::WordPieceWithSource],
    out: &mut Vec<(usize, usize, usize, String)>,
) {
    for piece in pieces {
        match &piece.piece {
            WordPiece::CommandSubstitution(inner) => {
                out.push((piece.start_index, piece.end_index, 2, inner.clone()));
            }
            WordPiece::BackquotedCommandSubstitution(inner) => {
                out.push((piece.start_index, piece.end_index, 1, inner.clone()));
            }
            WordPiece::DoubleQuotedSequence(nested)
            | WordPiece::GettextDoubleQuotedSequence(nested) => {
                collect_substitutions(nested, out);
            }
            _ => {}
        }
    }
    out.sort_by_key(|(start, ..)| *start);
}

/// Aligns `word`'s value with the source `text` its span covers: the start
/// of the value in `text`, and for each byte of the value, its offset in
/// `text`. The span may start early (before a line continuation or
/// whitespace), and a line continuation inside the word is in the source
/// but not in the value; both are accounted for. `None` when the word has no
/// span or the value cannot be aligned.
fn locate(text: &str, word: &ast::Word) -> Option<(usize, Vec<usize>)> {
    let span = word.loc.as_ref()?;
    let (start, end) = (span.start.offset, span.end.offset);
    let source: &str = text.get(start..end)?;
    let value: &[u8] = word.value.as_bytes();
    let bytes: &[u8] = source.as_bytes();
    // Skip what the span holds before the word itself.
    let mut skip: usize = 0;
    loop {
        match bytes.get(skip..) {
            Some([b'\\', b'\n', ..]) => skip += 2,
            Some([b' ' | b'\t', ..]) => skip += 1,
            _ => break,
        }
    }
    let mut map: Vec<usize> = Vec::with_capacity(value.len());
    let mut at: usize = skip;
    while map.len() < value.len() {
        if bytes.get(at..at + 2) == Some(b"\\\n".as_slice()) {
            at += 2;
            continue;
        }
        if bytes.get(at) != value.get(map.len()) {
            return None;
        }
        map.push(start + at);
        at += 1;
    }
    (at == bytes.len()).then(|| (start + skip, map))
}

/// `text` with every backslash line continuation removed, the way the
/// parser reads it.
fn remove_continuations(text: &str) -> String {
    text.replace("\\\n", "")
}

/// A `Debug` rendering with every source position replaced by `P`, so two
/// parses compare by structure and words alone: inserting text shifts every
/// later position without changing what the command is.
fn strip_locations(debug: &str) -> String {
    const OPEN: &str = "SourcePosition {";
    let mut out = String::with_capacity(debug.len());
    let mut rest: &str = debug;
    while let Some(at) = rest.find(OPEN) {
        out.push_str(&rest[..at]);
        out.push('P');
        let after: &str = &rest[at..];
        match after.find('}') {
            Some(close) => rest = &after[close + 1..],
            None => {
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proxy() -> Vec<String> {
        ["git", "gh", "grep", "rg"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    fn inserted(command: &str) -> Option<String> {
        proxy_insertion(command, &proxy()).expect("verifies")
    }

    #[test]
    fn each_position_gets_legion_before_the_name() {
        let cases: &[(&str, &str)] = &[
            ("git status", "legion git status"),
            ("gh pr view 5", "legion gh pr view 5"),
            ("ls | grep x", "ls | legion grep x"),
            (
                "cd a && git log || rg foo; gh x",
                "cd a && legion git log || legion rg foo; legion gh x",
            ),
            ("(git status)", "(legion git status)"),
            ("{ git status; }", "{ legion git status; }"),
            ("x=$(git rev-parse HEAD)", "x=$(legion git rev-parse HEAD)"),
            ("echo `git log`", "echo `legion git log`"),
            ("echo \"a $(git log) b\"", "echo \"a $(legion git log) b\""),
            ("diff <(git show a) b", "diff <(legion git show a) b"),
            (
                "for f in a b; do grep x $f; done",
                "for f in a b; do legion grep x $f; done",
            ),
            (
                "while true; do git fetch; done",
                "while true; do legion git fetch; done",
            ),
            (
                "if git diff --quiet; then rg y; else gh z; fi",
                "if legion git diff --quiet; then legion rg y; else legion gh z; fi",
            ),
            (
                "case $x in a) git a;; *) rg b;; esac",
                "case $x in a) legion git a;; *) legion rg b;; esac",
            ),
            ("f() { git status; }", "f() { legion git status; }"),
            ("FOO=1 git status", "FOO=1 legion git status"),
            ("A=1 B=2 grep x", "A=1 B=2 legion grep x"),
            (
                "git a && git b && grep c | rg d",
                "legion git a && legion git b && legion grep c | legion rg d",
            ),
            ("time git status", "time legion git status"),
            ("! git diff --quiet", "! legion git diff --quiet"),
            ("git push -h", "legion git push -h"),
            (
                "for f in $(git ls-files); do rg -c x \"$f\"; done",
                "for f in $(legion git ls-files); do legion rg -c x \"$f\"; done",
            ),
            (
                "case $(git branch --show-current) in main) echo m;; esac",
                "case $(legion git branch --show-current) in main) echo m;; esac",
            ),
            (
                "echo x > \"$(git rev-parse --show-toplevel)/out\"",
                "echo x > \"$(legion git rev-parse --show-toplevel)/out\"",
            ),
            (
                "cat <<< \"$(gh pr view)\"",
                "cat <<< \"$(legion gh pr view)\"",
            ),
            ("x=$(cd a && $(git log))", "x=$(cd a && $(legion git log))"),
        ];
        for (typed, expected) in cases {
            assert_eq!(inserted(typed).as_deref(), Some(*expected), "{typed}");
        }
    }

    #[test]
    fn a_name_after_a_line_continuation_gets_legion_directly_before_it() {
        let cases: &[(&str, &str)] = &[
            (
                "echo a && \\\ngit status",
                "echo a && \\\nlegion git status",
            ),
            ("ls | \\\n   rg foo", "ls | \\\n   legion rg foo"),
            (
                "cd /x && \\\n  git status && \\\n  git log",
                "cd /x && \\\n  legion git status && \\\n  legion git log",
            ),
            (
                "x=$(cd a && \\\n  git log)",
                "x=$(cd a && \\\n  legion git log)",
            ),
        ];
        for (typed, expected) in cases {
            assert_eq!(inserted(typed).as_deref(), Some(*expected), "{typed:?}");
        }
    }

    #[test]
    fn paths_arguments_quotes_heredocs_and_payloads_are_left_alone() {
        for typed in [
            "/usr/bin/git status",
            "xargs grep foo",
            "find . -exec grep x {} \\;",
            "echo git",
            "legion git status",
            "echo 'git status'",
            "echo \"git status\"",
            "sh -c 'git status'",
            "bash -c \"grep x\"",
            "python3 -c \"import glob; print(glob.glob('*'))\"",
            "cat <<EOF\ngit status\nEOF",
            "'git' status",
            "\\git status",
            "gitk",
            "grep() { echo; }",
            "echo \"use rg for search\"",
        ] {
            assert_eq!(inserted(typed), None, "{typed}");
        }
    }

    #[test]
    fn an_insertion_whose_reparse_differs_is_refused() {
        // A forced span offset: the plan's true sites, each shifted a byte
        // early or late, must never verify.
        for typed in [
            "git status",
            "ls | grep x",
            "echo a && \\\ngit status",
            "x=$(git log)",
            "echo \"$(rg foo)\" && gh pr view",
        ] {
            let placement = plan(typed, &proxy()).expect("parses");
            assert!(!placement.sites.is_empty(), "{typed}");
            for shift in [-1_isize, 1] {
                let shifted: Vec<usize> = placement
                    .sites
                    .iter()
                    .map(|s| s.saturating_add_signed(shift))
                    .collect();
                // A shift onto whitespace before the name parses the same as
                // the true site, so it verifies; only a shift that changes
                // the parse is a misplacement.
                let harmless = shifted.iter().zip(&placement.sites).all(|(s, t)| {
                    typed[*s.min(t)..*s.max(t)]
                        .chars()
                        .all(|c| c == ' ' || c == '\t')
                });
                if harmless {
                    continue;
                }
                let rewritten = insert_at(typed, &shifted);
                assert_eq!(
                    verify(&placement, &rewritten),
                    Err(InsertError::Misplaced {
                        offset: placement.sites[0]
                    }),
                    "{typed:?} shifted {shift}: {rewritten:?}"
                );
            }
            // The true sites verify.
            assert_eq!(
                verify(&placement, &insert_at(typed, &placement.sites)),
                Ok(())
            );
        }
    }

    #[test]
    fn a_dropped_site_is_refused() {
        let placement = plan("git a && git b", &proxy()).expect("parses");
        assert_eq!(placement.sites.len(), 2);
        let rewritten = insert_at("git a && git b", &placement.sites[..1]);
        assert!(verify(&placement, &rewritten).is_err());
    }

    #[test]
    fn nothing_is_inserted_with_an_empty_proxy_list_or_an_unparsable_command() {
        assert_eq!(proxy_insertion("git status", &[]), Ok(None));
        assert_eq!(proxy_insertion("git 'unterminated", &proxy()), Ok(None));
    }

    #[test]
    fn locate_recovers_a_start_the_span_records_early() {
        let text = "echo a && \\\ngit status";
        let placement = plan(text, &proxy()).expect("parses");
        assert_eq!(placement.sites, vec![text.find("git").expect("present")]);
    }
}
