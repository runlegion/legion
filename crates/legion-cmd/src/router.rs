//! The evaluator. The ONLY place routing logic lives (NFR-CMD-005).
//!
//! Purity is the contract (FR-CMD-001): `route` spawns no process, opens no
//! file, touches no database, and reads no environment variable. Everything it
//! knows arrives in `Ctx`, which the caller populated. That is what makes the
//! router testable without a fixture repo and what keeps a hook's latency
//! budget honest.

use std::collections::HashSet;

use crate::call::ToolCall;
use crate::ctx::Ctx;
use crate::decision::Routed;
use crate::table::{ArgKind, DefaultOutcome, Route, RouteTable, TableError};

/// A compiled, ready-to-evaluate policy.
#[derive(Debug)]
pub struct Router {
    table: RouteTable,
}

/// Every `{placeholder}` in `s`, or an error on an unbalanced brace.
///
/// Hand-scanned rather than regex: the whole crate is meant to have no regex
/// engine until a pattern kind actually needs one, and a brace scan is not
/// worth a dependency.
fn placeholders(s: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let close = after
            .find('}')
            .ok_or_else(|| format!("unbalanced '{{' in template `{s}`"))?;
        let name = &after[..close];
        if name.is_empty() {
            return Err(format!("empty placeholder `{{}}` in template `{s}`"));
        }
        if name.contains('{') {
            return Err(format!("nested '{{' in template `{s}`"));
        }
        out.push(name.to_owned());
        rest = &after[close + 1..];
    }
    Ok(out)
}

/// Every capture name a route declares.
fn declared_captures(route: &Route) -> HashSet<String> {
    let mut names: HashSet<String> = route.positional_captures.iter().cloned().collect();
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
    pub fn route(&self, _call: &ToolCall, _ctx: &Ctx) -> Routed {
        // Slice 1 ships no matching: `routes` is empty, so there is nothing to
        // match against. Slices 2 and 3 add the tokenizer and the populated
        // rows, and the match arm lands with them.
        self.no_match()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::Decision;
    use crate::table::{ArgOutcome, ArgPattern, FlagSpec};

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
