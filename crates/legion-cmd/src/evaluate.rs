//! The evaluator for tools other than Bash (FR-CMD-011).
//!
//! A non-Bash tool call is matched against its kind's ordered rules in the
//! policy's `tools` section, on the call's own input fields. A Bash command
//! never reaches this module: route decides it from the four Bash lists.

use serde_json::Value;

use crate::decision::{Deciding, Decision};
use crate::policy::{Policy, Predicate, Rule, RuleOutcome, ToolKind};
use crate::{Context, Lookup};

/// One call's routing result: the decision and the entry that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldsOutcome {
    pub decision: Decision,
    pub deciding: Deciding,
}

/// Which rule governs one call, walking the kind's rules in order against the
/// call's own `tool_input`.
pub(crate) enum FieldsSelection<'a> {
    /// A rule's field predicates hold; its outcome decides the call.
    Rule(&'a Rule),
    /// A rule reads a field the call does not carry as a usable value. The
    /// walk stops here: the managed rule cannot resolve (FR-CMD-016).
    Unresolvable { rule: &'a Rule, field: String },
    /// No rule under this kind applies to the call.
    NoMatch,
}

/// The one rule-selection step, shared by [`decide_fields`] and the lookup
/// pre-pass ([`crate::lookups`]) so the two cannot disagree about which rule
/// applies.
pub(crate) fn select_fields_rule<'a>(
    policy: &'a Policy,
    kind: ToolKind,
    input: &Value,
) -> FieldsSelection<'a> {
    let Some(rules) = policy.tools.get(&kind) else {
        return FieldsSelection::NoMatch;
    };
    for rule in rules {
        match field_predicates_hold(&rule.predicates, input) {
            FieldMatch::Holds => return FieldsSelection::Rule(rule),
            FieldMatch::Fails => {}
            FieldMatch::Unresolvable { field } => {
                return FieldsSelection::Unresolvable { rule, field };
            }
        }
    }
    FieldsSelection::NoMatch
}

/// Decides one call of a tool other than Bash, matching each rule's field
/// predicates against the call's own `tool_input` in order (FR-CMD-011,
/// FR-CMD-016).
///
/// No rule matches -> allow (nothing managed applies to this call). A rule
/// reads a field the call does not carry as a usable value -> deny, naming the
/// rule and the field: the managed rule cannot resolve, and FR-CMD-016 fails
/// closed rather than guessing. A rule matches but a lookup it requires was
/// not fetched -> deny. A rule matches -> its outcome.
pub fn decide_fields(
    policy: &Policy,
    kind: ToolKind,
    input: &Value,
    ctx: &Context,
) -> FieldsOutcome {
    match select_fields_rule(policy, kind, input) {
        FieldsSelection::Rule(rule) => resolve_rule(rule, ctx),
        FieldsSelection::Unresolvable { rule, field } => {
            let tool = kind.as_str();
            rule_outcome(
                rule,
                Decision::fixed_deny(
                    format!(
                        "this {tool} call carries no usable '{field}' field, which rule '{}' reads",
                        rule.id
                    ),
                    format!("retry the {tool} call with '{field}' set"),
                ),
            )
        }
        FieldsSelection::NoMatch => FieldsOutcome {
            decision: Decision::Allow { note: None },
            deciding: Deciding::Default,
        },
    }
}

/// A matched rule's outcome, through its lookup gates (FR-CMD-016).
fn resolve_rule(rule: &Rule, ctx: &Context) -> FieldsOutcome {
    for (required, lookup, name) in [
        (rule.requires_recall, &ctx.recall, "recall"),
        (rule.requires_consult, &ctx.consult, "consult"),
    ] {
        if required && *lookup == Lookup::NotFetched {
            return rule_outcome(
                rule,
                Decision::fixed_deny(
                    format!("this call needs a {name} result that was not fetched"),
                    format!("fetch the {name} result, then retry"),
                ),
            );
        }
    }
    let decision: Decision = match &rule.outcome {
        RuleOutcome::Allow { note } => Decision::Allow { note: note.clone() },
        RuleOutcome::Rewrite { target, reason } => Decision::Rewrite {
            target: target.clone(),
            reason: reason.clone(),
        },
        RuleOutcome::Deny { reason, instead } => Decision::fixed_deny(reason, instead),
        RuleOutcome::Ask {
            question, reason, ..
        } => Decision::ask(question, reason)
            .unwrap_or_else(|_| Decision::fixed_deny("an ask rule carried no question", "")),
    };
    rule_outcome(rule, decision)
}

/// An outcome naming `rule` as the deciding entry, with the operator mark
/// unset (FR-CMD-006).
fn rule_outcome(rule: &Rule, decision: Decision) -> FieldsOutcome {
    FieldsOutcome {
        decision,
        deciding: Deciding::Rule {
            id: rule.id.clone(),
            needs_operator: false,
        },
    }
}

/// How a rule's predicates resolved against one call.
#[derive(Debug, PartialEq, Eq)]
enum FieldMatch {
    Holds,
    Fails,
    /// A predicate read `field`, and the call carries no such field, carries
    /// it as null, or carries it with a JSON type the predicate cannot read.
    Unresolvable {
        field: String,
    },
}

/// All predicates in order; the first that fails or cannot resolve decides.
fn field_predicates_hold(predicates: &[Predicate], input: &Value) -> FieldMatch {
    for predicate in predicates {
        let result = field_predicate_holds(predicate, input);
        if result != FieldMatch::Holds {
            return result;
        }
    }
    FieldMatch::Holds
}

fn field_predicate_holds(predicate: &Predicate, input: &Value) -> FieldMatch {
    // A top-level key of `tool_input`; an explicit null is treated as absent,
    // which is how the harness sends an omitted optional field.
    let value_of = |field: &str| input.get(field).filter(|v| !v.is_null());
    let string_of = |field: &str, test: &dyn Fn(&str) -> bool| match value_of(field) {
        Some(Value::String(s)) => held(test(s)),
        _ => FieldMatch::Unresolvable {
            field: field.to_string(),
        },
    };

    match predicate {
        Predicate::FieldPresent { field } => held(value_of(field).is_some()),
        Predicate::FieldAbsent { field } => held(value_of(field).is_none()),
        Predicate::FieldEquals {
            field,
            any_of,
            ignore_case,
        } => string_of(field, &|s| {
            any_of.iter().any(|want| {
                if *ignore_case {
                    s.eq_ignore_ascii_case(want)
                } else {
                    s == want
                }
            })
        }),
        Predicate::FieldContains { field, any_of } => string_of(field, &|s| {
            any_of.iter().any(|want| s.contains(want.as_str()))
        }),
        Predicate::FieldEndsWith { field, any_of } => string_of(field, &|s| {
            any_of.iter().any(|want| s.ends_with(want.as_str()))
        }),
        Predicate::FieldGreaterThan { field, value } => {
            match value_of(field).and_then(Value::as_i64) {
                Some(n) => held(n > *value),
                None => FieldMatch::Unresolvable {
                    field: field.clone(),
                },
            }
        }
    }
}

fn held(holds: bool) -> FieldMatch {
    if holds {
        FieldMatch::Holds
    } else {
        FieldMatch::Fails
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_policy;
    use serde_json::json;

    fn policy() -> Policy {
        parse_policy(
            r#"{"tools": {
            "Read": {"rules": [
                {"id": "read-big", "predicates": [
                    {"kind": "field-ends-with", "field": "file_path", "any_of": [".rs"]},
                    {"kind": "field-greater-than", "field": "limit", "value": 500}],
                 "outcome": {"kind": "deny", "reason": "too big", "instead": "limit=200"}}]},
            "Agent": {"rules": [
                {"id": "agent-explore", "predicates": [
                    {"kind": "field-equals", "field": "subagent_type", "any_of": ["explore"],
                     "ignore_case": true}],
                 "outcome": {"kind": "rewrite", "target": "legion:legion-explore",
                             "reason": "legion explores through sym"}}]},
            "WebFetch": {"rules": [
                {"id": "fetch-recall", "requires_recall": true,
                 "outcome": {"kind": "allow", "note": "recall ran"}}]}
        }}"#,
        )
        .expect("valid policy")
    }

    #[test]
    fn a_matched_rule_decides_the_call() {
        let out = decide_fields(
            &policy(),
            ToolKind::Agent,
            &json!({"subagent_type": "Explore"}),
            &Context::default(),
        );
        assert!(matches!(out.decision, Decision::Rewrite { .. }));
        assert_eq!(
            out.deciding,
            Deciding::Rule {
                id: "agent-explore".to_string(),
                needs_operator: false
            }
        );
    }

    #[test]
    fn no_matching_rule_is_the_allow_default() {
        let out = decide_fields(
            &policy(),
            ToolKind::Agent,
            &json!({"subagent_type": "rust"}),
            &Context::default(),
        );
        assert_eq!(out.decision, Decision::Allow { note: None });
        assert_eq!(out.deciding, Deciding::Default);
    }

    #[test]
    fn an_unresolvable_field_denies_naming_the_rule() {
        let out = decide_fields(
            &policy(),
            ToolKind::Read,
            &json!({"file_path": "a.rs"}),
            &Context::default(),
        );
        let Decision::Deny(details) = out.decision else {
            panic!("expected deny");
        };
        assert!(details.reason().contains("read-big"));
    }

    #[test]
    fn a_required_lookup_not_fetched_denies_and_a_fetched_one_passes() {
        let unfetched = decide_fields(
            &policy(),
            ToolKind::WebFetch,
            &json!({"url": "https://x.y"}),
            &Context::default(),
        );
        assert!(matches!(unfetched.decision, Decision::Deny(_)));
        let fetched = decide_fields(
            &policy(),
            ToolKind::WebFetch,
            &json!({"url": "https://x.y"}),
            &Context {
                recall: Lookup::Empty,
                ..Context::default()
            },
        );
        assert_eq!(
            fetched.decision,
            Decision::Allow {
                note: Some("recall ran".to_string())
            }
        );
    }
}
