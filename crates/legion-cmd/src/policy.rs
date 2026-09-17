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

use crate::decision::{Decision, ManagedTarget, ProxyReason};

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

    /// A sym job has an empty `interpreter_patterns` list, which can never
    /// match anything.
    #[error("{pointer}: sym job must have at least one interpreter pattern")]
    EmptySymPatterns { pointer: String },

    /// A tool kind's rules are shaped for the wrong kind of tool: `Bash`
    /// must use `"kind": "bash"`, and every other tool kind must use
    /// `"kind": "fields"`. Both shapes parse fine on their own, but paired
    /// with the wrong tool kind they can never be evaluated correctly, so
    /// this is rejected here rather than silently treated as ungoverned.
    #[error("{pointer}: {message}")]
    MismatchedToolRulesShape { pointer: String, message: String },

    /// A sym job names neither `interpreter_patterns` nor `invocation`, or
    /// names both. Exactly one matcher shape is required, since a job
    /// otherwise has no defined way to recognize a command as its own (or
    /// two conflicting ways to).
    #[error("{pointer}: {message}")]
    AmbiguousSymJobMatcher { pointer: String, message: String },

    /// A sym job's `invocation` matcher names an empty `binary`.
    #[error("{pointer}: sym job's invocation matcher must name a non-empty binary")]
    EmptySymJobBinary { pointer: String },

    /// A predicate uses a leaf kind (recursing through `all`/`any`/`not`)
    /// that does not belong in its context: an `Args` predicate
    /// (`arg_equals`, `arg_absent`, `operand_contains`,
    /// `target_looks_like_directory`) on a Fields rule, or a `Field`/
    /// `field_contains` predicate on a Bash family rule or a sym job's
    /// invocation matcher.
    #[error("{pointer}: {message}")]
    MismatchedPredicateKind { pointer: String, message: String },

    /// A rewrite outcome's `target` names a `{...}` placeholder other than
    /// `{repo}` (FR-CMD-008). `route` passes `target` through unchanged;
    /// `{repo}`, substituted from `Context.repo` by the adapter, is the
    /// only placeholder any downstream code fills in. Any other
    /// placeholder -- or an unterminated `{` -- would reach the agent
    /// unresolved.
    #[error(
        "{pointer}: rewrite target names an unsupported placeholder \"{{{placeholder}}}\"; only \"{{repo}}\" is supported"
    )]
    UnsupportedRewriteTargetPlaceholder {
        pointer: String,
        placeholder: String,
    },

    /// A Bash family rule's outcome is `"rewrite"` with no `"exact_args"`
    /// declaration (FR-CMD-008): the rewrite fires only when the matched
    /// invocation's arguments equal `exact_args` exactly, so a rewrite
    /// with nothing declared has no defined eligibility at all.
    #[error(
        "{pointer}: a \"rewrite\" outcome on a Bash family rule must declare \"exact_args\" (FR-CMD-008)"
    )]
    MissingRewriteExactArgs { pointer: String },

    /// A Fields rule's outcome is `"rewrite"`. No Fields rewrite semantics
    /// exist: FR-CMD-008's lossless check reads an invocation's argument
    /// list, which a Fields call's JSON input does not have, so nothing
    /// could ever judge such a rewrite lossless -- rejected outright
    /// rather than accepted with no eligibility check at all.
    #[error(
        "{pointer}: a Fields rule cannot use a \"rewrite\" outcome; no Fields rewrite semantics exist"
    )]
    RewriteUnsupportedOnFieldsRule { pointer: String },
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
}

/// One tool kind's rules (FR-CMD-011). `Bash` is organized by managed-binary
/// family, since a command string can name any binary; the rest route on
/// their own fields with one flat rule list. `binaries` names each
/// managed binary's own global options that take a separate value word
/// (e.g. `git -C <dir>`, `gh --repo <owner/repo>`), so the evaluator can
/// skip past them to find the real subcommand word instead of hard-coding
/// per-binary option knowledge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolRules {
    Bash {
        families: BTreeMap<String, Family>,
        binaries: BTreeMap<String, BinaryOptions>,
    },
    Fields {
        rules: Vec<Rule>,
    },
}

/// A managed binary's own global options that take a separate value word
/// (FR-CMD-011), read by the evaluator to find a compound binary's real
/// subcommand word (e.g. skipping `-C <dir>` in `git -C /tmp push`)
/// rather than mistaking the option's value for the subcommand.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct BinaryOptions {
    #[serde(default)]
    pub global_value_options: Vec<String>,
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
/// `ArgEquals`/`ArgAbsent`/`OperandContains`/`TargetLooksLikeDirectory`
/// read a Bash invocation's argument list; `Field`/`FieldContains` read a
/// top-level field of a non-Bash tool call's JSON input. Evaluating the
/// wrong kind of predicate against the wrong kind of input is not an
/// error -- it simply does not match, since a Bash rule and a Fields rule
/// never share a family.
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
    /// Treats the first non-flag argument as the search pattern. Matches
    /// when a later non-flag operand's final `/`-separated segment does
    /// not look like `name.extension`. A missing later operand does not
    /// match: args alone cannot tell a bare current-directory search apart
    /// from piped input or a flag like `--version`. A flag's value word
    /// can be miscounted as an operand (accepted, as with
    /// [`Predicate::OperandContains`]).
    TargetLooksLikeDirectory,
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
            Predicate::TargetLooksLikeDirectory => match input {
                MatchInput::Args(args) => target_looks_like_directory(args),
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

/// Which kind of input a predicate is allowed to read, checked once at
/// parse time rather than left to `Predicate::matches`'s silent
/// wrong-kind-never-matches behavior -- that behavior is correct at
/// evaluation time (a policy author can combine predicates freely without
/// a panic), but a predicate of the wrong kind for its rule is a policy
/// authoring mistake that should fail to parse, not silently match
/// everything (a `Not` over a wrong-kind predicate, e.g. `Not(Field {..})`
/// on a Bash rule, always evaluates to `true`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PredicateContext {
    /// A Bash family rule or a sym job's invocation matcher: may use
    /// `Always`, `ArgEquals`, `ArgAbsent`, `OperandContains`, or
    /// `TargetLooksLikeDirectory`.
    Args,
    /// A Fields rule: may use `Always`, `Field`, or `FieldContains`.
    Fields,
}

/// Validates that every leaf of `predicate` -- recursing through `All`,
/// `Any`, and `Not` -- belongs to `context`. `Always` is valid in either
/// context.
fn validate_predicate_context(
    pointer: &str,
    context: PredicateContext,
    predicate: &Predicate,
) -> Result<(), PolicyError> {
    match predicate {
        Predicate::Always => Ok(()),
        Predicate::ArgEquals(_)
        | Predicate::ArgAbsent(_)
        | Predicate::OperandContains(_)
        | Predicate::TargetLooksLikeDirectory => match context {
            PredicateContext::Args => Ok(()),
            PredicateContext::Fields => Err(PolicyError::MismatchedPredicateKind {
                pointer: pointer.to_string(),
                message: "a Fields rule may only use \"always\", \"field\", or \"field_contains\""
                    .to_string(),
            }),
        },
        Predicate::Field { .. } | Predicate::FieldContains { .. } => match context {
            PredicateContext::Fields => Ok(()),
            PredicateContext::Args => Err(PolicyError::MismatchedPredicateKind {
                pointer: pointer.to_string(),
                message: "a Bash family rule or sym job invocation matcher may only use \"always\", \"arg_equals\", \"arg_absent\", \"operand_contains\", or \"target_looks_like_directory\""
                    .to_string(),
            }),
        },
        Predicate::All(parts) | Predicate::Any(parts) => parts
            .iter()
            .try_for_each(|part| validate_predicate_context(pointer, context, part)),
        Predicate::Not(inner) => validate_predicate_context(pointer, context, inner),
    }
}

/// Implements [`Predicate::TargetLooksLikeDirectory`].
fn target_looks_like_directory(args: &[String]) -> bool {
    let mut operands = args.iter().filter(|a| !a.starts_with('-'));
    let _pattern = operands.next();
    let Some(last) = operands.next_back() else {
        return false;
    };
    !looks_like_a_file_name(last)
}

/// A path's final `/`-separated segment looks like a file name when
/// splitting it on `.` yields at least two parts with the first and last
/// both non-empty (`"foo.rs"`), not a bare dot or dotfile-shaped segment
/// (`"."`, `".."`, `".git"`) or a plain name with no dot at all (`"src"`).
fn looks_like_a_file_name(operand: &str) -> bool {
    let segment = operand.rsplit('/').next().unwrap_or(operand);
    let mut parts = segment.split('.');
    let Some(first) = parts.next() else {
        return false;
    };
    let Some(last) = parts.next_back() else {
        return false;
    };
    !first.is_empty() && !last.is_empty()
}

/// One argument-level rule (FR-CMD-011): a predicate, the lookups it
/// requires, and the [`Decision`] it yields when both are satisfied,
/// already validated at parse time through `Decision`'s own constructors
/// (see [`convert_decision`]). `needs_operator` (FR-CMD-006) is only
/// meaningful when `decision` is [`Decision::Ask`]; it is always `false`
/// otherwise.
///
/// `exact_args` is FR-CMD-008's lossless-rewrite declaration for this
/// rule: when `decision` is [`Decision::Rewrite`], `parse_policy`
/// requires it (see [`PolicyError::MissingRewriteExactArgs`]), and
/// `evaluate` rewrites only when the matched invocation's arguments equal
/// `exact_args` exactly, denying naming the target otherwise. It is
/// always `None` for every other decision kind. A Fields rule can never
/// have a `Decision::Rewrite` at all (see
/// [`PolicyError::RewriteUnsupportedOnFieldsRule`]): no Fields rewrite
/// semantics exist, since a Fields call's JSON input has no argument
/// list to judge losslessness against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub id: String,
    pub predicate: Predicate,
    pub requires: Vec<RequiredLookup>,
    pub decision: Decision,
    pub needs_operator: bool,
    pub exact_args: Option<Vec<String>>,
}

/// One job `legion sym` serves (FR-CMD-007): the sym command that job maps
/// to, and how a command's job is recognized as this one -- either a
/// resolved [`crate::Invocation`] (a visible binary the tokenizer already
/// split into args, e.g. `grep -rn foo src` at any position or depth), or
/// the raw text of an [`crate::Opaque::Interpreter`] body the tokenizer
/// cannot parse into args at all (e.g. a Python one-liner).
///
/// A sym job never rewrites: it always denies naming `sym_command`. See
/// [`crate::evaluate`]'s `find_sym_job` for why -- rewriting a sym-served
/// search needs an argument-carrying template that does not exist yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymJob {
    pub id: String,
    pub sym_command: String,
    pub matcher: SymJobMatcher,
}

/// How a [`SymJob`] recognizes a command as its job (FR-CMD-007).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymJobMatcher {
    /// Matches a resolved invocation of `binary` whose args satisfy
    /// `predicate` -- visible at any position (first, after a pipe, inside
    /// `sh -c`, ...) since the evaluator checks every invocation the
    /// tokenizer resolved, not just the first.
    Invocation {
        binary: String,
        predicate: Predicate,
    },

    /// Matches an interpreter one-liner's opaque body: all of `patterns`
    /// must appear in the body for this job to match -- the evaluator gets
    /// the "any of these shapes" behavior by declaring more than one
    /// `SymJob` for the same `sym_command`, not by treating this list as a
    /// disjunction. A body that merely reads a file (`read_text()`)
    /// without also traversing a tree (`rglob(`, `os.walk(`, ...) is not a
    /// search: requiring both tokens in one job, rather than matching on
    /// either alone, is what keeps that negative from false-positiving.
    InterpreterPatterns(Vec<String>),
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
        #[serde(default)]
        binaries: BTreeMap<String, BinaryOptions>,
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
    interpreter_patterns: Option<Vec<String>>,
    #[serde(default)]
    invocation: Option<RawSymJobInvocation>,
}

#[derive(Deserialize)]
struct RawSymJobInvocation {
    binary: String,
    #[serde(default)]
    predicate: Predicate,
}

/// Parses policy text into a [`Policy`] (FR-CMD-011). Pure: the caller
/// reads the file and passes its contents; this function performs no I/O
/// (NFR-CMD-001).
pub fn parse_policy(text: &str) -> Result<Policy, PolicyError> {
    let raw: RawPolicy = serde_json::from_str(text)?;

    let mut tools = BTreeMap::new();
    for (key, raw_rules) in raw.tools {
        let pointer = format!("/tools/{key}");
        let kind = ToolKind::from_tool_name(&key).ok_or_else(|| PolicyError::UnknownToolKind {
            pointer: pointer.clone(),
            value: key.clone(),
        })?;
        let rules = convert_tool_rules(&pointer, kind, raw_rules)?;
        tools.insert(kind, rules);
    }

    let mut sym_jobs = Vec::with_capacity(raw.sym_jobs.len());
    for (index, raw_job) in raw.sym_jobs.into_iter().enumerate() {
        let job_pointer = format!("/sym_jobs/{index}");
        if raw_job.sym_command.is_empty() {
            return Err(PolicyError::EmptySymCommand {
                pointer: format!("{job_pointer}/sym_command"),
            });
        }
        let matcher = match (raw_job.interpreter_patterns, raw_job.invocation) {
            (Some(patterns), None) => {
                if patterns.is_empty() {
                    return Err(PolicyError::EmptySymPatterns {
                        pointer: format!("{job_pointer}/interpreter_patterns"),
                    });
                }
                SymJobMatcher::InterpreterPatterns(patterns)
            }
            (None, Some(invocation)) => {
                if invocation.binary.is_empty() {
                    return Err(PolicyError::EmptySymJobBinary {
                        pointer: format!("{job_pointer}/invocation/binary"),
                    });
                }
                validate_predicate_context(
                    &format!("{job_pointer}/invocation/predicate"),
                    PredicateContext::Args,
                    &invocation.predicate,
                )?;
                SymJobMatcher::Invocation {
                    binary: invocation.binary,
                    predicate: invocation.predicate,
                }
            }
            (Some(_), Some(_)) => {
                return Err(PolicyError::AmbiguousSymJobMatcher {
                    pointer: job_pointer,
                    message:
                        "a sym job must name exactly one of \"interpreter_patterns\" or \"invocation\", found both"
                            .to_string(),
                });
            }
            (None, None) => {
                return Err(PolicyError::AmbiguousSymJobMatcher {
                    pointer: job_pointer,
                    message:
                        "a sym job must name exactly one of \"interpreter_patterns\" or \"invocation\", found neither"
                            .to_string(),
                });
            }
        };
        sym_jobs.push(SymJob {
            id: raw_job.id,
            sym_command: raw_job.sym_command,
            matcher,
        });
    }

    Ok(Policy { tools, sym_jobs })
}

/// Converts a tool kind's raw rules, rejecting a shape that does not fit
/// `kind`: `Bash` must carry `"kind": "bash"` families, and every other
/// tool kind must carry `"kind": "fields"` rules (FR-CMD-011). Both raw
/// shapes parse fine on their own; only pairing them with the wrong tool
/// kind is invalid, since `evaluate` would otherwise treat a mismatched
/// tool as ungoverned and silently allow everything routed to it.
fn convert_tool_rules(
    pointer: &str,
    kind: ToolKind,
    raw: RawToolRules,
) -> Result<ToolRules, PolicyError> {
    match (kind, raw) {
        (ToolKind::Bash, RawToolRules::Bash { families, binaries }) => {
            let mut converted = BTreeMap::new();
            for (name, raw_family) in families {
                let family_pointer = format!("{pointer}/families/{name}");
                let rules = convert_rules(
                    &format!("{family_pointer}/rules"),
                    PredicateContext::Args,
                    raw_family.rules,
                )?;
                converted.insert(name, Family { rules });
            }
            Ok(ToolRules::Bash {
                families: converted,
                binaries,
            })
        }
        (ToolKind::Bash, RawToolRules::Fields { .. }) => {
            Err(PolicyError::MismatchedToolRulesShape {
                pointer: pointer.to_string(),
                message: "Bash requires \"kind\": \"bash\", found \"fields\"".to_string(),
            })
        }
        (_, RawToolRules::Fields { rules }) => Ok(ToolRules::Fields {
            rules: convert_rules(&format!("{pointer}/rules"), PredicateContext::Fields, rules)?,
        }),
        (_, RawToolRules::Bash { .. }) => Err(PolicyError::MismatchedToolRulesShape {
            pointer: pointer.to_string(),
            message:
                "every tool kind other than Bash requires \"kind\": \"fields\", found \"bash\""
                    .to_string(),
        }),
    }
}

fn convert_rules(
    pointer: &str,
    context: PredicateContext,
    raw_rules: Vec<RawRule>,
) -> Result<Vec<Rule>, PolicyError> {
    raw_rules
        .into_iter()
        .enumerate()
        .map(|(index, raw_rule)| {
            let rule_pointer = format!("{pointer}/{index}");
            validate_predicate_context(
                &format!("{rule_pointer}/predicate"),
                context,
                &raw_rule.predicate,
            )?;
            let (decision, needs_operator, exact_args) = convert_decision(
                &format!("{rule_pointer}/outcome"),
                context,
                &raw_rule.outcome,
            )?;
            Ok(Rule {
                id: raw_rule.id,
                predicate: raw_rule.predicate,
                requires: raw_rule.requires,
                decision,
                needs_operator,
                exact_args,
            })
        })
        .collect()
}

/// Rejects a rewrite `target` naming a `{...}` placeholder other than
/// `{repo}` (FR-CMD-008): `route` passes `target` through unchanged, so
/// `{repo}` -- substituted from `Context.repo` by the adapter -- is the
/// only placeholder any downstream code fills in. An unterminated `{` is
/// rejected the same way: whatever follows it can never be a valid
/// placeholder either.
fn validate_rewrite_target(pointer: &str, target: &str) -> Result<(), PolicyError> {
    let mut rest = target;
    while let Some(open) = rest.find('{') {
        let after_open = &rest[open + 1..];
        let placeholder_and_rest = after_open.find('}').map(|close| {
            let (placeholder, after_close) = after_open.split_at(close);
            (placeholder, &after_close[1..])
        });
        let Some((placeholder, after_close)) = placeholder_and_rest else {
            return Err(PolicyError::UnsupportedRewriteTargetPlaceholder {
                pointer: pointer.to_string(),
                placeholder: after_open.to_string(),
            });
        };
        if placeholder != "repo" {
            return Err(PolicyError::UnsupportedRewriteTargetPlaceholder {
                pointer: pointer.to_string(),
                placeholder: placeholder.to_string(),
            });
        }
        rest = after_close;
    }
    Ok(())
}

/// Converts a rule's raw `outcome` JSON into a validated [`Decision`],
/// its `needs_operator` mark, and -- for a `"rewrite"` outcome -- its
/// FR-CMD-008 `exact_args` declaration. Building the `Decision` through
/// its own constructors (`Decision::deny`, `Decision::ask`,
/// `ProxyReason::try_from`) rather than re-implementing their non-empty
/// checks here means the two cannot drift apart, and nothing downstream
/// needs to `.expect()` past an invariant this function already enforced.
fn convert_decision(
    pointer: &str,
    context: PredicateContext,
    value: &Value,
) -> Result<(Decision, bool, Option<Vec<String>>), PolicyError> {
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
    // `exact_args` (FR-CMD-008) only means anything on a `rewrite`
    // outcome, the same reasoning as `needs_operator` above.
    if kind != "rewrite" && value.get("exact_args").is_some() {
        return Err(PolicyError::InvalidOutcome {
            pointer: pointer.to_string(),
            message: format!(
                "\"exact_args\" only applies to a \"rewrite\" outcome, not \"{kind}\""
            ),
        });
    }

    match kind {
        "allow" => Ok((
            Decision::Allow {
                note: field("note"),
            },
            false,
            None,
        )),
        "rewrite" => {
            // No Fields rewrite semantics exist (FR-CMD-008): a Fields
            // call's JSON input has no argument list to judge losslessness
            // against, so a "rewrite" outcome here is rejected outright
            // rather than accepted with no eligibility check at all.
            if let PredicateContext::Fields = context {
                return Err(PolicyError::RewriteUnsupportedOnFieldsRule {
                    pointer: pointer.to_string(),
                });
            }
            let target = required_field("target")?;
            let reason = required_field("reason")?;
            validate_rewrite_target(&format!("{pointer}/target"), &target)?;
            // FR-CMD-008: a rewrite fires only when the matched
            // invocation's arguments equal `exact_args` exactly.
            let exact_args = match value.get("exact_args") {
                Some(raw) => {
                    let args: Vec<String> = serde_json::from_value(raw.clone()).map_err(|e| {
                        PolicyError::InvalidOutcome {
                            pointer: format!("{pointer}/exact_args"),
                            message: format!("invalid \"exact_args\": {e}"),
                        }
                    })?;
                    args
                }
                None => {
                    return Err(PolicyError::MissingRewriteExactArgs {
                        pointer: pointer.to_string(),
                    });
                }
            };
            Ok((
                Decision::Rewrite {
                    target: ManagedTarget::new(target),
                    reason,
                },
                false,
                Some(exact_args),
            ))
        }
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
            Ok((Decision::Proxy { reason }, false, None))
        }
        "deny" => {
            let reason = field("reason").unwrap_or_default();
            let instead = field("instead").unwrap_or_default();
            let decision =
                Decision::deny(reason, instead).map_err(|_| PolicyError::InvalidOutcome {
                    pointer: pointer.to_string(),
                    message: "a \"deny\" outcome must have a non-empty \"reason\" and \"instead\""
                        .to_string(),
                })?;
            Ok((decision, false, None))
        }
        "ask" => {
            let question = field("question").unwrap_or_default();
            let reason = field("reason").unwrap_or_default();
            let decision =
                Decision::ask(question, reason).map_err(|_| PolicyError::EmptyAskRule {
                    pointer: pointer.to_string(),
                })?;
            let needs_operator = value
                .get("needs_operator")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Ok((decision, needs_operator, None))
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
            Some(ToolRules::Bash { families, .. }) => assert!(families.contains_key("git push")),
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
            Some(ToolRules::Bash { families, .. }) => {
                let rule = &families.get("gh").expect("gh family").rules[0];
                assert_eq!(
                    rule.decision,
                    Decision::Proxy {
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

    // -- Sym job without any interpreter patterns ---------------------------

    #[test]
    fn sym_job_without_interpreter_patterns_is_rejected_with_pointer() {
        let text = r#"{"sym_jobs": [{"id": "job-1", "sym_command": "legion sym etc find-content", "interpreter_patterns": []}]}"#;
        let err = parse_policy(text).unwrap_err();
        match err {
            PolicyError::EmptySymPatterns { pointer } => {
                assert_eq!(pointer, "/sym_jobs/0/interpreter_patterns");
            }
            other => panic!("expected EmptySymPatterns, got {other:?}"),
        }
    }

    #[test]
    fn sym_job_with_neither_matcher_is_rejected_with_pointer() {
        let text =
            r#"{"sym_jobs": [{"id": "job-1", "sym_command": "legion sym etc find-content"}]}"#;
        let err = parse_policy(text).unwrap_err();
        match err {
            PolicyError::AmbiguousSymJobMatcher { pointer, .. } => {
                assert_eq!(pointer, "/sym_jobs/0");
            }
            other => panic!("expected AmbiguousSymJobMatcher, got {other:?}"),
        }
    }

    #[test]
    fn sym_job_with_both_matchers_is_rejected_with_pointer() {
        let text = r#"{"sym_jobs": [{
            "id": "job-1",
            "sym_command": "legion sym etc find-content",
            "interpreter_patterns": ["rglob("],
            "invocation": {"binary": "grep"}
        }]}"#;
        let err = parse_policy(text).unwrap_err();
        match err {
            PolicyError::AmbiguousSymJobMatcher { pointer, .. } => {
                assert_eq!(pointer, "/sym_jobs/0");
            }
            other => panic!("expected AmbiguousSymJobMatcher, got {other:?}"),
        }
    }

    #[test]
    fn sym_job_invocation_matcher_with_empty_binary_is_rejected_with_pointer() {
        let text = r#"{"sym_jobs": [{
            "id": "job-1",
            "sym_command": "legion sym etc find-content",
            "invocation": {"binary": ""}
        }]}"#;
        let err = parse_policy(text).unwrap_err();
        match err {
            PolicyError::EmptySymJobBinary { pointer } => {
                assert_eq!(pointer, "/sym_jobs/0/invocation/binary");
            }
            other => panic!("expected EmptySymJobBinary, got {other:?}"),
        }
    }

    #[test]
    fn sym_job_invocation_matcher_parses() {
        let text = r#"{"sym_jobs": [{
            "id": "grep-recursive",
            "sym_command": "legion sym etc find-content",
            "invocation": {"binary": "grep", "predicate": {"operand_contains": "-r"}}
        }]}"#;
        let policy = parse_policy(text).expect("valid invocation sym job should parse");
        match &policy.sym_jobs[0].matcher {
            SymJobMatcher::Invocation { binary, predicate } => {
                assert_eq!(binary, "grep");
                assert_eq!(predicate, &Predicate::OperandContains("-r".to_string()));
            }
            other => panic!("expected Invocation matcher, got {other:?}"),
        }
    }

    // -- FR-CMD-008 applies to every rewrite, not only sym jobs: a Bash
    // family rule's "rewrite" outcome must declare "exact_args", and a
    // Fields rule's must not (it has no argument list to check). A sym
    // job never declares a rewrite at all -- see find_sym_job's doc.

    #[test]
    fn bash_family_rewrite_outcome_without_exact_args_is_rejected_with_pointer() {
        let text = r#"{"tools": {"Bash": {"kind": "bash", "families": {"gh issue": {"rules": [
            {"id": "r1", "predicate": {"arg_equals": "list"},
             "outcome": {"kind": "rewrite", "target": "legion issue list", "reason": "why"}}
        ]}}}}}"#;
        let err = parse_policy(text).unwrap_err();
        match err {
            PolicyError::MissingRewriteExactArgs { pointer } => {
                assert_eq!(pointer, "/tools/Bash/families/gh issue/rules/0/outcome");
            }
            other => panic!("expected MissingRewriteExactArgs, got {other:?}"),
        }
    }

    #[test]
    fn bash_family_rewrite_outcome_with_exact_args_parses() {
        let text = r#"{"tools": {"Bash": {"kind": "bash", "families": {"gh issue": {"rules": [
            {"id": "r1", "predicate": {"arg_equals": "list"},
             "outcome": {"kind": "rewrite", "target": "legion issue list", "reason": "why",
                         "exact_args": ["issue", "list"]}}
        ]}}}}}"#;
        let policy = parse_policy(text).expect("valid exact_args rewrite should parse");
        match policy.tools.get(&ToolKind::Bash) {
            Some(ToolRules::Bash { families, .. }) => {
                let rule = &families.get("gh issue").expect("gh issue family").rules[0];
                assert_eq!(
                    rule.exact_args.as_deref(),
                    Some(["issue".to_string(), "list".to_string()].as_slice())
                );
            }
            other => panic!("expected Bash families, got {other:?}"),
        }
    }

    #[test]
    fn bash_family_rewrite_outcome_with_empty_exact_args_parses() {
        // An empty list is not meaningless -- it declares a rewrite whose
        // invocation must carry no argument at all.
        let text = r#"{"tools": {"Bash": {"kind": "bash", "families": {"gh": {"rules": [
            {"id": "r1", "predicate": "always",
             "outcome": {"kind": "rewrite", "target": "legion gh", "reason": "why",
                         "exact_args": []}}
        ]}}}}}"#;
        let policy = parse_policy(text).expect("an empty exact_args declaration should parse");
        match policy.tools.get(&ToolKind::Bash) {
            Some(ToolRules::Bash { families, .. }) => {
                let rule = &families.get("gh").expect("gh family").rules[0];
                assert_eq!(rule.exact_args.as_deref(), Some([].as_slice()));
            }
            other => panic!("expected Bash families, got {other:?}"),
        }
    }

    #[test]
    fn fields_rule_rewrite_outcome_with_exact_args_is_rejected_with_pointer() {
        // No Fields rewrite semantics exist: the "rewrite" outcome kind
        // itself is rejected on a Fields rule, not just its exact_args.
        let text = r#"{"tools": {"Edit": {"kind": "fields", "rules": [
            {"id": "r1", "predicate": "always",
             "outcome": {"kind": "rewrite", "target": "legion edit", "reason": "why",
                         "exact_args": ["x"]}}
        ]}}}"#;
        let err = parse_policy(text).unwrap_err();
        match err {
            PolicyError::RewriteUnsupportedOnFieldsRule { pointer } => {
                assert_eq!(pointer, "/tools/Edit/rules/0/outcome");
            }
            other => panic!("expected RewriteUnsupportedOnFieldsRule, got {other:?}"),
        }
    }

    #[test]
    fn fields_rule_rewrite_outcome_without_exact_args_is_still_rejected_with_pointer() {
        let text = r#"{"tools": {"Edit": {"kind": "fields", "rules": [
            {"id": "r1", "predicate": "always",
             "outcome": {"kind": "rewrite", "target": "legion edit", "reason": "why"}}
        ]}}}"#;
        let err = parse_policy(text).unwrap_err();
        match err {
            PolicyError::RewriteUnsupportedOnFieldsRule { pointer } => {
                assert_eq!(pointer, "/tools/Edit/rules/0/outcome");
            }
            other => panic!("expected RewriteUnsupportedOnFieldsRule, got {other:?}"),
        }
    }

    // -- Rewrite target placeholders: only {repo} is supported
    // (FR-CMD-008) --------------------------------------------------------

    #[test]
    fn rewrite_target_with_the_repo_placeholder_parses() {
        let text = r#"{"tools": {"Bash": {"kind": "bash", "families": {"gh issue": {"rules": [
            {"id": "r1", "predicate": {"arg_equals": "list"},
             "outcome": {"kind": "rewrite", "target": "legion issue list --repo {repo}",
                         "reason": "why", "exact_args": ["issue", "list"]}}
        ]}}}}}"#;
        let policy = parse_policy(text).expect("the {repo} placeholder should parse");
        match policy.tools.get(&ToolKind::Bash) {
            Some(ToolRules::Bash { families, .. }) => {
                let rule = &families.get("gh issue").expect("gh issue family").rules[0];
                match &rule.decision {
                    Decision::Rewrite { target, .. } => {
                        assert_eq!(target.as_str(), "legion issue list --repo {repo}")
                    }
                    other => panic!("expected Rewrite, got {other:?}"),
                }
            }
            other => panic!("expected Bash families, got {other:?}"),
        }
    }

    #[test]
    fn rewrite_target_with_an_unsupported_placeholder_is_rejected_with_pointer() {
        let text = r#"{"tools": {"Bash": {"kind": "bash", "families": {"gh issue": {"rules": [
            {"id": "r1", "predicate": {"arg_equals": "list"},
             "outcome": {"kind": "rewrite", "target": "legion issue list --label {label}",
                         "reason": "why", "exact_args": ["issue", "list"]}}
        ]}}}}}"#;
        let err = parse_policy(text).unwrap_err();
        match err {
            PolicyError::UnsupportedRewriteTargetPlaceholder {
                pointer,
                placeholder,
            } => {
                assert_eq!(
                    pointer,
                    "/tools/Bash/families/gh issue/rules/0/outcome/target"
                );
                assert_eq!(placeholder, "label");
            }
            other => panic!("expected UnsupportedRewriteTargetPlaceholder, got {other:?}"),
        }
    }

    #[test]
    fn rewrite_target_with_an_unterminated_placeholder_is_rejected() {
        let text = r#"{"tools": {"Bash": {"kind": "bash", "families": {"gh issue": {"rules": [
            {"id": "r1", "predicate": {"arg_equals": "list"},
             "outcome": {"kind": "rewrite", "target": "legion issue list --repo {repo",
                         "reason": "why", "exact_args": ["issue", "list"]}}
        ]}}}}}"#;
        let err = parse_policy(text).unwrap_err();
        assert!(matches!(
            err,
            PolicyError::UnsupportedRewriteTargetPlaceholder { .. }
        ));
    }

    #[test]
    fn rewrite_target_with_no_placeholder_at_all_parses() {
        let text = r#"{"tools": {"Bash": {"kind": "bash", "families": {"gh issue": {"rules": [
            {"id": "r1", "predicate": {"arg_equals": "list"},
             "outcome": {"kind": "rewrite", "target": "legion issue list",
                         "reason": "why", "exact_args": ["issue", "list"]}}
        ]}}}}}"#;
        assert!(parse_policy(text).is_ok());
    }

    // -- A tool kind's rules must be shaped for that kind -------------------

    #[test]
    fn bash_tool_kind_with_fields_shaped_rules_is_rejected() {
        let text = r#"{"tools": {"Bash": {"kind": "fields", "rules": []}}}"#;
        let err = parse_policy(text).unwrap_err();
        match err {
            PolicyError::MismatchedToolRulesShape { pointer, .. } => {
                assert_eq!(pointer, "/tools/Bash");
            }
            other => panic!("expected MismatchedToolRulesShape, got {other:?}"),
        }
    }

    #[test]
    fn non_bash_tool_kind_with_bash_shaped_rules_is_rejected() {
        let text = r#"{"tools": {"Edit": {"kind": "bash", "families": {}}}}"#;
        let err = parse_policy(text).unwrap_err();
        match err {
            PolicyError::MismatchedToolRulesShape { pointer, .. } => {
                assert_eq!(pointer, "/tools/Edit");
            }
            other => panic!("expected MismatchedToolRulesShape, got {other:?}"),
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

    // -- A predicate's leaf kind must fit its context (correctness review
    // on PR #1240): a Bash/invocation predicate is Args-only, a Fields
    // predicate is Field-only, and a `Not` over a wrong-kind predicate
    // must not silently match everything.

    #[test]
    fn bash_rule_with_a_field_predicate_is_rejected_with_pointer() {
        let text = r#"{"tools": {"Bash": {"kind": "bash", "families": {"gh": {"rules": [
            {"id": "r1", "predicate": {"field": {"path": "file_path", "equals": "x"}},
             "outcome": {"kind": "allow", "note": null}}
        ]}}}}}"#;
        let err = parse_policy(text).unwrap_err();
        match err {
            PolicyError::MismatchedPredicateKind { pointer, .. } => {
                assert_eq!(pointer, "/tools/Bash/families/gh/rules/0/predicate");
            }
            other => panic!("expected MismatchedPredicateKind, got {other:?}"),
        }
    }

    #[test]
    fn fields_rule_with_an_arg_equals_predicate_is_rejected_with_pointer() {
        let text = r#"{"tools": {"Edit": {"kind": "fields", "rules": [
            {"id": "r1", "predicate": {"arg_equals": "-r"},
             "outcome": {"kind": "allow", "note": null}}
        ]}}}"#;
        let err = parse_policy(text).unwrap_err();
        match err {
            PolicyError::MismatchedPredicateKind { pointer, .. } => {
                assert_eq!(pointer, "/tools/Edit/rules/0/predicate");
            }
            other => panic!("expected MismatchedPredicateKind, got {other:?}"),
        }
    }

    #[test]
    fn not_over_a_field_predicate_on_a_bash_rule_is_rejected() {
        // Before this validation, `matches` on a wrong-kind predicate
        // always returns false, so `Not` over it always returns true --
        // a rule that silently matches every command. This must be
        // caught at parse time instead.
        let text = r#"{"tools": {"Bash": {"kind": "bash", "families": {"gh": {"rules": [
            {"id": "r1", "predicate": {"not": {"field": {"path": "x", "equals": "y"}}},
             "outcome": {"kind": "allow", "note": null}}
        ]}}}}}"#;
        let err = parse_policy(text).unwrap_err();
        assert!(matches!(err, PolicyError::MismatchedPredicateKind { .. }));
    }

    #[test]
    fn field_predicate_nested_in_all_on_a_bash_rule_is_rejected() {
        let text = r#"{"tools": {"Bash": {"kind": "bash", "families": {"gh": {"rules": [
            {"id": "r1", "predicate": {"all": [
                {"arg_equals": "pr"},
                {"field": {"path": "x", "equals": "y"}}
            ]},
             "outcome": {"kind": "allow", "note": null}}
        ]}}}}}"#;
        let err = parse_policy(text).unwrap_err();
        assert!(matches!(err, PolicyError::MismatchedPredicateKind { .. }));
    }

    #[test]
    fn sym_job_invocation_matcher_with_a_field_predicate_is_rejected_with_pointer() {
        let text = r#"{"sym_jobs": [{
            "id": "job-1",
            "sym_command": "legion sym etc find-content",
            "invocation": {"binary": "grep", "predicate": {"field": {"path": "x", "equals": "y"}}}
        }]}"#;
        let err = parse_policy(text).unwrap_err();
        match err {
            PolicyError::MismatchedPredicateKind { pointer, .. } => {
                assert_eq!(pointer, "/sym_jobs/0/invocation/predicate");
            }
            other => panic!("expected MismatchedPredicateKind, got {other:?}"),
        }
    }

    #[test]
    fn always_predicate_is_valid_in_either_context() {
        let bash_text = r#"{"tools": {"Bash": {"kind": "bash", "families": {"gh": {"rules": [
            {"id": "r1", "predicate": "always", "outcome": {"kind": "allow", "note": null}}
        ]}}}}}"#;
        parse_policy(bash_text).expect("always is valid on a Bash rule");

        let fields_text = r#"{"tools": {"Edit": {"kind": "fields", "rules": [
            {"id": "r1", "predicate": "always", "outcome": {"kind": "allow", "note": null}}
        ]}}}"#;
        parse_policy(fields_text).expect("always is valid on a Fields rule");
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

    // -- TargetLooksLikeDirectory: no path operand, or a directory-looking
    // one, matches; a file-looking last operand does not (#1227's recall
    // pass on rg/ag, which recurse by default with no operand at all).

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn target_looks_like_directory_does_not_match_with_no_second_operand() {
        // A bare pattern with no path operand cannot be told apart from
        // `rg` reading a preceding pipe or a bare `rg --version`, both of
        // which the real-corpus measurement found matching here before
        // this case was excluded.
        let p = Predicate::TargetLooksLikeDirectory;
        assert!(!p.matches(MatchInput::Args(&args(&["foo"]))));
        assert!(!p.matches(MatchInput::Args(&args(&["--version"]))));
    }

    #[test]
    fn target_looks_like_directory_matches_a_bare_dot() {
        let p = Predicate::TargetLooksLikeDirectory;
        assert!(p.matches(MatchInput::Args(&args(&["foo", "."]))));
    }

    #[test]
    fn target_looks_like_directory_matches_a_plain_directory_name() {
        let p = Predicate::TargetLooksLikeDirectory;
        assert!(p.matches(MatchInput::Args(&args(&["foo", "src"]))));
        assert!(p.matches(MatchInput::Args(&args(&["foo", "src/"]))));
        assert!(p.matches(MatchInput::Args(&args(&["foo", "packages/ui/src"]))));
    }

    #[test]
    fn target_looks_like_directory_matches_a_dotfile_shaped_segment() {
        let p = Predicate::TargetLooksLikeDirectory;
        assert!(p.matches(MatchInput::Args(&args(&["foo", ".git"]))));
        assert!(p.matches(MatchInput::Args(&args(&["foo", ".."]))));
    }

    #[test]
    fn target_looks_like_directory_does_not_match_a_file_looking_operand() {
        let p = Predicate::TargetLooksLikeDirectory;
        assert!(!p.matches(MatchInput::Args(&args(&["foo", "src/main.rs"]))));
        assert!(!p.matches(MatchInput::Args(&args(&["foo", "crates/x/src/diff.rs"]))));
    }

    #[test]
    fn target_looks_like_directory_uses_the_last_operand_when_several_are_given() {
        let p = Predicate::TargetLooksLikeDirectory;
        assert!(p.matches(MatchInput::Args(&args(&["foo", "src/main.rs", "tests"]))));
        assert!(!p.matches(MatchInput::Args(&args(&["foo", "tests", "src/main.rs"]))));
    }

    #[test]
    fn target_looks_like_directory_never_matches_json_input() {
        let p = Predicate::TargetLooksLikeDirectory;
        assert!(!p.matches(MatchInput::Json(&serde_json::json!({"file_path": "src"}))));
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
        let text = r#"{"sym_jobs": [{"id": "j", "sym_command": "legion sym etc find-content", "interpreter_patterns": ["rglob("]}]}"#;
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
