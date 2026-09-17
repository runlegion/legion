//! The pure pre-pass over a policy and a command (#1229): before the
//! adapter calls [`crate::route`], it needs to know which recall/consult
//! lookups the rule that will govern this command requires, so it can fetch
//! them within its own deadline and hand the results back through
//! [`crate::Context`] before calling `route` for the decision that counts
//! (FR-CMD-016: a required lookup `route` cannot see is a missing-lookup
//! deny, not a silent skip).
//!
//! This module answers a narrower question than `route` does -- "what would
//! the first rule that matches this command need fetched" -- using only
//! [`crate::Policy`]'s and [`crate::Predicate`]'s already-public surface. It
//! deliberately does not replicate `route`'s sym-job precedence or
//! compound-command folding: neither of those changes which lookups a
//! *matched* rule declares, and the shipped policy declares no `requires`
//! at all today, so the common case matches nothing and this returns empty.
//! It has no filesystem, network, database, or process dependency
//! (NFR-CMD-001), same as the rest of this crate -- running the lookups it
//! names is the adapter's job.

use std::collections::BTreeMap;

use crate::decision::ToolCall;
use crate::policy::{Family, MatchInput, Policy, RequiredLookup, Rule, ToolKind, ToolRules};
use crate::tokenizer::{self, Invocation};

/// One lookup a matched rule requires, with the free-text query to run it.
/// `query` is the whole Bash command string, or a Fields tool's JSON input
/// rendered as text -- the same text `route` itself would scan, so the
/// adapter never re-derives its own query shape from the command (FR-CMD-003).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredQuery {
    pub lookup: RequiredLookup,
    pub query: String,
}

/// Finds the [`RequiredQuery`]s the rule that would govern `call` under
/// `policy` declares. Empty when no rule matches, or when the matching rule
/// declares no requirement.
pub fn required_lookups(policy: &Policy, call: &ToolCall) -> Vec<RequiredQuery> {
    let Some(rule) = matching_rule(policy, call) else {
        return Vec::new();
    };
    if rule.requires.is_empty() {
        return Vec::new();
    }
    let query = query_text(call);
    rule.requires
        .iter()
        .map(|lookup| RequiredQuery {
            lookup: *lookup,
            query: query.clone(),
        })
        .collect()
}

fn matching_rule<'p>(policy: &'p Policy, call: &ToolCall) -> Option<&'p Rule> {
    if call.tool == "Bash" {
        return matching_bash_rule(policy, call);
    }
    let kind = ToolKind::from_tool_name(&call.tool)?;
    match policy.tools.get(&kind) {
        Some(ToolRules::Fields { rules }) => first_match(rules, MatchInput::Json(&call.input)),
        _ => None,
    }
}

/// Mirrors `crate::evaluate`'s verb-scoped-then-bare family lookup
/// (`"git push"` before `"git"`), so a rule this finds is the same rule
/// `route` itself would reach for the first invocation that names it.
fn matching_bash_rule<'p>(policy: &'p Policy, call: &ToolCall) -> Option<&'p Rule> {
    let families = match policy.tools.get(&ToolKind::Bash) {
        Some(ToolRules::Bash { families }) => families,
        _ => return None,
    };
    let command = call
        .input
        .get("command")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let scan = tokenizer::scan(command).ok()?;
    scan.invocations.iter().find_map(|inv| {
        let family = find_family(families, inv)?;
        first_match(&family.rules, MatchInput::Args(&inv.args))
    })
}

fn find_family<'p>(families: &'p BTreeMap<String, Family>, inv: &Invocation) -> Option<&'p Family> {
    if let Some(first) = inv.args.first()
        && !first.starts_with('-')
    {
        let two_word = format!("{} {first}", inv.binary);
        if let Some(family) = families.get(&two_word) {
            return Some(family);
        }
    }
    families.get(&inv.binary)
}

fn first_match<'p>(rules: &'p [Rule], input: MatchInput<'_>) -> Option<&'p Rule> {
    rules.iter().find(|rule| rule.predicate.matches(input))
}

fn query_text(call: &ToolCall) -> String {
    if call.tool == "Bash" {
        return call
            .input
            .get("command")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
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
