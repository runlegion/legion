//! The pure lookup pre-pass (#1229, FR-CMD-016).
//!
//! A rule may require a recall or consult result before it matches
//! ([`crate::Rule::requires_recall`], [`crate::Rule::requires_consult`]), and
//! route denies when a required result is [`crate::Lookup::NotFetched`]. The
//! adapter therefore has to know, before it calls route, which lookups the
//! rules governing this call require and what to query them with. This module
//! answers by calling the same expansion and the same rule selection route
//! uses, so it can never name a rule route would not consult. It runs that
//! expansion itself, before route runs it again; what it shares with route is
//! the code, not the result. The adapter never scans the command (FR-CMD-003,
//! FR-CMD-017). Like the rest of the
//! crate it performs no I/O (NFR-CMD-001): running the lookups is the
//! adapter's job.

use crate::decision::ToolCall;
use crate::evaluate::{self, BashReading, FieldsSelection, select_bash_rule, select_fields_rule};
use crate::policy::{Policy, Rule, ToolKind};
use crate::route::{bash_command, expand_command};
use serde_json::Value;

/// The lookups the matched rules require, each with the query text to run it
/// with. `None` means no matched rule requires that lookup. Mirrors the two
/// lookup fields of [`crate::Context`], which is where the adapter puts the
/// results.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequiredLookups {
    pub recall: Option<String>,
    pub consult: Option<String>,
}

impl RequiredLookups {
    /// True when no matched rule requires any lookup.
    pub fn is_empty(&self) -> bool {
        self.recall.is_none() && self.consult.is_none()
    }
}

/// The recall and consult lookups the rules governing `call` require.
///
/// For a Bash call the query text is the words of every invocation whose rule
/// requires the lookup -- the binary and its dequoted arguments, as the scan
/// resolved them, not the raw command string. A command that does not parse
/// requires nothing: route asks about it without consulting a rule. For a
/// Fields tool the query text is the tool input's string values, the same
/// values the rule's predicates were matched against.
pub fn required_lookups(policy: &Policy, call: &ToolCall) -> RequiredLookups {
    let mut recall: Vec<String> = Vec::new();
    let mut consult: Vec<String> = Vec::new();

    for (rule, query) in matched_rules(policy, call) {
        if rule.requires_recall {
            recall.push(query.clone());
        }
        if rule.requires_consult {
            consult.push(query);
        }
    }

    RequiredLookups {
        recall: join_queries(recall),
        consult: join_queries(consult),
    }
}

/// Every rule that would govern a part of `call`, paired with the text that
/// part contributes to a lookup query.
fn matched_rules<'a>(policy: &'a Policy, call: &ToolCall) -> Vec<(&'a Rule, String)> {
    if call.tool == "Bash" {
        let Ok(expanded) = expand_command(policy, bash_command(call)) else {
            return Vec::new();
        };
        let mut matched: Vec<(&Rule, String)> = Vec::new();
        for invocation in &expanded.invocations {
            let binary: &str = &invocation.binary;
            // Every reading route decides under (#1298): an inline alias's
            // expansion and the words as typed. A rule both readings select
            // is one part, queried once.
            let readings = evaluate::bash_readings(policy, binary, &invocation.args);
            let mut rules: Vec<&Rule> = Vec::new();
            for reading in &readings {
                let BashReading::Words { args, start } = reading else {
                    continue;
                };
                if let Some(rule) = select_bash_rule(policy, binary, args, *start)
                    .and_then(|selection| selection.rule)
                    && !rules.iter().any(|seen| seen.id == rule.id)
                {
                    rules.push(rule);
                }
            }
            let mut words: Vec<&str> = vec![binary];
            words.extend(invocation.args.iter().map(|a| evaluate::dequote_outer(a)));
            let query: String = words.join(" ");
            matched.extend(rules.into_iter().map(|rule| (rule, query.clone())));
        }
        return matched;
    }

    let Some(kind) = ToolKind::ALL.into_iter().find(|k| k.as_str() == call.tool) else {
        return Vec::new();
    };
    let FieldsSelection::Rule(rule) = select_fields_rule(policy, kind, &call.input) else {
        return Vec::new();
    };
    let mut values: Vec<String> = Vec::new();
    collect_strings(&call.input, &mut values);
    vec![(rule, values.join(" "))]
}

/// Every string value in a Fields tool's input, depth-first: the text a
/// matched Fields rule's lookup query is built from.
fn collect_strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => out.push(s.clone()),
        Value::Array(items) => items.iter().for_each(|v| collect_strings(v, out)),
        Value::Object(map) => map.values().for_each(|v| collect_strings(v, out)),
        _ => {}
    }
}

fn join_queries(queries: Vec<String>) -> Option<String> {
    if queries.is_empty() {
        None
    } else {
        Some(queries.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_policy;

    fn policy(text: &str) -> Policy {
        parse_policy(text).expect("valid policy")
    }

    fn bash(command: &str) -> ToolCall {
        ToolCall {
            tool: "Bash".to_string(),
            input: serde_json::json!({ "command": command }),
        }
    }

    /// `gh issue` needs recall, `gh pr` needs both, `git push` needs nothing;
    /// `env` is a wrapper.
    fn sample_policy() -> Policy {
        policy(
            r#"{
            "wrappers": [{"binary": "env"}],
            "tools": {"Bash": {"families": {
                "gh issue": {"rules": [
                    {"id": "gh-issue", "requires_recall": true, "outcome": {"kind": "allow"}}
                ]},
                "gh pr": {"rules": [
                    {"id": "gh-pr-merge", "predicates": [{"kind": "arg-present", "arg": "merge"}],
                     "requires_recall": true, "requires_consult": true,
                     "outcome": {"kind": "allow"}}
                ]},
                "git push": {"rules": [
                    {"id": "git-push", "outcome": {"kind": "allow"}}
                ]}
            }}}
        }"#,
        )
    }

    #[test]
    fn an_unmanaged_command_requires_nothing() {
        let found = required_lookups(&sample_policy(), &bash("echo hi"));
        assert!(found.is_empty());
    }

    #[test]
    fn a_matched_rule_with_no_requirement_requires_nothing() {
        let found = required_lookups(&sample_policy(), &bash("git push origin main"));
        assert_eq!(found, RequiredLookups::default());
    }

    #[test]
    fn a_rule_requiring_recall_names_the_invocation_as_its_query() {
        let found = required_lookups(&sample_policy(), &bash("gh issue view \"#12\""));
        // The query is the scan's words, dequoted -- not the raw string.
        assert_eq!(found.recall.as_deref(), Some("gh issue view #12"));
        assert!(found.consult.is_none());
    }

    #[test]
    fn an_inline_alias_requires_the_lookups_of_both_its_readings() {
        // Route decides an inline alias under the alias's reading and the
        // word as typed (#1298), so the pre-pass names the rules of both --
        // once, when both readings select the same rule.
        let p = policy(
            r#"{
            "global_options": [{"binary": "git", "value_options": ["-c"],
                "inline_alias": {"options": ["-c"], "prefix": "alias."}}],
            "tools": {"Bash": {"families": {
                "git push": {"rules": [
                    {"id": "git-push", "requires_recall": true, "outcome": {"kind": "allow"}}]},
                "git fetch": {"rules": [
                    {"id": "git-fetch", "requires_consult": true, "outcome": {"kind": "allow"}}]}
            }}}
        }"#,
        );
        let aliased = required_lookups(&p, &bash("git -c alias.p=push p"));
        assert_eq!(aliased.recall.as_deref(), Some("git -c alias.p=push p"));
        assert!(aliased.consult.is_none());
        let both = required_lookups(&p, &bash("git -c alias.fetch=push fetch"));
        assert_eq!(
            both.recall.as_deref(),
            Some("git -c alias.fetch=push fetch")
        );
        assert_eq!(
            both.consult.as_deref(),
            Some("git -c alias.fetch=push fetch")
        );
        // An alias route cannot read still has its typed reading.
        let opaque = required_lookups(&p, &bash("git -c alias.push=!x push"));
        assert_eq!(opaque.recall.as_deref(), Some("git -c alias.push=!x push"));

        let whole = policy(
            r#"{
            "global_options": [{"binary": "git", "value_options": ["-c"],
                "inline_alias": {"options": ["-c"], "prefix": "alias."}}],
            "tools": {"Bash": {"families": {"git": {"rules": [
                {"id": "git", "requires_recall": true, "outcome": {"kind": "allow"}}]}}}}
        }"#,
        );
        let once = required_lookups(&whole, &bash("git -c alias.p=push p"));
        assert_eq!(once.recall.as_deref(), Some("git -c alias.p=push p"));
    }

    #[test]
    fn a_rule_requiring_both_lookups_names_both() {
        let found = required_lookups(&sample_policy(), &bash("gh pr merge 7"));
        assert_eq!(found.recall.as_deref(), Some("gh pr merge 7"));
        assert_eq!(found.consult.as_deref(), Some("gh pr merge 7"));
    }

    #[test]
    fn a_family_whose_rules_do_not_resolve_requires_nothing() {
        // `gh pr view` matches the `gh pr` family but not the merge rule:
        // route denies it as unresolvable, so no lookup is needed.
        let found = required_lookups(&sample_policy(), &bash("gh pr view 7"));
        assert!(found.is_empty());
    }

    #[test]
    fn a_wrapped_invocation_is_seen_through_the_wrapper() {
        // The pre-pass reads route's expansion, so `env gh issue list`
        // reaches the `gh issue` rule the same way route does.
        let found = required_lookups(&sample_policy(), &bash("env gh issue list"));
        assert_eq!(found.recall.as_deref(), Some("gh issue list"));
    }

    #[test]
    fn a_compound_command_joins_the_requiring_parts() {
        let found = required_lookups(&sample_policy(), &bash("gh issue list | gh pr merge 7"));
        assert_eq!(found.recall.as_deref(), Some("gh issue list gh pr merge 7"));
        assert_eq!(found.consult.as_deref(), Some("gh pr merge 7"));
    }

    #[test]
    fn an_unparsable_command_requires_nothing() {
        // route asks about a parse error without consulting a rule.
        let found = required_lookups(&sample_policy(), &bash("gh issue 'unterminated"));
        assert!(found.is_empty());
    }

    #[test]
    fn a_fields_rule_requiring_a_lookup_uses_the_input_values_as_its_query() {
        let p = policy(
            r#"{"tools": {"Edit": {"rules": [
                {"id": "edit-env", "predicates": [{"kind": "field-equals", "field": "file_path", "any_of": [".env"]}],
                 "requires_consult": true,
                 "outcome": {"kind": "deny", "reason": "secrets", "instead": "leave it"}}
            ]}}}"#,
        );
        let call = ToolCall {
            tool: "Edit".to_string(),
            input: serde_json::json!({ "file_path": ".env", "old_string": "KEY" }),
        };
        let found = required_lookups(&p, &call);
        assert!(found.recall.is_none());
        assert_eq!(found.consult.as_deref(), Some(".env KEY"));
    }

    #[test]
    fn a_tool_the_policy_does_not_model_requires_nothing() {
        let call = ToolCall {
            tool: "WebFetch".to_string(),
            input: serde_json::json!({ "url": "https://example.com" }),
        };
        assert!(required_lookups(&sample_policy(), &call).is_empty());
    }
}
