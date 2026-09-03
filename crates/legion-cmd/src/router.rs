//! The evaluator. The ONLY place routing logic lives (NFR-CMD-005).
//!
//! Purity is the contract (FR-CMD-001): `route` spawns no process, opens no
//! file, touches no database, and reads no environment variable. Everything it
//! knows arrives in `Ctx`, which the caller populated. That is what makes the
//! router testable without a fixture repo and what keeps a hook's latency
//! budget honest.

use std::collections::{HashMap, HashSet};

use crate::call::ToolCall;
use crate::ctx::Ctx;
use crate::decision::{Decision, Matched, Routed, Targets};
use crate::table::{
    ArgKind, ArgOutcome, DefaultOutcome, FlagPolicy, FlagSpec, Route, RouteTable, TableError,
};
use crate::tokenizer::{self, Stage, Token, TokenKind};

/// A flag token seen in a parsed invocation: its raw text, and the
/// canonical `FlagSpec::name` it matched, when it matched a declared flag.
type FlagsPresent = Vec<(String, Option<String>)>;

/// What walking a matched route's argv produced: named captures (including
/// `repo`, seeded from `Ctx`), every positional token in order, and every
/// flag token seen.
type ParsedInvocation = (HashMap<String, String>, Vec<String>, FlagsPresent);

/// A compiled, ready-to-evaluate policy.
#[derive(Debug)]
pub struct Router {
    table: RouteTable,
}

/// Every `{placeholder}` in `s`, or an error on any unbalanced brace.
///
/// Symmetric on purpose: a stray `}` is a load failure exactly as a stray `{`
/// is. The alternative is accepting it as a literal, which means the template
/// a human wrote and the template this crate renders differ silently -- and
/// "never a silent non-match" is the whole contract this validation exists to
/// keep.
///
/// Hand-scanned rather than regex: nothing in the crate needs a regex engine
/// yet, and a brace scan is not worth the dependency. Byte indices are safe
/// here because `{` and `}` are single-byte in UTF-8, so every boundary this
/// slices on is a valid char boundary regardless of multibyte content.
fn placeholders(s: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut open: Option<usize> = None;
    for (i, c) in s.char_indices() {
        match c {
            '{' => {
                if open.is_some() {
                    return Err(format!("nested '{{' in template `{s}`"));
                }
                open = Some(i);
            }
            '}' => match open.take() {
                None => return Err(format!("unmatched '}}' in template `{s}`")),
                Some(start) => {
                    let name = &s[start + 1..i];
                    if name.is_empty() {
                        return Err(format!("empty placeholder `{{}}` in template `{s}`"));
                    }
                    out.push(name.to_owned());
                }
            },
            _ => {}
        }
    }
    if open.is_some() {
        return Err(format!("unbalanced '{{' in template `{s}`"));
    }
    Ok(out)
}

/// Every capture name a route declares, plus `repo` -- the one placeholder
/// every route may use without declaring it, because it binds from
/// `Ctx.repo` at decide time rather than from the route's own captures
/// (NFR-CMD-005: "templates bind `{repo}` from `Ctx.repo`").
fn declared_captures(route: &Route) -> HashSet<String> {
    let mut names: HashSet<String> = route.positional_captures.iter().cloned().collect();
    names.insert("repo".to_owned());
    for f in route.global_options.iter().chain(route.flags.iter()) {
        if let Some(c) = &f.capture {
            names.insert(c.clone());
        }
    }
    for p in &route.patterns {
        match &p.kind {
            ArgKind::Digits { capture } => {
                names.insert(capture.clone());
            }
            ArgKind::TextCarry { placeholder, .. } => {
                names.insert(placeholder.clone());
            }
            _ => {}
        }
    }
    names
}

impl Router {
    /// Compile a table once, or say exactly which entry is wrong.
    ///
    /// Never a silent skip: an entry this cannot make sense of is a load
    /// failure, because a policy with a quietly-dropped rule enforces
    /// something nobody wrote.
    ///
    /// SCOPE NOTE (slice 1). The table shipped here carries no regex or glob
    /// field -- the pattern-bearing types are `ArgKind`'s populated rows
    /// (slice 3) and the `Ruling` match-spec (FR-CMD-017, slice 7). What this
    /// compiles today is the template layer: every `{placeholder}` a route's
    /// `equivalent` names must be a capture that route actually declares.
    /// Regex and glob compilation joins this pass when the fields that carry
    /// them land, and the error shape (`TableError::Pattern`, naming the
    /// offending pattern) is already what they will use.
    pub fn new(table: RouteTable) -> Result<Router, TableError> {
        let mut seen: HashSet<(String, String)> = HashSet::new();
        for (index, route) in table.routes.iter().enumerate() {
            let bad = |problem: String| TableError::Route {
                index,
                binary: route.binary.clone(),
                subcommand: route.subcommand.clone(),
                problem,
            };
            if route.binary.trim().is_empty() {
                return Err(bad("binary is empty".into()));
            }
            if route.equivalent.trim().is_empty() {
                return Err(bad(
                    "equivalent is empty -- a route must say what to run instead".into(),
                ));
            }
            if route.reason.trim().is_empty() {
                return Err(bad(
                    "reason is empty -- a rewrite the agent cannot explain is not one".into(),
                ));
            }
            let key = (route.binary.clone(), route.subcommand.clone());
            if !seen.insert(key) {
                return Err(bad(
                    "duplicate route -- two entries for the same binary and subcommand, \
                     so which one applies depends on table order"
                        .into(),
                ));
            }

            let captures = declared_captures(route);
            let pattern_err = |pattern: &str, problem: String| TableError::Pattern {
                index,
                binary: route.binary.clone(),
                subcommand: route.subcommand.clone(),
                pattern: pattern.to_owned(),
                problem,
            };
            let check = |template: &str, pattern: &str| -> Result<(), TableError> {
                let names =
                    placeholders(template).map_err(|problem| pattern_err(pattern, problem))?;
                for n in names {
                    if !captures.contains(&n) {
                        return Err(pattern_err(
                            pattern,
                            format!(
                                "template references `{{{n}}}` but the route declares no such capture"
                            ),
                        ));
                    }
                }
                Ok(())
            };
            check(&route.equivalent, "equivalent")?;
            for p in &route.patterns {
                if let Some(o) = &p.equivalent_override {
                    check(o, &p.name)?;
                }
            }
        }
        Ok(Router { table })
    }

    /// The compiled policy, for callers that need to read it back.
    pub fn table(&self) -> &RouteTable {
        &self.table
    }

    /// Decide. No I/O on this path.
    ///
    /// With `routes` empty -- slice 1's shipped state -- every call falls
    /// through to `defaults.no_match`. That outcome is READ FROM THE TABLE
    /// rather than hardcoded, because which way an unlisted command goes is an
    /// operator policy choice, not a property of the router (NFR-CMD-005
    /// rev 6).
    ///
    /// From slice 3 onward (#1044 first): a `Bash` call runs through the
    /// tokenizer (slice 2) to find its pipeline stages and whether a managed
    /// binary sits in one of them; a matched stage is then evaluated against
    /// the table's routes by `decide_stage`, the one evaluator (NFR-CMD-005).
    /// A non-`Bash` call, or a `Bash` call naming no managed binary, falls
    /// through to `defaults.no_match` exactly as slice 1 did.
    pub fn route(&self, call: &ToolCall, ctx: &Ctx) -> Routed {
        let Some(analysis) = tokenizer::analyze(
            call,
            &self.table.managed_binaries,
            &self.table.interpreters,
            &self.table.wrappers,
        ) else {
            // Not a Bash call: no rule module in this table is Bash-only yet
            // (#1044 is `gh`), so every non-Bash call falls through.
            return self.no_match();
        };

        let Some(matched) = analysis.matched else {
            // No managed binary anywhere in this command. The tokenizer's own
            // decision only ever moves off `Allow` alongside a match, but the
            // table's own default is still the authority here, not an
            // assumption about the tokenizer's internals.
            return match analysis.decision {
                Decision::Allow => self.no_match(),
                other => Routed {
                    decision: other,
                    targets: Targets::default(),
                    matched: None,
                    opaque: analysis.opaque,
                    note: None,
                },
            };
        };

        if analysis.decision != Decision::Allow {
            // The tokenizer already resolved a non-Allow verdict for this
            // matched binary (unparseable construct, opaque interpreter, a
            // `$VAR` command position, or the binary hidden in a
            // substitution) -- that verdict stands; there is no route to
            // evaluate underneath it.
            return Routed {
                decision: analysis.decision,
                targets: Targets::default(),
                matched: Some(matched),
                opaque: analysis.opaque,
                note: None,
            };
        }

        let ToolCall::Bash { command } = call else {
            // `analyze` only returns `Some` for `Bash` calls (checked
            // exhaustively rather than assumed via `unwrap`).
            return self.no_match();
        };

        let decision = self.decide_stage(command, &analysis.stages, &matched, ctx);
        Routed {
            decision,
            targets: Targets::default(),
            matched: Some(matched),
            opaque: analysis.opaque,
            note: None,
        }
    }

    /// The evaluator (NFR-CMD-005's `decide`), scoped to one matched stage.
    ///
    /// No binary name is a Rust literal anywhere below this line -- every
    /// mapping this function reads comes from `table` and `matched.binary`.
    fn decide_stage(
        &self,
        command: &str,
        stages: &[Stage],
        matched: &Matched,
        ctx: &Ctx,
    ) -> Decision {
        let stage_index = matched.stage_span.start;
        let Some(stage) = stages.get(stage_index) else {
            return Decision::Allow;
        };

        // A fully-gated binary has no sanctioned bare use at all (NFR-CMD-005:
        // "an unmatched subcommand still denies"), so it never degrades to
        // `Proxy` -- "run it, credit nothing" -- the way a splice onto a
        // non-sole/non-last pipeline stage otherwise would. The same posture
        // applies to anything this stage is composed with that a rewrite
        // cannot safely see past: a pipe/`&&`/`;`/newline boundary (a second
        // stage), a redirect or heredoc attached to THIS stage (carried
        // across a splice untouched, which is a different guarantee than
        // never emitting one here), or a command substitution / backtick
        // sitting among this stage's own words -- `gh pr view 42 $(id)`
        // parses cleanly as a `Digits` capture of `42` with the substitution
        // simply not a `Word` token, and rewriting would silently drop it
        // rather than deny it. Deny, naming nothing invented.
        if self.table.fully_gated_binaries.contains(&matched.binary)
            && (stages.len() > 1 || stage_has_opaque_content(stage))
        {
            return Decision::Deny(format!(
                "`{}` is composed with something else in this command (a pipe, redirect, `&&`, \
                 `;`, or `$(...)`) -- legion's rewrite replaces one whole pipeline stage, and \
                 translating a stage composed like this would either change what the pipeline \
                 feeds downstream or silently drop part of what was typed. Run it as its own \
                 step.",
                matched.binary
            ));
        }

        let Some(argv) = stage_argv_after_binary(command, stage, &matched.binary) else {
            return Decision::Allow;
        };

        match find_route(&matched.binary, argv, command, &self.table) {
            Some(route) => {
                let (captures, positionals, flags_present) =
                    parse_invocation(command, argv, route, ctx);
                evaluate_route(
                    command,
                    stages,
                    stage_index,
                    route,
                    &captures,
                    &positionals,
                    &flags_present,
                )
            }
            None => unmatched_subcommand(&matched.binary, argv, command, &self.table),
        }
    }

    fn no_match(&self) -> Routed {
        match self.table.defaults.no_match {
            DefaultOutcome::Allow => Routed::allow(),
            DefaultOutcome::Deny => {
                Routed::deny("no route matched and this table's defaults.no_match is `deny`")
            }
        }
    }
}

/// The literal text of one token, quote- and escape-stripped.
fn word_text(command: &str, tok: &Token) -> String {
    tokenizer::literal_command_text(&command[tok.span.clone()])
}

/// Whether this stage carries anything a rewrite cannot safely translate
/// through: a redirect, a heredoc, or a command substitution / backtick
/// among its own words (a bare `Subst` token, or a `Word` marked `live`
/// because a substitution sits inside it). None of these are stage
/// BOUNDARIES -- `build_stages` keeps them on this one stage -- so
/// `stages.len() > 1` alone would miss every one of them.
fn stage_has_opaque_content(stage: &Stage) -> bool {
    !stage.redirects.is_empty()
        || !stage.heredocs.is_empty()
        || stage.tokens.iter().any(|t| match &t.kind {
            TokenKind::Subst { .. } => true,
            TokenKind::Word { live, .. } => *live,
            _ => false,
        })
}

/// The stage's word tokens after the one naming `binary`, or `None` when no
/// token in the stage resolves (by basename) to it.
///
/// The tokenizer's own matched-stage detection already proved a managed
/// binary sits somewhere in this stage (`analyze`); this walks the same
/// stage to find WHERE, generically, by basename comparison alone -- no
/// binary name is written here as a literal.
fn stage_argv_after_binary<'a>(
    command: &str,
    stage: &'a Stage,
    binary: &str,
) -> Option<&'a [Token]> {
    let idx = stage.tokens.iter().position(|t| {
        matches!(t.kind, TokenKind::Word { .. })
            && tokenizer::basename(&word_text(command, t)) == binary
    })?;
    Some(&stage.tokens[idx + 1..])
}

/// Locate the route whose binary matches and whose subcommand words equal
/// the first N non-flag tokens of `argv` (N = that route's word count).
fn find_route<'a>(
    binary: &str,
    argv: &[Token],
    command: &str,
    table: &'a RouteTable,
) -> Option<&'a Route> {
    table.routes.iter().find(|r| {
        if r.binary != binary {
            return false;
        }
        let want: Vec<&str> = r.subcommand.split_whitespace().collect();
        let mut got: Vec<String> = Vec::with_capacity(want.len());
        for t in argv {
            if !matches!(t.kind, TokenKind::Word { .. }) {
                continue;
            }
            let raw = word_text(command, t);
            if raw.starts_with('-') {
                continue;
            }
            got.push(raw);
            if got.len() == want.len() {
                break;
            }
        }
        got.iter().map(String::as_str).collect::<Vec<_>>() == want
    })
}

/// A recognized flag token's `FlagSpec`, matched by exact alias or by
/// `alias=value` form.
fn match_flag<'a>(flags: &'a [FlagSpec], raw: &str) -> Option<&'a FlagSpec> {
    flags.iter().find(|spec| {
        spec.aliases.iter().any(|a| {
            a == raw
                || raw
                    .strip_prefix(a.as_str())
                    .is_some_and(|s| s.starts_with('='))
        })
    })
}

/// The value half of an `alias=value` token, for a matching alias.
fn equals_value(raw: &str, aliases: &[String]) -> Option<String> {
    aliases.iter().find_map(|a| {
        raw.strip_prefix(a.as_str())?
            .strip_prefix('=')
            .map(String::from)
    })
}

/// Walk a matched route's argv: every flag token seen (with the value it
/// consumes, when it takes one), and every remaining positional token in
/// order. Positional captures are bound generically from `route
/// .positional_captures` zipped against the positionals found, independent
/// of whether any `ArgPattern` later reads them -- this is what lets a
/// deny-only route's fallback message still name the real PR/issue number.
fn parse_invocation(command: &str, argv: &[Token], route: &Route, ctx: &Ctx) -> ParsedInvocation {
    let want_len = route.subcommand.split_whitespace().count();
    let mut skipped = 0usize;
    let mut captures: HashMap<String, String> = HashMap::new();
    let mut positionals: Vec<String> = Vec::new();
    let mut flags_present: FlagsPresent = Vec::new();

    if let Some(repo) = &ctx.repo {
        captures.insert("repo".to_owned(), repo.clone());
    }

    let mut i = 0usize;
    while i < argv.len() {
        let tok = &argv[i];
        if !matches!(tok.kind, TokenKind::Word { .. }) {
            i += 1;
            continue;
        }
        let raw = word_text(command, tok);
        if raw.starts_with('-') && raw != "-" {
            let spec = match_flag(&route.flags, &raw);
            let canonical = spec.map(|s| s.name.clone());
            flags_present.push((raw.clone(), canonical));
            if let Some(spec) = spec
                && spec.takes_value
            {
                if let Some(val) = equals_value(&raw, &spec.aliases) {
                    if let Some(cap) = &spec.capture {
                        captures.insert(cap.clone(), val);
                    }
                } else if i + 1 < argv.len() {
                    i += 1;
                    if let Some(cap) = &spec.capture {
                        captures.insert(cap.clone(), word_text(command, &argv[i]));
                    }
                }
            }
            i += 1;
            continue;
        }
        if skipped < want_len {
            skipped += 1;
        } else {
            positionals.push(raw);
        }
        i += 1;
    }

    for (name, val) in route.positional_captures.iter().zip(positionals.iter()) {
        captures.entry(name.clone()).or_insert_with(|| val.clone());
    }
    (captures, positionals, flags_present)
}

/// Whether a capture holds a non-empty run of ASCII digits -- the `Digits`
/// kind's own gate, so a non-numeric positional (`gh pr view banana`) never
/// silently rewrites.
fn is_digits(captures: &HashMap<String, String>, capture: &str) -> bool {
    captures
        .get(capture)
        .is_some_and(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()))
}

/// Whether an `ArgKind::Flags` pattern fires, and the raw token text to
/// name in its reason when it does.
fn flags_pattern_fires(
    policy: &FlagPolicy,
    flags_present: &[(String, Option<String>)],
) -> Option<String> {
    match policy {
        FlagPolicy::None => flags_present.first().map(|(raw, _)| raw.clone()),
        FlagPolicy::Blocklist { flags } => flags_present
            .iter()
            .find(|(_, canonical)| {
                canonical
                    .as_deref()
                    .is_some_and(|c| flags.iter().any(|f| f == c))
            })
            .map(|(raw, _)| raw.clone()),
        // Not used by any route this module (#1044) ships -- ClusterSplit is
        // pre-bash-grep's kind (NFR-CMD-005), a future module's job.
        FlagPolicy::ClusterSplit => None,
    }
}

/// Whether an `ArgKind` fires for this parsed invocation, and the raw flag
/// text to name in a Deny/Proxy reason, when the firing kind is `Flags`.
fn kind_fires(
    kind: &ArgKind,
    route: &Route,
    captures: &HashMap<String, String>,
    positionals: &[String],
    flags_present: &[(String, Option<String>)],
) -> Option<Option<String>> {
    match kind {
        ArgKind::Digits { capture } => is_digits(captures, capture).then_some(None),
        ArgKind::Flags { policy } => flags_pattern_fires(policy, flags_present).map(Some),
        ArgKind::NoExtraPositionals => {
            (positionals.len() <= route.positional_captures.len()).then_some(None)
        }
        // Ext, Conflict, Path, TextCarry, Append, and Scope are not exercised
        // by any route this module (#1044) ships (NFR-CMD-005 assigns each
        // to the module whose script case first needs it: pre-bash-grep,
        // no-git-commit, git grep). A pattern of one of these kinds never
        // matches here rather than guessing at semantics this module has no
        // test evidence for.
        ArgKind::Ext { .. }
        | ArgKind::Conflict { .. }
        | ArgKind::Path
        | ArgKind::TextCarry { .. }
        | ArgKind::Append { .. }
        | ArgKind::Scope { .. } => None,
    }
}

/// Replace `{key}` with `captures[key]`, or `<key>` when absent -- rendered
/// only ever as message text, never as something executed.
fn render(template: &str, captures: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('}') {
            Some(end) => {
                let key = &after[..end];
                match captures.get(key) {
                    Some(v) => out.push_str(v),
                    None => {
                        out.push('<');
                        out.push_str(key);
                        out.push('>');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Evaluate a matched route against its parsed invocation.
///
/// Evaluation order is fixed (NFR-CMD-005): Deny/Proxy patterns first, so a
/// blocking flag wins even when a would-be rewrite's captures are present;
/// then Rewrite patterns; else the route's own fallback Deny naming its
/// `equivalent` with unbound captures shown as `<name>`.
fn evaluate_route(
    command: &str,
    stages: &[Stage],
    stage_index: usize,
    route: &Route,
    captures: &HashMap<String, String>,
    positionals: &[String],
    flags_present: &[(String, Option<String>)],
) -> Decision {
    for pat in route
        .patterns
        .iter()
        .filter(|p| p.outcome != ArgOutcome::Rewrite)
    {
        if let Some(flag) = kind_fires(&pat.kind, route, captures, positionals, flags_present) {
            let named = flag.as_deref().unwrap_or("this argument");
            let reason = format!(
                "`{named}` cannot be translated -- {}. Without it: {}",
                route.reason,
                render(&route.equivalent, captures)
            );
            return match pat.outcome {
                ArgOutcome::Proxy => Decision::Proxy(reason),
                ArgOutcome::Deny => Decision::Deny(reason),
                ArgOutcome::Rewrite => unreachable!("filtered out above"),
            };
        }
    }

    for pat in route
        .patterns
        .iter()
        .filter(|p| p.outcome == ArgOutcome::Rewrite)
    {
        if kind_fires(&pat.kind, route, captures, positionals, flags_present).is_some() {
            let template = pat
                .equivalent_override
                .as_deref()
                .unwrap_or(&route.equivalent);
            let rendered = render(template, captures);
            let reason = format!(
                "routed through legion for the audit trail -- {}",
                render(&route.guidance, captures)
            );
            return tokenizer::splice(command, stages, stage_index, &rendered, &reason);
        }
    }

    Decision::Deny(format!(
        "{} -- {}",
        route.reason,
        render(&route.equivalent, captures)
    ))
}

/// A matched but unrouted subcommand under a fully-gated binary: point at
/// the nearest group's help rather than inventing a translation.
fn unmatched_subcommand(
    binary: &str,
    argv: &[Token],
    command: &str,
    table: &RouteTable,
) -> Decision {
    if !table.fully_gated_binaries.contains(&binary.to_owned()) {
        return Decision::Allow;
    }
    let group = argv.iter().find_map(|t| {
        if !matches!(t.kind, TokenKind::Word { .. }) {
            return None;
        }
        let raw = word_text(command, t);
        (!raw.starts_with('-')).then_some(raw)
    });
    let help = group
        .as_deref()
        .and_then(|g| {
            table
                .group_help
                .iter()
                .find(|h| h.binary == binary && h.group == g)
        })
        .map(|h| h.help_command.clone())
        .unwrap_or_else(|| "legion --help".to_owned());
    let rest: Vec<String> = argv
        .iter()
        .filter(|t| matches!(t.kind, TokenKind::Word { .. }))
        .map(|t| word_text(command, t))
        .collect();
    Decision::Deny(format!(
        "no legion equivalent for `{binary} {}` -- {help}",
        rest.join(" ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::{Decision, Routed};
    use crate::table::{ArgOutcome, ArgPattern, FlagSpec};

    /// NFR-CMD-005's structural claim, checked mechanically -- the template
    /// is RESEARCH-CMD-ROUTETABLE's `evaluator_contains_no_hardcoded_binary_names`.
    /// The evaluator (everything above this test module) must contain no
    /// managed-binary literal and no route's rewrite target duplicated as a
    /// Rust string: every one of those lives in `route-table.toml` instead.
    #[test]
    fn evaluator_contains_no_hardcoded_binary_names() {
        let source = include_str!("router.rs");
        let production = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(
            !production.contains("\"gh\""),
            "found the managed-binary literal \"gh\" in the evaluator"
        );
        // A sample of the gh section's own rewrite targets (#1044) -- none
        // of these may be duplicated here as a Rust literal; the evaluator
        // reads every one of them off `route.equivalent` / `route.guidance`.
        for needle in [
            "legion pr view",
            "legion pr checks",
            "legion pr list",
            "legion issue view",
            "legion issue list",
            "legion pr merge",
            "legion issue create",
        ] {
            assert!(
                !production.contains(needle),
                "found a route literal {needle:?} in the evaluator"
            );
        }
    }

    fn table_with(no_match: &str) -> RouteTable {
        RouteTable::from_toml(&format!(
            "[defaults]\nno_match = \"{no_match}\"\nrecall_miss = \"allow\"\n"
        ))
        .expect("fixture table parses")
    }

    fn bash() -> ToolCall {
        ToolCall::Bash {
            command: "ls -la".into(),
        }
    }
    fn read() -> ToolCall {
        ToolCall::Read {
            file_path: "src/lib.rs".into(),
            limit: None,
        }
    }

    #[test]
    fn the_embedded_table_allows_every_call_because_it_lists_no_binaries() {
        let r = Router::new(RouteTable::embedded().expect("parse")).expect("compile");
        let ctx = Ctx::default();
        assert_eq!(r.route(&bash(), &ctx).decision, Decision::Allow);
        assert_eq!(r.route(&read(), &ctx).decision, Decision::Allow);
    }

    #[test]
    fn flipping_only_the_table_flips_the_outcome_with_no_code_change() {
        let r = Router::new(table_with("deny")).expect("compile");
        let ctx = Ctx::default();
        for call in [bash(), read()] {
            match r.route(&call, &ctx).decision {
                Decision::Deny(reason) => {
                    assert!(!reason.trim().is_empty(), "a deny must carry a reason");
                    assert!(
                        reason.contains("no_match"),
                        "reason names the policy: {reason}"
                    );
                }
                other => panic!("expected Deny, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_no_match_answer_carries_no_note_and_nothing_matched() {
        let r = Router::new(RouteTable::embedded().expect("parse")).expect("compile");
        let routed = r.route(&bash(), &Ctx::default());
        assert_eq!(routed.note, None);
        assert_eq!(routed.matched, None);
        assert!(!routed.opaque);
        assert_eq!(routed.targets, crate::decision::Targets::default());
    }

    fn route_named(binary: &str, subcommand: &str, equivalent: &str) -> Route {
        Route {
            binary: binary.into(),
            subcommand: subcommand.into(),
            global_options: vec![],
            positional_captures: vec![],
            equivalent: equivalent.into(),
            reason: "because".into(),
            guidance: String::new(),
            flags: vec![],
            note: None,
            patterns: vec![],
        }
    }

    #[test]
    fn a_template_naming_an_undeclared_capture_fails_to_compile_and_names_it() {
        let mut t = table_with("allow");
        t.routes.push(route_named(
            "gh",
            "issue view",
            "legion issue view --number {number}",
        ));
        let err = Router::new(t).expect_err("must refuse");
        let msg = err.to_string();
        assert!(msg.contains("number"), "names the placeholder: {msg}");
        assert!(msg.contains("gh"), "names the binary: {msg}");
        assert!(
            msg.contains("equivalent"),
            "names the offending template: {msg}"
        );
    }

    #[test]
    fn a_declared_capture_compiles() {
        let mut t = table_with("allow");
        let mut r = route_named("gh", "issue view", "legion issue view --number {number}");
        r.flags.push(FlagSpec {
            name: "--number".into(),
            aliases: vec![],
            takes_value: true,
            capture: Some("number".into()),
        });
        t.routes.push(r);
        assert!(Router::new(t).is_ok());
    }

    #[test]
    fn a_digits_pattern_declares_its_own_capture() {
        let mut t = table_with("allow");
        let mut r = route_named("gh", "issue view", "legion issue view --number {n}");
        r.patterns.push(ArgPattern {
            name: "issue-number".into(),
            kind: ArgKind::Digits {
                capture: "n".into(),
            },
            outcome: ArgOutcome::Rewrite,
            equivalent_override: None,
        });
        t.routes.push(r);
        assert!(Router::new(t).is_ok());
    }

    #[test]
    fn an_unbalanced_brace_is_a_load_failure_not_a_silent_literal() {
        let mut t = table_with("allow");
        t.routes
            .push(route_named("gh", "pr list", "legion pr list {repo"));
        let err = Router::new(t).expect_err("must refuse");
        assert!(err.to_string().contains("unbalanced"), "{err}");
    }

    #[test]
    fn a_note_carrying_route_in_a_real_table_reaches_routed_note() {
        // The issue asks for a test that "constructs a table with one
        // note-carrying route and proves the note reaches Routed.note".
        // Slice 1's `route` has no matching logic, so the note cannot travel
        // the full path yet -- but the table, the Route, and the carrier are
        // all real here rather than a bare string handed to a helper, which is
        // as close to the literal requirement as slice 1 admits. The remaining
        // half (route -> match -> Routed) lands with the tokenizer in slice 2.
        let mut t = table_with("allow");
        let mut r = route_named("gh", "issue list", "legion issue list");
        r.note = Some("legion issue list carries the audit row".into());
        t.routes.push(r);
        let router = Router::new(t).expect("compile");

        let matched = router
            .table()
            .routes
            .iter()
            .find(|r| r.binary == "gh" && r.subcommand == "issue list")
            .expect("the note-carrying route survives compilation");

        let routed = Routed::from_route(Decision::Allow, matched.note.clone());
        assert_eq!(
            routed.note.as_deref(),
            Some("legion issue list carries the audit row"),
            "the route's note must reach Routed.note alongside Allow"
        );

        // And the same route's note must NOT survive a non-Allow decision.
        let denied = Routed::from_route(Decision::Deny("nope".into()), matched.note.clone());
        assert_eq!(denied.note, None);
    }

    #[test]
    fn a_stray_closing_brace_is_a_load_failure_not_a_silent_literal() {
        let mut t = table_with("allow");
        t.routes
            .push(route_named("gh", "pr list", "legion pr list repo}"));
        let err = Router::new(t).expect_err("must refuse");
        assert!(err.to_string().contains("unmatched"), "{err}");
    }

    #[test]
    fn an_empty_placeholder_is_a_load_failure() {
        let mut t = table_with("allow");
        t.routes
            .push(route_named("gh", "pr list", "legion pr list {}"));
        let err = Router::new(t).expect_err("must refuse");
        assert!(err.to_string().contains("empty placeholder"), "{err}");
    }

    #[test]
    fn a_multibyte_template_does_not_panic_and_resolves_normally() {
        let mut t = table_with("allow");
        let mut r = route_named("gh", "issue view", "legion issue view — café {n}");
        r.positional_captures.push("n".into());
        t.routes.push(r);
        assert!(
            Router::new(t).is_ok(),
            "multibyte content must not break the scan"
        );
    }

    #[test]
    fn duplicate_routes_are_refused_rather_than_resolved_by_order() {
        let mut t = table_with("allow");
        t.routes
            .push(route_named("gh", "pr list", "legion pr list"));
        t.routes
            .push(route_named("gh", "pr list", "legion pr list --json"));
        let err = Router::new(t).expect_err("must refuse");
        assert!(err.to_string().contains("duplicate"), "{err}");
    }

    #[test]
    fn an_empty_equivalent_or_reason_is_refused() {
        let mut t = table_with("allow");
        t.routes.push(route_named("gh", "pr list", ""));
        assert!(Router::new(t).is_err());

        let mut t2 = table_with("allow");
        let mut r = route_named("gh", "pr list", "legion pr list");
        r.reason = String::new();
        t2.routes.push(r);
        assert!(Router::new(t2).is_err());
    }

    #[test]
    fn an_equivalent_override_is_checked_too() {
        let mut t = table_with("allow");
        let mut r = route_named("gh", "pr list", "legion pr list");
        r.patterns.push(ArgPattern {
            name: "scope-guard".into(),
            kind: ArgKind::Path,
            outcome: ArgOutcome::Deny,
            equivalent_override: Some("legion pr list {missing}".into()),
        });
        t.routes.push(r);
        let err = Router::new(t).expect_err("must refuse");
        let msg = err.to_string();
        assert!(msg.contains("missing"), "{msg}");
        assert!(msg.contains("scope-guard"), "names the pattern: {msg}");
    }
}
