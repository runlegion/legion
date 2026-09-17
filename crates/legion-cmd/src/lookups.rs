//! The pure pre-pass over a policy and a command (#1229): before the
//! adapter calls [`crate::route`], it needs to know which recall/consult
//! lookups the rule(s) that will govern this command require, so it can
//! fetch them within its own deadline and hand the results back through
//! [`crate::Context`] before calling `route` for the decision that counts
//! (FR-CMD-016: a required lookup `route` cannot see is a missing-lookup
//! deny, not a silent skip).
//!
//! A thin consumer of [`crate::evaluate::candidate_rules`], which exposes
//! `evaluate`'s own rule-selection step (sym-job precedence, the
//! family/global-value-option-aware subcommand resolver, everything), so
//! this module can never disagree with what `route` actually consults. It
//! has no filesystem, network, database, or process dependency
//! (NFR-CMD-001), same as the rest of this crate -- running the lookups
//! it names is the adapter's job.

use crate::decision::ToolCall;
use crate::evaluate::{bash_command, candidate_rules};
use crate::policy::{Policy, RequiredLookup};

/// One lookup a matched rule requires, with the free-text query to run it.
/// `query` is the whole Bash command string, or a Fields tool's JSON input
/// rendered as text -- the same text `route` itself would scan, so the
/// adapter never re-derives its own query shape from the command (FR-CMD-003).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredQuery {
    pub lookup: RequiredLookup,
    pub query: String,
}

/// Finds the [`RequiredQuery`]s the rule(s) that would govern `call` under
/// `policy` declare. Empty when no rule matches, or when every matching
/// rule declares no requirement (the shipped policy today).
///
/// `query` is always the whole command text (or, for a non-Bash call, the
/// whole JSON `tool_input`) -- the identical string on every
/// `RequiredQuery` this returns, regardless of which candidate rule
/// declared the requirement; there is no per-invocation query text to
/// derive. `Context` (which `route` reads) also carries exactly one
/// `Lookup` per kind (`recall`, `consult`), not one per rule, so a
/// compound command whose invocations resolve to two different rules that
/// both `require` the same kind still produces exactly one entry for
/// it -- the dedup below keeps the first declaration in scan order, which
/// changes nothing observable here since every entry's `query` is the
/// same string regardless.
pub fn required_lookups(policy: &Policy, call: &ToolCall) -> Vec<RequiredQuery> {
    let query = query_text(call);
    let mut seen_recall = false;
    let mut seen_consult = false;
    let mut found = Vec::new();

    for rule in candidate_rules(policy, call) {
        for lookup in &rule.requires {
            let seen = match lookup {
                RequiredLookup::Recall => &mut seen_recall,
                RequiredLookup::Consult => &mut seen_consult,
            };
            if *seen {
                continue;
            }
            *seen = true;
            found.push(RequiredQuery {
                lookup: *lookup,
                query: query.clone(),
            });
        }
    }
    found
}

fn query_text(call: &ToolCall) -> String {
    if call.tool == "Bash" {
        return bash_command(call).to_string();
    }
    call.input.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::parse_policy;

    fn bash_call(command: &str) -> ToolCall {
        ToolCall {
            tool: "Bash".to_string(),
            input: serde_json::json!({"command": command}),
        }
    }

    #[test]
    fn empty_policy_requires_nothing() {
        let policy = parse_policy("{}").expect("empty policy parses");
        assert!(required_lookups(&policy, &bash_call("git push")).is_empty());
    }

    #[test]
    fn shipped_style_rule_with_no_requires_returns_empty() {
        let policy = parse_policy(
            r#"{"tools": {"Bash": {"kind": "bash", "families": {"chmod": {"rules": [
                {"id": "r1", "predicate": {"operand_contains": "777"},
                 "outcome": {"kind": "deny", "reason": "no", "instead": "chmod 755"}}
            ]}}}}}"#,
        )
        .expect("valid policy");
        assert!(required_lookups(&policy, &bash_call("chmod 777 x")).is_empty());
    }

    #[test]
    fn a_rule_requiring_recall_names_it_with_the_command_as_query() {
        let policy = parse_policy(
            r#"{"tools": {"Bash": {"kind": "bash", "families": {"gh issue": {"rules": [
                {"id": "gh-issue-close", "predicate": {"arg_equals": "close"},
                 "requires": ["recall"],
                 "outcome": {"kind": "deny", "reason": "needs recall", "instead": "legion issue close"}}
            ]}}}}}"#,
        )
        .expect("valid policy");
        let found = required_lookups(&policy, &bash_call("gh issue close 42"));
        assert_eq!(
            found,
            vec![RequiredQuery {
                lookup: RequiredLookup::Recall,
                query: "gh issue close 42".to_string(),
            }]
        );
    }

    #[test]
    fn a_rule_requiring_both_lookups_names_both() {
        let policy = parse_policy(
            r#"{"tools": {"Bash": {"kind": "bash", "families": {"gh issue": {"rules": [
                {"id": "gh-issue-close", "predicate": {"arg_equals": "close"},
                 "requires": ["recall", "consult"],
                 "outcome": {"kind": "deny", "reason": "needs both", "instead": "legion issue close"}}
            ]}}}}}"#,
        )
        .expect("valid policy");
        let found = required_lookups(&policy, &bash_call("gh issue close 42"));
        assert_eq!(found.len(), 2);
        assert!(found.iter().any(|q| q.lookup == RequiredLookup::Recall));
        assert!(found.iter().any(|q| q.lookup == RequiredLookup::Consult));
    }

    #[test]
    fn a_non_matching_command_requires_nothing() {
        let policy = parse_policy(
            r#"{"tools": {"Bash": {"kind": "bash", "families": {"gh issue": {"rules": [
                {"id": "gh-issue-close", "predicate": {"arg_equals": "close"},
                 "requires": ["recall"],
                 "outcome": {"kind": "deny", "reason": "needs recall", "instead": "legion issue close"}}
            ]}}}}}"#,
        )
        .expect("valid policy");
        assert!(required_lookups(&policy, &bash_call("gh issue list")).is_empty());
    }

    #[test]
    fn an_unparsable_command_requires_nothing() {
        let policy = parse_policy(
            r#"{"tools": {"Bash": {"kind": "bash", "families": {"gh issue": {"rules": [
                {"id": "gh-issue-close", "predicate": "always",
                 "requires": ["recall"],
                 "outcome": {"kind": "deny", "reason": "needs recall", "instead": "legion issue close"}}
            ]}}}}}"#,
        )
        .expect("valid policy");
        // An unbalanced quote is a tokenizer::ScanError; the pre-pass has
        // nothing to name, the same way `route` itself would return Ask
        // rather than consult a rule at all.
        assert!(required_lookups(&policy, &bash_call("echo '")).is_empty());
    }

    #[test]
    fn a_sym_job_match_requires_nothing_even_if_a_family_rule_would_have() {
        // A sym job matching takes precedence over every family rule
        // (evaluate_bash returns before evaluate_invocation ever runs), so
        // the pre-pass must not report a lookup a rule that never gets
        // consulted would have required.
        let policy = parse_policy(
            r#"{"tools": {"Bash": {"kind": "bash", "families": {"grep": {"rules": [
                {"id": "grep-rule", "predicate": "always", "requires": ["recall"],
                 "outcome": {"kind": "deny", "reason": "no", "instead": "none"}}
            ]}}}},
            "sym_jobs": [
                {"id": "sym-grep", "sym_command": "legion sym etc find-content",
                 "invocation": {"binary": "grep", "predicate": "always"}}
            ]}"#,
        )
        .expect("valid policy");
        assert!(required_lookups(&policy, &bash_call("grep -rn foo src")).is_empty());
    }

    #[test]
    fn a_fields_tool_rule_requiring_a_lookup_uses_json_input_as_query() {
        let policy = parse_policy(
            r#"{"tools": {"Edit": {"kind": "fields", "rules": [
                {"id": "edit-secrets", "predicate": {"field_contains": {"path": "file_path", "contains": ".env"}},
                 "requires": ["recall"],
                 "outcome": {"kind": "deny", "reason": "no", "instead": "none"}}
            ]}}}"#,
        )
        .expect("valid policy");
        let call = ToolCall {
            tool: "Edit".to_string(),
            input: serde_json::json!({"file_path": ".env"}),
        };
        let found = required_lookups(&policy, &call);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].lookup, RequiredLookup::Recall);
        assert_eq!(found[0].query, call.input.to_string());
    }
}
