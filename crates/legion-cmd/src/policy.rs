//! The routing policy: one declarative data structure, parsed from JSON,
//! that `evaluate` reads (FR-CMD-011). Nothing here decides anything --
//! `Policy` is inert data; `evaluate::evaluate` is the only code that turns
//! it into a [`crate::Decision`].
//!
//! Parsing happens in two passes. The outer shape (tool kinds, families,
//! sym jobs) is deserialized directly with `serde`, which already rejects a
//! structurally malformed document. The one part `serde` cannot validate on
//! its own -- a rule's `outcome`, whose shape depends on which of the five
//! [`crate::Decision`] arms it names -- is kept as a raw [`serde_json::Value`]
//! through that first pass and converted in a second, explicit walk. That
//! walk is where every [`PolicyError`] below is raised, each one carrying
//! the JSON pointer of the entry that failed.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use crate::decision::ProxyReason;

/// Errors raised while parsing or validating a policy document. Each names
/// the JSON pointer of the invalid entry, so an operator editing
/// `policy.json` can find the exact line the error is about.
#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    /// The document is not valid JSON at all, or its outer shape (the
    /// `tools` map, a family's `rules` array, a sym job's fields, ...)
    /// does not match what `serde` expects.
    #[error("invalid policy JSON: {0}")]
    Malformed(#[from] serde_json::Error),

    /// `tools`' key is not one of the six known tool kinds.
    #[error("{pointer}: unknown tool kind {value:?}")]
    UnknownToolKind { pointer: String, value: String },

    /// A rule's `outcome.kind` is not one of the five closed Decision arms.
    #[error("{pointer}: unknown decision kind {value:?}")]
    UnknownDecisionKind { pointer: String, value: String },

    /// A rule's `outcome` is missing a field its `kind` requires (e.g. a
    /// `deny` with no `reason`), or a required field is an empty string.
    #[error("{pointer}: {message}")]
    InvalidOutcome { pointer: String, message: String },

    /// A `proxy` outcome's `reason` is outside the closed seven-member
    /// [`ProxyReason`] set.
    #[error("{pointer}: unknown proxy reason {value:?}")]
    UnknownProxyReason { pointer: String, value: String },

    /// An `ask` outcome has an empty `question` or `reason` (FR-CMD-006).
    #[error("{pointer}: ask rule must have a non-empty question and reason")]
    EmptyAskRule { pointer: String },

    /// A sym job has an empty `sym_command`.
    #[error("{pointer}: sym job must name a non-empty sym command")]
    EmptySymCommand { pointer: String },
}

/// The tool kinds the policy can route on (FR-CMD-011). A `Bash` call
/// carries a command string the tokenizer splits; the rest route on their
/// own fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ToolKind {
    Bash,
    Edit,
    Write,
    Read,
    Grep,
    Agent,
}

impl ToolKind {
    const ALL: [(&'static str, ToolKind); 6] = [
        ("Bash", ToolKind::Bash),
        ("Edit", ToolKind::Edit),
        ("Write", ToolKind::Write),
        ("Read", ToolKind::Read),
        ("Grep", ToolKind::Grep),
        ("Agent", ToolKind::Agent),
    ];

    /// Maps a `ToolCall.tool` name to the kind that governs it. `None` for
    /// a tool the policy has no vocabulary for at all -- such a call is
    /// simply outside the policy's concern, not a validation error.
    pub fn from_tool_name(name: &str) -> Option<ToolKind> {
        ToolKind::ALL
            .iter()
            .find(|(known, _)| *known == name)
            .map(|(_, kind)| *kind)
    }

    fn from_policy_key(key: &str) -> Option<ToolKind> {
        ToolKind::from_tool_name(key)
    }
}

/// One tool kind's rules (FR-CMD-011). `Bash` is organized by managed-binary
/// family, since a command string can name any binary; the rest route on
/// their own fields with one flat rule list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolRules {
    Bash { families: BTreeMap<String, Family> },
    Fields { rules: Vec<Rule> },
}

/// One managed-binary family's argument-level rules (FR-CMD-011). The key
/// this `Family` is stored under in [`ToolRules::Bash`] is verb-scoped: a
/// plain binary name (`"gh"`), or a binary plus its leading subcommand word
/// (`"git push"`) when only that verb is governed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Family {
    pub rules: Vec<Rule>,
}

/// A lookup a [`Rule`] requires before its outcome can be trusted
/// (FR-CMD-016): if the caller-supplied [`crate::Context`] shows it as
/// [`crate::Lookup::NotFetched`], the rule cannot resolve and the default
/// deny applies instead of the rule's own outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequiredLookup {
    Recall,
    Consult,
}

/// An argument-level (or, for a [`ToolRules::Fields`] rule, field-level)
/// predicate over the command being routed. Combinators (`All`, `Any`,
/// `Not`) let a policy author build a compound condition out of the
/// primitives without inventing a second predicate language.
///
/// `ArgEquals`/`ArgAbsent`/`OperandContains` read a Bash invocation's
/// argument list; `Field`/`FieldContains` read a top-level field of a
/// non-Bash tool call's JSON input. Evaluating the wrong kind of predicate
/// against the wrong kind of input is not an error -- it simply does not
/// match, since a Bash rule and a Fields rule never share a family.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Predicate {
    /// Matches every command in the family (or every call, for a Fields
    /// rule). Used for a catch-all rule at the end of a family's list.
    #[default]
    Always,
    /// A literal argument equal to `value` appears in the invocation --
    /// a flag (`"--force"`) or a bare word (`"push"`, `"merge"`) alike.
    ArgEquals(String),
    /// No argument equal to `value` appears in the invocation.
    ArgAbsent(String),
    /// Some argument contains `needle` as a substring.
    OperandContains(String),
    /// The named top-level JSON field, read as a string, equals `equals`.
    Field {
        path: String,
        equals: String,
    },
    /// The named top-level JSON field, read as a string, contains `contains`
    /// as a substring.
    FieldContains {
        path: String,
        contains: String,
    },
    All(Vec<Predicate>),
    Any(Vec<Predicate>),
    Not(Box<Predicate>),
}

/// What input a [`Predicate`] is evaluated against: a Bash invocation's
/// arguments, or a non-Bash tool call's JSON input.
#[derive(Debug, Clone, Copy)]
pub enum MatchInput<'a> {
    Args(&'a [String]),
    Json(&'a Value),
}

impl Predicate {
    /// Evaluates this predicate against `input`. A predicate whose kind
    /// does not match `input`'s kind (e.g. `Field` against `Args`) simply
    /// fails to match rather than erroring: a policy author can combine
    /// predicates freely and `evaluate` never panics on the combination.
    pub fn matches(&self, input: MatchInput<'_>) -> bool {
        match self {
            Predicate::Always => true,
            Predicate::ArgEquals(flag) => match input {
                MatchInput::Args(args) => args.iter().any(|a| a == flag),
                MatchInput::Json(_) => false,
            },
            Predicate::ArgAbsent(flag) => match input {
                MatchInput::Args(args) => !args.iter().any(|a| a == flag),
                MatchInput::Json(_) => false,
            },
            Predicate::OperandContains(needle) => match input {
                MatchInput::Args(args) => args.iter().any(|a| a.contains(needle.as_str())),
                MatchInput::Json(_) => false,
            },
            Predicate::Field { path, equals } => match input {
                MatchInput::Json(value) => value
                    .get(path)
                    .and_then(Value::as_str)
                    .is_some_and(|v| v == equals),
                MatchInput::Args(_) => false,
            },
            Predicate::FieldContains { path, contains } => match input {
                MatchInput::Json(value) => value
                    .get(path)
                    .and_then(Value::as_str)
                    .is_some_and(|v| v.contains(contains.as_str())),
                MatchInput::Args(_) => false,
            },
            Predicate::All(parts) => parts.iter().all(|p| p.matches(input)),
            Predicate::Any(parts) => parts.iter().any(|p| p.matches(input)),
            Predicate::Not(inner) => !inner.matches(input),
        }
    }
}

/// The outcome a matched [`Rule`] yields: one of the five closed
/// [`crate::Decision`] arms, expressed as data rather than as a
/// caller-constructed `Decision`, since some fields (a proxy reason, an
/// ask question) need validation the parser performs once at load time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleOutcome {
    Allow {
        note: Option<String>,
    },
    Rewrite {
        target: String,
        reason: String,
    },
    Proxy {
        reason: ProxyReason,
    },
    Deny {
        reason: String,
        instead: String,
    },
    Ask {
        question: String,
        reason: String,
        needs_operator: bool,
    },
}

/// One argument-level rule (FR-CMD-011): a predicate, the lookups it
/// requires, and the outcome it yields when both are satisfied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub id: String,
    pub predicate: Predicate,
    pub requires: Vec<RequiredLookup>,
    pub outcome: RuleOutcome,
}

/// One job `legion sym` serves (FR-CMD-007): the sym command that job maps
/// to, and the search-shaped patterns that mark an interpreter one-liner's
/// opaque body as that job. All of `interpreter_patterns` must appear in
/// the body for this job to match -- the evaluator gets the "any of these
/// shapes" behavior by declaring more than one `SymJob` for the same
/// `sym_command`, not by treating this list as a disjunction. A body that
/// merely reads a file (`read_text()`) without also traversing a tree
/// (`rglob(`, `os.walk(`, ...) is not a search: requiring both tokens in
/// one job, rather than matching on either alone, is what keeps that
/// negative from false-positiving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymJob {
    pub id: String,
    pub sym_command: String,
    pub interpreter_patterns: Vec<String>,
}

/// The parsed policy (FR-CMD-011): organized by tool kind, then within Bash
/// by managed-binary family, plus the sym jobs an interpreter one-liner's
/// opaque body is checked against (FR-CMD-007). An empty policy (no tools,
/// no sym jobs) is meaningful: `evaluate` denies every command until it is
/// populated (FR-CMD-016), rather than silently allowing everything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    pub tools: BTreeMap<ToolKind, ToolRules>,
    pub sym_jobs: Vec<SymJob>,
}

impl Policy {
    /// True when the policy governs nothing at all: no tool kind and no
    /// sym job. `evaluate` treats this as fail-closed (FR-CMD-016), not as
    /// "nothing to check."
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty() && self.sym_jobs.is_empty()
    }
}

// -- Raw shapes: what `serde` parses directly ------------------------------
//
// These mirror the public types above field-for-field, except that a
// rule's `outcome` is kept as a raw `Value` so the second pass can convert
// it with a JSON pointer in hand. Keeping the raw and public shapes
// separate (rather than a custom `Deserialize` impl on `Policy` itself)
// means the bulk of parsing is ordinary derived `serde`, and the only
// hand-written code is the part that genuinely needs a pointer: `outcome`.

#[derive(Deserialize)]
struct RawPolicy {
    #[serde(default)]
    tools: BTreeMap<String, RawToolRules>,
    #[serde(default)]
    sym_jobs: Vec<RawSymJob>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum RawToolRules {
    Bash {
        families: BTreeMap<String, RawFamily>,
    },
    Fields {
        rules: Vec<RawRule>,
    },
}

#[derive(Deserialize)]
struct RawFamily {
    rules: Vec<RawRule>,
}

#[derive(Deserialize)]
struct RawRule {
    id: String,
    #[serde(default)]
    predicate: Predicate,
    #[serde(default)]
    requires: Vec<RequiredLookup>,
    outcome: Value,
}

#[derive(Deserialize)]
struct RawSymJob {
    id: String,
    sym_command: String,
    #[serde(default)]
    interpreter_patterns: Vec<String>,
}

/// Parses policy text into a [`Policy`] (FR-CMD-011). Pure: the caller
/// reads the file and passes its contents; this function performs no I/O
/// (NFR-CMD-001).
pub fn parse_policy(text: &str) -> Result<Policy, PolicyError> {
    let raw: RawPolicy = serde_json::from_str(text)?;

    let mut tools = BTreeMap::new();
    for (key, raw_rules) in raw.tools {
        let pointer = format!("/tools/{key}");
        let kind = ToolKind::from_policy_key(&key).ok_or_else(|| PolicyError::UnknownToolKind {
            pointer: pointer.clone(),
            value: key.clone(),
        })?;
        let rules = convert_tool_rules(&pointer, raw_rules)?;
        tools.insert(kind, rules);
    }

    let mut sym_jobs = Vec::with_capacity(raw.sym_jobs.len());
    for (index, raw_job) in raw.sym_jobs.into_iter().enumerate() {
        let pointer = format!("/sym_jobs/{index}/sym_command");
        if raw_job.sym_command.is_empty() {
            return Err(PolicyError::EmptySymCommand { pointer });
        }
        sym_jobs.push(SymJob {
            id: raw_job.id,
            sym_command: raw_job.sym_command,
            interpreter_patterns: raw_job.interpreter_patterns,
        });
    }

    Ok(Policy { tools, sym_jobs })
}

fn convert_tool_rules(pointer: &str, raw: RawToolRules) -> Result<ToolRules, PolicyError> {
    match raw {
        RawToolRules::Bash { families } => {
            let mut converted = BTreeMap::new();
            for (name, raw_family) in families {
                let family_pointer = format!("{pointer}/families/{name}");
                let rules = convert_rules(&format!("{family_pointer}/rules"), raw_family.rules)?;
                converted.insert(name, Family { rules });
            }
            Ok(ToolRules::Bash {
                families: converted,
            })
        }
        RawToolRules::Fields { rules } => Ok(ToolRules::Fields {
            rules: convert_rules(&format!("{pointer}/rules"), rules)?,
        }),
    }
}

fn convert_rules(pointer: &str, raw_rules: Vec<RawRule>) -> Result<Vec<Rule>, PolicyError> {
    raw_rules
        .into_iter()
        .enumerate()
        .map(|(index, raw_rule)| {
            let rule_pointer = format!("{pointer}/{index}");
            let outcome = convert_outcome(&format!("{rule_pointer}/outcome"), &raw_rule.outcome)?;
            Ok(Rule {
                id: raw_rule.id,
                predicate: raw_rule.predicate,
                requires: raw_rule.requires,
                outcome,
            })
        })
        .collect()
}

fn convert_outcome(pointer: &str, value: &Value) -> Result<RuleOutcome, PolicyError> {
    let kind =
        value
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| PolicyError::InvalidOutcome {
                pointer: pointer.to_string(),
                message: "outcome must have a string \"kind\" field".to_string(),
            })?;

    let field = |name: &str| -> Option<String> {
        value.get(name).and_then(Value::as_str).map(str::to_string)
    };
    let required_field = |name: &str| -> Result<String, PolicyError> {
        field(name)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| PolicyError::InvalidOutcome {
                pointer: pointer.to_string(),
                message: format!("a \"{kind}\" outcome must have a non-empty \"{name}\""),
            })
    };

    // `needs_operator` (FR-CMD-006) only means anything on an `ask`
    // outcome: rejecting it elsewhere, rather than silently ignoring it,
    // means a policy author who typos the outcome kind on an
    // operator-gated rule finds out at parse time, not at review time.
    if kind != "ask" && value.get("needs_operator").is_some() {
        return Err(PolicyError::InvalidOutcome {
            pointer: pointer.to_string(),
            message: format!(
                "\"needs_operator\" only applies to an \"ask\" outcome, not \"{kind}\""
            ),
        });
    }

    match kind {
        "allow" => Ok(RuleOutcome::Allow {
            note: field("note"),
        }),
        "rewrite" => Ok(RuleOutcome::Rewrite {
            target: required_field("target")?,
            reason: required_field("reason")?,
        }),
        "proxy" => {
            let reason_pointer = format!("{pointer}/reason");
            let raw_reason = required_field("reason").map_err(|_| PolicyError::InvalidOutcome {
                pointer: reason_pointer.clone(),
                message: "a \"proxy\" outcome must have a non-empty \"reason\"".to_string(),
            })?;
            let reason = ProxyReason::try_from(raw_reason.as_str()).map_err(|_| {
                PolicyError::UnknownProxyReason {
                    pointer: reason_pointer,
                    value: raw_reason,
                }
            })?;
            Ok(RuleOutcome::Proxy { reason })
        }
        "deny" => Ok(RuleOutcome::Deny {
            reason: required_field("reason")?,
            instead: required_field("instead")?,
        }),
        "ask" => {
            let question = field("question").unwrap_or_default();
            let reason = field("reason").unwrap_or_default();
            if question.is_empty() || reason.is_empty() {
                return Err(PolicyError::EmptyAskRule {
                    pointer: pointer.to_string(),
                });
            }
            let needs_operator = value
                .get("needs_operator")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Ok(RuleOutcome::Ask {
                question,
                reason,
                needs_operator,
            })
        }
        other => Err(PolicyError::UnknownDecisionKind {
            pointer: pointer.to_string(),
            value: other.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- The happy path: a well-formed policy parses ----------------------

    #[test]
    fn well_formed_policy_parses() {
        let text = r#"{
            "tools": {
                "Bash": {
                    "kind": "bash",
                    "families": {
                        "git push": {
                            "rules": [
                                {
                                    "id": "git-push-force",
                                    "predicate": {"arg_equals": "--force"},
                                    "outcome": {
                                        "kind": "deny",
                                        "reason": "force-push rewrites shared history",
                                        "instead": "git push --force-with-lease"
                                    }
                                }
                            ]
                        }
                    }
                },
                "Edit": {
                    "kind": "fields",
                    "rules": [
                        {
                            "id": "edit-dotenv",
                            "predicate": {"field_contains": {"path": "file_path", "contains": ".env"}},
                            "outcome": {
                                "kind": "ask",
                                "question": "edit a secrets file?",
                                "reason": ".env holds credentials",
                                "needs_operator": true
                            }
                        }
                    ]
                }
            },
            "sym_jobs": [
                {
                    "id": "py-find-content",
                    "sym_command": "legion sym etc find-content",
                    "interpreter_patterns": ["rglob(", "read_text()"]
                }
            ]
        }"#;
        let policy = parse_policy(text).expect("well-formed policy should parse");
        assert!(!policy.is_empty());
        assert_eq!(policy.sym_jobs.len(), 1);
        match policy.tools.get(&ToolKind::Bash) {
            Some(ToolRules::Bash { families }) => assert!(families.contains_key("git push")),
            other => panic!("expected Bash families, got {other:?}"),
        }
        match policy.tools.get(&ToolKind::Edit) {
            Some(ToolRules::Fields { rules }) => assert_eq!(rules.len(), 1),
            other => panic!("expected Edit fields rules, got {other:?}"),
        }
    }

    #[test]
    fn empty_policy_parses_and_reports_empty() {
        let policy = parse_policy("{}").expect("empty policy should parse");
        assert!(policy.is_empty());
    }

    // -- Malformed JSON --------------------------------------------------

    #[test]
    fn malformed_json_is_rejected() {
        let err = parse_policy("not json").unwrap_err();
        assert!(matches!(err, PolicyError::Malformed(_)));
    }

    // -- Unknown tool kind -------------------------------------------------

    #[test]
    fn unknown_tool_kind_is_rejected_with_pointer() {
        let text = r#"{"tools": {"Frobnicate": {"kind": "fields", "rules": []}}}"#;
        let err = parse_policy(text).unwrap_err();
        match err {
            PolicyError::UnknownToolKind { pointer, value } => {
                assert_eq!(pointer, "/tools/Frobnicate");
                assert_eq!(value, "Frobnicate");
            }
            other => panic!("expected UnknownToolKind, got {other:?}"),
        }
    }

    // -- Unknown decision kind ----------------------------------------------

    #[test]
    fn unknown_decision_kind_is_rejected_with_pointer() {
        let text = r#"{"tools": {"Bash": {"kind": "bash", "families": {"gh": {"rules": [
            {"id": "r1", "outcome": {"kind": "teleport"}}
        ]}}}}}"#;
        let err = parse_policy(text).unwrap_err();
        match err {
            PolicyError::UnknownDecisionKind { pointer, value } => {
                assert_eq!(pointer, "/tools/Bash/families/gh/rules/0/outcome");
                assert_eq!(value, "teleport");
            }
            other => panic!("expected UnknownDecisionKind, got {other:?}"),
        }
    }

    // -- Unknown proxy reason ----------------------------------------------

    #[test]
    fn unknown_proxy_reason_is_rejected_with_pointer() {
        let text = r#"{"tools": {"Bash": {"kind": "bash", "families": {"gh": {"rules": [
            {"id": "r1", "outcome": {"kind": "proxy", "reason": "magic"}}
        ]}}}}}"#;
        let err = parse_policy(text).unwrap_err();
        match err {
            PolicyError::UnknownProxyReason { pointer, value } => {
                assert_eq!(pointer, "/tools/Bash/families/gh/rules/0/outcome/reason");
                assert_eq!(value, "magic");
            }
            other => panic!("expected UnknownProxyReason, got {other:?}"),
        }
    }

    #[test]
    fn valid_proxy_reason_parses() {
        let text = r#"{"tools": {"Bash": {"kind": "bash", "families": {"gh": {"rules": [
            {"id": "r1", "outcome": {"kind": "proxy", "reason": "opaque"}}
        ]}}}}}"#;
        let policy = parse_policy(text).expect("valid proxy reason should parse");
        match policy.tools.get(&ToolKind::Bash) {
            Some(ToolRules::Bash { families }) => {
                let rule = &families.get("gh").expect("gh family").rules[0];
                assert_eq!(
                    rule.outcome,
                    RuleOutcome::Proxy {
                        reason: ProxyReason::Opaque
                    }
                );
            }
            other => panic!("expected Bash families, got {other:?}"),
        }
    }

    // -- Ask rule without a question or reason -----------------------------

    #[test]
    fn ask_rule_without_question_is_rejected_with_pointer() {
        let text = r#"{"tools": {"Bash": {"kind": "bash", "families": {"gh": {"rules": [
            {"id": "r1", "outcome": {"kind": "ask", "reason": "needs a look"}}
        ]}}}}}"#;
        let err = parse_policy(text).unwrap_err();
        match err {
            PolicyError::EmptyAskRule { pointer } => {
                assert_eq!(pointer, "/tools/Bash/families/gh/rules/0/outcome");
            }
            other => panic!("expected EmptyAskRule, got {other:?}"),
        }
    }

    #[test]
    fn ask_rule_without_reason_is_rejected() {
        let text = r#"{"tools": {"Bash": {"kind": "bash", "families": {"gh": {"rules": [
            {"id": "r1", "outcome": {"kind": "ask", "question": "sure?"}}
        ]}}}}}"#;
        let err = parse_policy(text).unwrap_err();
        assert!(matches!(err, PolicyError::EmptyAskRule { .. }));
    }

    // -- Sym job without a sym command --------------------------------------

    #[test]
    fn sym_job_without_sym_command_is_rejected_with_pointer() {
        let text = r#"{"sym_jobs": [{"id": "job-1", "sym_command": ""}]}"#;
        let err = parse_policy(text).unwrap_err();
        match err {
            PolicyError::EmptySymCommand { pointer } => {
                assert_eq!(pointer, "/sym_jobs/0/sym_command");
            }
            other => panic!("expected EmptySymCommand, got {other:?}"),
        }
    }

    // -- Deny and rewrite require their fields ------------------------------

    #[test]
    fn deny_outcome_without_instead_is_rejected() {
        let text = r#"{"tools": {"Bash": {"kind": "bash", "families": {"gh": {"rules": [
            {"id": "r1", "outcome": {"kind": "deny", "reason": "no"}}
        ]}}}}}"#;
        let err = parse_policy(text).unwrap_err();
        assert!(matches!(err, PolicyError::InvalidOutcome { .. }));
    }

    #[test]
    fn needs_operator_on_a_non_ask_outcome_is_rejected() {
        // needs_operator only means anything on an ask outcome (FR-CMD-006);
        // a policy author who sets it on a deny outcome by mistake should
        // find out at parse time, not have it silently ignored.
        let text = r#"{"tools": {"Bash": {"kind": "bash", "families": {"gh": {"rules": [
            {"id": "r1", "outcome": {"kind": "deny", "reason": "no", "instead": "x", "needs_operator": true}}
        ]}}}}}"#;
        let err = parse_policy(text).unwrap_err();
        assert!(matches!(err, PolicyError::InvalidOutcome { .. }));
    }

    #[test]
    fn rewrite_outcome_without_target_is_rejected() {
        let text = r#"{"tools": {"Bash": {"kind": "bash", "families": {"gh": {"rules": [
            {"id": "r1", "outcome": {"kind": "rewrite", "reason": "use legion instead"}}
        ]}}}}}"#;
        let err = parse_policy(text).unwrap_err();
        assert!(matches!(err, PolicyError::InvalidOutcome { .. }));
    }

    // -- Predicate combinators ----------------------------------------------

    #[test]
    fn predicate_arg_equals_matches_args() {
        let p = Predicate::ArgEquals("-r".to_string());
        assert!(p.matches(MatchInput::Args(&["-r".to_string(), "foo".to_string()])));
        assert!(!p.matches(MatchInput::Args(&["foo".to_string()])));
    }

    #[test]
    fn predicate_arg_absent_matches_args() {
        let p = Predicate::ArgAbsent("-r".to_string());
        assert!(p.matches(MatchInput::Args(&["foo".to_string()])));
        assert!(!p.matches(MatchInput::Args(&["-r".to_string()])));
    }

    #[test]
    fn predicate_operand_contains_matches_substring() {
        let p = Predicate::OperandContains("foo".to_string());
        assert!(p.matches(MatchInput::Args(&["barfoobaz".to_string()])));
        assert!(!p.matches(MatchInput::Args(&["barbaz".to_string()])));
    }

    #[test]
    fn predicate_field_matches_json() {
        let p = Predicate::Field {
            path: "file_path".to_string(),
            equals: "a.rs".to_string(),
        };
        let json = serde_json::json!({"file_path": "a.rs"});
        assert!(p.matches(MatchInput::Json(&json)));
        let other = serde_json::json!({"file_path": "b.rs"});
        assert!(!p.matches(MatchInput::Json(&other)));
    }

    #[test]
    fn predicate_field_contains_matches_json() {
        let p = Predicate::FieldContains {
            path: "file_path".to_string(),
            contains: ".env".to_string(),
        };
        let json = serde_json::json!({"file_path": "config/.env.local"});
        assert!(p.matches(MatchInput::Json(&json)));
    }

    #[test]
    fn predicate_wrong_input_kind_never_matches() {
        let flag = Predicate::ArgEquals("-r".to_string());
        let json = serde_json::json!({"file_path": "-r"});
        assert!(!flag.matches(MatchInput::Json(&json)));

        let field = Predicate::Field {
            path: "x".to_string(),
            equals: "-r".to_string(),
        };
        assert!(!field.matches(MatchInput::Args(&["-r".to_string()])));
    }

    #[test]
    fn predicate_all_requires_every_part() {
        let p = Predicate::All(vec![
            Predicate::ArgEquals("-r".to_string()),
            Predicate::OperandContains("foo".to_string()),
        ]);
        assert!(p.matches(MatchInput::Args(&["-r".to_string(), "foo".to_string()])));
        assert!(!p.matches(MatchInput::Args(&["-r".to_string()])));
    }

    #[test]
    fn predicate_any_requires_one_part() {
        let p = Predicate::Any(vec![
            Predicate::ArgEquals("-r".to_string()),
            Predicate::ArgEquals("-n".to_string()),
        ]);
        assert!(p.matches(MatchInput::Args(&["-n".to_string()])));
        assert!(!p.matches(MatchInput::Args(&["-z".to_string()])));
    }

    #[test]
    fn predicate_not_inverts() {
        let p = Predicate::Not(Box::new(Predicate::ArgEquals("-r".to_string())));
        assert!(p.matches(MatchInput::Args(&["foo".to_string()])));
        assert!(!p.matches(MatchInput::Args(&["-r".to_string()])));
    }

    #[test]
    fn predicate_always_matches_anything() {
        assert!(Predicate::Always.matches(MatchInput::Args(&[])));
        assert!(Predicate::Always.matches(MatchInput::Json(&Value::Null)));
    }

    // -- Policy::is_empty ----------------------------------------------------

    #[test]
    fn policy_with_only_sym_jobs_is_not_empty() {
        let text = r#"{"sym_jobs": [{"id": "j", "sym_command": "legion sym etc find-content"}]}"#;
        let policy = parse_policy(text).expect("valid policy");
        assert!(!policy.is_empty());
    }

    #[test]
    fn tool_kind_from_tool_name_covers_all_six() {
        for name in ["Bash", "Edit", "Write", "Read", "Grep", "Agent"] {
            assert!(
                ToolKind::from_tool_name(name).is_some(),
                "{name} should map"
            );
        }
        assert!(ToolKind::from_tool_name("WebFetch").is_none());
    }
}
