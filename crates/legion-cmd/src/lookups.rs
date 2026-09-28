//! The pure lookup pre-pass (#1229, FR-CMD-016).
//!
//! A rule for a tool other than Bash may require a recall or consult result
//! before it matches ([`crate::Rule::requires_recall`],
//! [`crate::Rule::requires_consult`]), and route denies when a required
//! result is [`crate::Lookup::NotFetched`]. The adapter therefore has to know,
//! before it calls route, which lookups the rule governing this call requires
//! and what to query them with. This module answers with the same rule
//! selection route uses, so it can never name a rule route would not consult.
//! A Bash command requires none: its four lists carry no lookups. Like the
//! rest of the crate it performs no I/O (NFR-CMD-001).

use crate::decision::ToolCall;
use crate::evaluate::{FieldsSelection, select_fields_rule};
use crate::policy::{Policy, ToolKind};
use serde_json::Value;

/// The lookups the matched rule requires, each with the query text to run it
/// with. `None` means the rule does not require that lookup. Mirrors the two
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

/// The recall and consult lookups the rule governing `call` requires. The
/// query text is the tool input's string values, the same values the rule's
/// predicates were matched against.
pub fn required_lookups(policy: &Policy, call: &ToolCall) -> RequiredLookups {
    let Some(kind) = ToolKind::parse(&call.tool) else {
        return RequiredLookups::default();
    };
    let FieldsSelection::Rule(rule) = select_fields_rule(policy, kind, &call.input) else {
        return RequiredLookups::default();
    };
    let mut values: Vec<String> = Vec::new();
    collect_strings(&call.input, &mut values);
    let query: String = values.join(" ");
    RequiredLookups {
        recall: rule.requires_recall.then(|| query.clone()),
        consult: rule.requires_consult.then_some(query),
    }
}

/// Every string value in a tool's input, depth-first: the text a matched
/// rule's lookup query is built from.
fn collect_strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => out.push(s.clone()),
        Value::Array(items) => items.iter().for_each(|v| collect_strings(v, out)),
        Value::Object(map) => map.values().for_each(|v| collect_strings(v, out)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_policy;

    fn policy() -> Policy {
        parse_policy(
            r#"{"tools": {
            "WebFetch": {"rules": [{"id": "fetch", "requires_recall": true,
                                    "outcome": {"kind": "allow"}}]},
            "WebSearch": {"rules": [{"id": "search", "requires_consult": true,
                                     "outcome": {"kind": "allow"}}]}
        }}"#,
        )
        .expect("valid policy")
    }

    fn call(tool: &str, input: serde_json::Value) -> ToolCall {
        ToolCall {
            tool: tool.to_string(),
            input,
        }
    }

    #[test]
    fn a_rule_requiring_recall_names_the_input_strings_as_its_query() {
        let found = required_lookups(
            &policy(),
            &call(
                "WebFetch",
                serde_json::json!({"url": "https://a.b", "prompt": "why"}),
            ),
        );
        let query = found.recall.expect("recall required");
        assert!(query.contains("https://a.b") && query.contains("why"));
        assert!(found.consult.is_none());
    }

    #[test]
    fn a_rule_requiring_consult_names_it() {
        let found = required_lookups(
            &policy(),
            &call("WebSearch", serde_json::json!({"query": "tantivy"})),
        );
        assert_eq!(found.consult.as_deref(), Some("tantivy"));
        assert!(found.recall.is_none());
    }

    #[test]
    fn a_bash_command_requires_nothing() {
        let found = required_lookups(
            &policy(),
            &call("Bash", serde_json::json!({"command": "git status"})),
        );
        assert!(found.is_empty());
    }
}
