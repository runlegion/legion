//! The routing policy as one declarative data structure (FR-CMD-011).
//!
//! Every name the splitter refuses to hold -- a managed binary, a wrapper, an
//! interpreter, a shell, a JS package runner -- lives here as data, never as a
//! branch in Rust code. [`parse_policy`] reads the JSON the adapter loads and
//! builds a [`Policy`]; the evaluator ([`crate::evaluate`]) walks it, and
//! [`crate::route`] drives the whole thing. No routing decision turns on a
//! `match` over a binary name anywhere outside the policy data.
//!
//! # sh -c is an interpreter whose body is shell
//!
//! FR-CMD-007 groups `sh -c` and `python3 -c` together as names that "carry an
//! interpreter body", yet it also requires that "a managed binary reached
//! through an inline wrapper (e.g. env, sh -c '<inline>') is routed
//! identically" to first position. Those two statements are only jointly
//! satisfiable if `sh -c`'s body -- which is shell -- is re-entered through
//! [`crate::splitter::scan_at`], while `python3 -c`'s body -- which is not
//! shell -- stays opaque. So an [`Interpreter`] carries a [`BodyLanguage`]:
//! `Shell` bodies are re-entered (a managed binary inside is routed alike),
//! `Foreign` bodies are recorded opaque and matched against the sym-job
//! patterns. The field shape is the builder's to choose (per the issue), and
//! this is the shape that makes both of FR-CMD-007's statements literally true.

use std::collections::{BTreeMap, HashMap};

use serde_json::Value;

use crate::decision::ProxyReason;

/// The tool a set of rules governs. The policy is organized by tool kind first
/// (FR-CMD-011), then, within Bash, by managed-binary family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ToolKind {
    Bash,
    Edit,
    Write,
    Read,
    Grep,
    Agent,
    Glob,
    MultiEdit,
    Task,
    WebFetch,
    WebSearch,
}

impl ToolKind {
    /// Every member, so the parser rejects a wire name outside the set the
    /// same way [`ProxyReason`] does.
    pub const ALL: [ToolKind; 11] = [
        ToolKind::Bash,
        ToolKind::Edit,
        ToolKind::Write,
        ToolKind::Read,
        ToolKind::Grep,
        ToolKind::Agent,
        ToolKind::Glob,
        ToolKind::MultiEdit,
        ToolKind::Task,
        ToolKind::WebFetch,
        ToolKind::WebSearch,
    ];

    /// The tool kind's wire name, as it appears as a key under `tools`.
    pub fn as_str(self) -> &'static str {
        match self {
            ToolKind::Bash => "Bash",
            ToolKind::Edit => "Edit",
            ToolKind::Write => "Write",
            ToolKind::Read => "Read",
            ToolKind::Grep => "Grep",
            ToolKind::Agent => "Agent",
            ToolKind::Glob => "Glob",
            ToolKind::MultiEdit => "MultiEdit",
            ToolKind::Task => "Task",
            ToolKind::WebFetch => "WebFetch",
            ToolKind::WebSearch => "WebSearch",
        }
    }

    fn parse(raw: &str) -> Option<ToolKind> {
        ToolKind::ALL.into_iter().find(|k| k.as_str() == raw)
    }
}

/// The rules for one tool kind. Bash routes by managed-binary family; every
/// other tool routes on its own input fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolRules {
    /// Bash: keyed by managed binary, e.g. `"gh"`, `"git push"`. The key's
    /// first whitespace-separated token is the binary; any further tokens are
    /// leading operands (subcommand words) the invocation must carry.
    Bash { families: BTreeMap<String, Family> },
    /// Every other tool kind: an ordered rule list matched against the tool
    /// call's own input fields (`file_path`, `pattern`, `subagent_type`, ...).
    Fields { rules: Vec<Rule> },
}

/// One managed-binary family and its argument-level rules (FR-CMD-011).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Family {
    pub rules: Vec<Rule>,
}

/// One argument-level rule: predicates that must all hold, the lookups it
/// requires, and the action it yields when matched. Data only -- no closures,
/// no per-family code (FR-CMD-011).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// Unique across the whole policy (the ledger records it; #1237 keys
    /// confirmations by it).
    pub id: String,
    /// All must hold for the rule to match.
    pub predicates: Vec<Predicate>,
    /// The rule matches only when `ctx.recall` is not [`crate::Lookup::NotFetched`].
    pub requires_recall: bool,
    /// The rule matches only when `ctx.consult` is not [`crate::Lookup::NotFetched`].
    pub requires_consult: bool,
    pub outcome: RuleOutcome,
}

/// A rule predicate. The `Arg*` arms read a Bash invocation's arguments; the
/// `Field*` arms read one top-level key of a Fields tool's `tool_input`. The
/// parser scopes them: an arg predicate under a Fields tool, or a field
/// predicate under Bash, is a policy error rather than a rule that silently
/// never fires -- the same fail-closed posture as an unknown field.
///
/// A field predicate that READS a value (`FieldEquals`, `FieldContains`,
/// `FieldEndsWith`, `FieldGreaterThan`) is unresolvable when the call carries
/// no such field, or carries it with the wrong JSON type; the evaluator turns
/// that into the FR-CMD-016 deny. `FieldPresent`/`FieldAbsent` only test
/// presence and are always resolvable, so a rule that must fire when a field
/// is legitimately missing (a Read with no `limit`, an Agent with no
/// `subagent_type`) is written with those and ordered before the reading
/// rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Predicate {
    /// Some argument equals this word.
    ArgPresent(String),
    /// No argument equals this word.
    ArgAbsent(String),
    /// The field exists and is not null.
    FieldPresent { field: String },
    /// The field is missing or null.
    FieldAbsent { field: String },
    /// The string field equals one of the listed values.
    FieldEquals {
        field: String,
        any_of: Vec<String>,
        ignore_case: bool,
    },
    /// The string field contains one of the listed substrings.
    FieldContains { field: String, any_of: Vec<String> },
    /// The string field ends with one of the listed suffixes.
    FieldEndsWith { field: String, any_of: Vec<String> },
    /// The integer field is strictly greater than `value`.
    FieldGreaterThan { field: String, value: i64 },
}

impl Predicate {
    /// Whether this predicate reads a Bash invocation's arguments (true) or a
    /// Fields tool's input (false). The parser uses it to scope predicates to
    /// the tool kind they can resolve against.
    pub fn reads_args(&self) -> bool {
        matches!(self, Predicate::ArgPresent(_) | Predicate::ArgAbsent(_))
    }
}

/// What a matched rule yields.
///
/// The first five variants are exactly the five Decision arms (FR-CMD-001).
/// [`RuleOutcome::Sym`] is not a sixth Decision: it is a routing action that
/// resolves to a Decision via the referenced [`SymJob`] -- a deny naming the
/// sym command in this issue, a rewrite when lossless once #1228 lands. It is
/// carried here, rather than as a plain deny, so the evaluator can give a sym
/// job precedence over the strictest-order fold (FR-CMD-007).
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
        /// Whether the command needs the operator after the agent confirms
        /// (FR-CMD-006). route never copies this into its output in this
        /// issue; #1237 adds the path that reads it.
        needs_operator: bool,
    },
    /// Route this command to the named sym job.
    Sym {
        job: String,
    },
}

/// A job legion sym serves (searching files for content, finding a definition)
/// (FR-CMD-007): the sym command it maps to, and the search-shaped patterns
/// that mark an interpreter one-liner as this job. The patterns are a
/// conjunction -- every pattern must be a substring of the interpreter body
/// for the job to match -- so a job that needs both a traversal and a read is
/// not tripped by a body that only reads. The disjunction across shapes comes
/// from listing several [`SymJob`] entries, most specific first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymJob {
    pub id: String,
    pub sym_command: String,
    pub interpreter_patterns: Vec<String>,
}

/// A name the policy says wraps a shell command (FR-CMD-007). route re-enters
/// the wrapped command through [`crate::splitter::scan_at`], so a managed
/// binary inside is routed like the same binary in first position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wrapper {
    pub binary: String,
    /// A word that must follow the binary for it to wrap a command, e.g.
    /// `exec`/`dlx` for `pnpm`/`npm`/`yarn`. A bare `pnpm <name>` carries no
    /// required word and is an ordinary invocation, not a runner.
    pub required_subcommand: Option<String>,
}

/// Whether an interpreter body is shell (re-entered) or a foreign language
/// (recorded opaque). See the module docs for why `sh -c` and `python3 -c`
/// differ here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyLanguage {
    Shell,
    Foreign,
}

/// A name that carries an interpreter body in one of its arguments
/// (FR-CMD-007), e.g. `sh -c '<body>'`, `python3 -c '<body>'`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interpreter {
    pub binary: String,
    /// The flag whose following argument carries the body, e.g. `-c`.
    pub flag: String,
    pub body: BodyLanguage,
}

/// A name that takes a script file rather than an inline payload (FR-CMD-007),
/// e.g. `bash script.sh`. The named file's body is opaque.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptCarrier {
    pub binary: String,
}

/// The parsed policy (FR-CMD-011).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Policy {
    pub tools: BTreeMap<ToolKind, ToolRules>,
    pub sym_jobs: Vec<SymJob>,
    pub wrappers: Vec<Wrapper>,
    pub interpreters: Vec<Interpreter>,
    pub script_carriers: Vec<ScriptCarrier>,
}

impl Policy {
    /// A policy is non-empty only when at least one rule exists under some
    /// tool kind (FR-CMD-016). Sym jobs, wrappers, interpreters and script
    /// carriers on their own do not make a policy non-empty: they route
    /// nothing by themselves.
    pub fn is_empty(&self) -> bool {
        !self.tools.values().any(|rules| match rules {
            ToolRules::Bash { families } => families.values().any(|f| !f.rules.is_empty()),
            ToolRules::Fields { rules } => !rules.is_empty(),
        })
    }

    /// The wrapper matching `binary` given its arguments, if the policy names
    /// one. A wrapper with a required subcommand matches only when that word
    /// is the first argument.
    pub fn matching_wrapper(&self, binary: &str, args: &[String]) -> Option<&Wrapper> {
        self.wrappers.iter().find(|w| {
            w.binary == binary
                && match &w.required_subcommand {
                    None => true,
                    Some(word) => args.first().map(String::as_str) == Some(word.as_str()),
                }
        })
    }

    /// The interpreter matching `binary`, if the policy names one.
    pub fn matching_interpreter(&self, binary: &str) -> Option<&Interpreter> {
        self.interpreters.iter().find(|i| i.binary == binary)
    }

    /// The script carrier matching `binary`, if the policy names one.
    pub fn matching_script_carrier(&self, binary: &str) -> Option<&ScriptCarrier> {
        self.script_carriers.iter().find(|s| s.binary == binary)
    }

    /// The sym job with this id, if any.
    pub fn sym_job(&self, id: &str) -> Option<&SymJob> {
        self.sym_jobs.iter().find(|j| j.id == id)
    }
}

/// Every failure `parse_policy` can raise (FR-CMD-011 Error Handling). Each
/// carries the JSON pointer of the entry that failed -- not just the first,
/// and not a bare message -- so an operator editing the policy is told exactly
/// where the error is.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PolicyError {
    #[error("policy is not valid JSON: {detail}")]
    Json { detail: String },

    #[error("{pointer}: expected {expected}")]
    WrongType { pointer: String, expected: String },

    #[error("{pointer}: unknown field '{field}'")]
    UnknownField { pointer: String, field: String },

    #[error("{pointer}: missing field '{field}'")]
    MissingField { pointer: String, field: String },

    #[error("{pointer}: unknown tool kind '{kind}'")]
    UnknownToolKind { pointer: String, kind: String },

    #[error("{pointer}: unknown decision arm '{arm}'")]
    UnknownDecision { pointer: String, arm: String },

    #[error("{pointer}: unknown predicate kind '{kind}'")]
    UnknownPredicateKind { pointer: String, kind: String },

    #[error("{pointer}: predicate kind '{kind}' does not apply to tool kind '{tool}'")]
    PredicateNotApplicable {
        pointer: String,
        kind: String,
        tool: String,
    },

    #[error("{pointer}: unknown proxy reason '{reason}'")]
    UnknownProxyReason { pointer: String, reason: String },

    #[error("{pointer}: unknown body language '{language}'")]
    UnknownBodyLanguage { pointer: String, language: String },

    #[error("{pointer}: ask rule must carry a non-empty question and reason")]
    IncompleteAsk { pointer: String },

    #[error("{pointer}: deny rule must carry a non-empty reason and instead")]
    IncompleteDeny { pointer: String },

    #[error("{pointer}: rewrite rule must carry a non-empty target and reason")]
    IncompleteRewrite { pointer: String },

    #[error("{pointer}: sym job must carry a non-empty sym command")]
    SymJobMissingCommand { pointer: String },

    #[error("{pointer}: sym action references unknown sym job '{job}'")]
    UnknownSymJobRef { pointer: String, job: String },

    #[error("{pointer}: rule id '{id}' is empty")]
    EmptyRuleId { pointer: String, id: String },

    #[error("{pointer}: sym job id is empty")]
    EmptySymJobId { pointer: String },

    #[error("{pointer}: duplicate rule id '{id}', first defined at {first}")]
    DuplicateRuleId {
        pointer: String,
        id: String,
        first: String,
    },
}

/// Parses policy text into a [`Policy`] (FR-CMD-011). Pure: the caller reads
/// the file and passes its contents; route never opens it.
///
/// Every entry is validated. An unknown field anywhere is an error rather than
/// silently ignored, because a typo that drops a rule's lookup requirement
/// would otherwise fail open. Rule ids are unique across the whole policy.
pub fn parse_policy(text: &str) -> Result<Policy, PolicyError> {
    let root: Value = serde_json::from_str(text).map_err(|e| PolicyError::Json {
        detail: e.to_string(),
    })?;
    let root = as_object(&root, "")?;
    check_known_keys(
        root,
        "",
        &[
            "tools",
            "sym_jobs",
            "wrappers",
            "interpreters",
            "script_carriers",
        ],
    )?;

    // Ids are unique across the whole policy -- rule ids and sym-job ids share
    // one namespace, because the ledger records the id and #1237 keys
    // confirmations by it, and `Deciding::Rule.id` carries either kind.
    let mut seen_ids: HashMap<String, String> = HashMap::new();

    // Sym jobs first, so an action's sym-job reference can be validated.
    let sym_jobs = match root.get("sym_jobs") {
        Some(value) => parse_sym_jobs(value, "/sym_jobs", &mut seen_ids)?,
        None => Vec::new(),
    };
    let sym_job_ids: Vec<&str> = sym_jobs.iter().map(|j| j.id.as_str()).collect();

    let wrappers = match root.get("wrappers") {
        Some(value) => parse_wrappers(value, "/wrappers")?,
        None => Vec::new(),
    };
    let interpreters = match root.get("interpreters") {
        Some(value) => parse_interpreters(value, "/interpreters")?,
        None => Vec::new(),
    };
    let script_carriers = match root.get("script_carriers") {
        Some(value) => parse_script_carriers(value, "/script_carriers")?,
        None => Vec::new(),
    };

    let tools = match root.get("tools") {
        Some(value) => parse_tools(value, "/tools", &sym_job_ids, &mut seen_ids)?,
        None => BTreeMap::new(),
    };

    Ok(Policy {
        tools,
        sym_jobs,
        wrappers,
        interpreters,
        script_carriers,
    })
}

// -- JSON walking helpers -----------------------------------------------------
//
// Every helper takes the pointer of the value it is handed, so the error it
// raises names the exact entry, not a summary. A child's pointer is its
// parent's pointer plus `/<key>` or `/<index>`.

fn as_object<'a>(
    value: &'a Value,
    pointer: &str,
) -> Result<&'a serde_json::Map<String, Value>, PolicyError> {
    value.as_object().ok_or_else(|| PolicyError::WrongType {
        pointer: root_pointer(pointer),
        expected: "an object".to_string(),
    })
}

fn as_array<'a>(value: &'a Value, pointer: &str) -> Result<&'a Vec<Value>, PolicyError> {
    value.as_array().ok_or_else(|| PolicyError::WrongType {
        pointer: pointer.to_string(),
        expected: "an array".to_string(),
    })
}

fn as_string(value: &Value, pointer: &str) -> Result<String, PolicyError> {
    value
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| PolicyError::WrongType {
            pointer: pointer.to_string(),
            expected: "a string".to_string(),
        })
}

fn as_bool(value: &Value, pointer: &str) -> Result<bool, PolicyError> {
    value.as_bool().ok_or_else(|| PolicyError::WrongType {
        pointer: pointer.to_string(),
        expected: "a boolean".to_string(),
    })
}

/// The root object's pointer is the empty string, which reads badly in a
/// message; show it as `<root>` there but keep it exact for every child.
fn root_pointer(pointer: &str) -> String {
    if pointer.is_empty() {
        "<root>".to_string()
    } else {
        pointer.to_string()
    }
}

fn child_pointer(pointer: &str, key: &str) -> String {
    format!("{pointer}/{key}")
}

fn require<'a>(
    map: &'a serde_json::Map<String, Value>,
    key: &str,
    pointer: &str,
) -> Result<&'a Value, PolicyError> {
    map.get(key).ok_or_else(|| PolicyError::MissingField {
        pointer: root_pointer(pointer),
        field: key.to_string(),
    })
}

fn require_string(
    map: &serde_json::Map<String, Value>,
    key: &str,
    pointer: &str,
) -> Result<String, PolicyError> {
    as_string(require(map, key, pointer)?, &child_pointer(pointer, key))
}

fn optional_bool(
    map: &serde_json::Map<String, Value>,
    key: &str,
    pointer: &str,
) -> Result<bool, PolicyError> {
    match map.get(key) {
        Some(value) => as_bool(value, &child_pointer(pointer, key)),
        None => Ok(false),
    }
}

fn check_known_keys(
    map: &serde_json::Map<String, Value>,
    pointer: &str,
    known: &[&str],
) -> Result<(), PolicyError> {
    for key in map.keys() {
        if !known.contains(&key.as_str()) {
            return Err(PolicyError::UnknownField {
                pointer: child_pointer(pointer, key),
                field: key.clone(),
            });
        }
    }
    Ok(())
}

// -- tools --------------------------------------------------------------------

fn parse_tools(
    value: &Value,
    pointer: &str,
    sym_job_ids: &[&str],
    seen_ids: &mut HashMap<String, String>,
) -> Result<BTreeMap<ToolKind, ToolRules>, PolicyError> {
    let map = as_object(value, pointer)?;
    let mut tools = BTreeMap::new();
    for (key, tool_value) in map {
        let tool_pointer = child_pointer(pointer, key);
        let kind = ToolKind::parse(key).ok_or_else(|| PolicyError::UnknownToolKind {
            pointer: tool_pointer.clone(),
            kind: key.clone(),
        })?;
        let rules = parse_tool_rules(kind, tool_value, &tool_pointer, sym_job_ids, seen_ids)?;
        tools.insert(kind, rules);
    }
    Ok(tools)
}

fn parse_tool_rules(
    kind: ToolKind,
    value: &Value,
    pointer: &str,
    sym_job_ids: &[&str],
    seen_ids: &mut HashMap<String, String>,
) -> Result<ToolRules, PolicyError> {
    let map = as_object(value, pointer)?;
    match kind {
        ToolKind::Bash => {
            check_known_keys(map, pointer, &["families"])?;
            let families = match map.get("families") {
                Some(families_value) => parse_families(
                    families_value,
                    &child_pointer(pointer, "families"),
                    sym_job_ids,
                    seen_ids,
                )?,
                None => BTreeMap::new(),
            };
            Ok(ToolRules::Bash { families })
        }
        _ => {
            check_known_keys(map, pointer, &["rules"])?;
            let rules = match map.get("rules") {
                Some(rules_value) => parse_rules(
                    rules_value,
                    &child_pointer(pointer, "rules"),
                    kind,
                    sym_job_ids,
                    seen_ids,
                )?,
                None => Vec::new(),
            };
            Ok(ToolRules::Fields { rules })
        }
    }
}

fn parse_families(
    value: &Value,
    pointer: &str,
    sym_job_ids: &[&str],
    seen_ids: &mut HashMap<String, String>,
) -> Result<BTreeMap<String, Family>, PolicyError> {
    let map = as_object(value, pointer)?;
    let mut families = BTreeMap::new();
    for (key, family_value) in map {
        let family_pointer = child_pointer(pointer, key);
        let family_map = as_object(family_value, &family_pointer)?;
        check_known_keys(family_map, &family_pointer, &["rules"])?;
        let rules = match family_map.get("rules") {
            Some(rules_value) => parse_rules(
                rules_value,
                &child_pointer(&family_pointer, "rules"),
                ToolKind::Bash,
                sym_job_ids,
                seen_ids,
            )?,
            None => Vec::new(),
        };
        families.insert(key.clone(), Family { rules });
    }
    Ok(families)
}

fn parse_rules(
    value: &Value,
    pointer: &str,
    kind: ToolKind,
    sym_job_ids: &[&str],
    seen_ids: &mut HashMap<String, String>,
) -> Result<Vec<Rule>, PolicyError> {
    let array = as_array(value, pointer)?;
    let mut rules = Vec::with_capacity(array.len());
    for (index, rule_value) in array.iter().enumerate() {
        let rule_pointer = child_pointer(pointer, &index.to_string());
        rules.push(parse_rule(
            rule_value,
            &rule_pointer,
            kind,
            sym_job_ids,
            seen_ids,
        )?);
    }
    Ok(rules)
}

fn parse_rule(
    value: &Value,
    pointer: &str,
    kind: ToolKind,
    sym_job_ids: &[&str],
    seen_ids: &mut HashMap<String, String>,
) -> Result<Rule, PolicyError> {
    let map = as_object(value, pointer)?;
    check_known_keys(
        map,
        pointer,
        &[
            "id",
            "predicates",
            "requires_recall",
            "requires_consult",
            "outcome",
        ],
    )?;

    let id = require_string(map, "id", pointer)?;
    if id.is_empty() {
        return Err(PolicyError::EmptyRuleId {
            pointer: child_pointer(pointer, "id"),
            id,
        });
    }
    register_id(seen_ids, &id, &child_pointer(pointer, "id"))?;

    let predicates = match map.get("predicates") {
        Some(preds) => parse_predicates(preds, &child_pointer(pointer, "predicates"), kind)?,
        None => Vec::new(),
    };
    let requires_recall = optional_bool(map, "requires_recall", pointer)?;
    let requires_consult = optional_bool(map, "requires_consult", pointer)?;
    let outcome = parse_outcome(
        require(map, "outcome", pointer)?,
        &child_pointer(pointer, "outcome"),
        sym_job_ids,
    )?;

    Ok(Rule {
        id,
        predicates,
        requires_recall,
        requires_consult,
        outcome,
    })
}

/// Registers an id in the policy-wide id set, rejecting a duplicate. Rule ids
/// and sym-job ids share this one namespace.
fn register_id(
    seen_ids: &mut HashMap<String, String>,
    id: &str,
    pointer: &str,
) -> Result<(), PolicyError> {
    if let Some(first) = seen_ids.get(id) {
        return Err(PolicyError::DuplicateRuleId {
            pointer: pointer.to_string(),
            id: id.to_string(),
            first: first.clone(),
        });
    }
    seen_ids.insert(id.to_string(), pointer.to_string());
    Ok(())
}

fn parse_predicates(
    value: &Value,
    pointer: &str,
    tool: ToolKind,
) -> Result<Vec<Predicate>, PolicyError> {
    let array = as_array(value, pointer)?;
    let mut predicates = Vec::with_capacity(array.len());
    for (index, predicate_value) in array.iter().enumerate() {
        let predicate_pointer = child_pointer(pointer, &index.to_string());
        predicates.push(parse_predicate(predicate_value, &predicate_pointer, tool)?);
    }
    Ok(predicates)
}

fn parse_predicate(value: &Value, pointer: &str, tool: ToolKind) -> Result<Predicate, PolicyError> {
    let map = as_object(value, pointer)?;
    let kind = require_string(map, "kind", pointer)?;
    let predicate = match kind.as_str() {
        "arg-present" | "arg-absent" => {
            check_known_keys(map, pointer, &["kind", "arg"])?;
            let arg = require_string(map, "arg", pointer)?;
            if kind == "arg-present" {
                Predicate::ArgPresent(arg)
            } else {
                Predicate::ArgAbsent(arg)
            }
        }
        "field-present" | "field-absent" => {
            check_known_keys(map, pointer, &["kind", "field"])?;
            let field = require_string(map, "field", pointer)?;
            if kind == "field-present" {
                Predicate::FieldPresent { field }
            } else {
                Predicate::FieldAbsent { field }
            }
        }
        "field-equals" => {
            check_known_keys(map, pointer, &["kind", "field", "any_of", "ignore_case"])?;
            Predicate::FieldEquals {
                field: require_string(map, "field", pointer)?,
                any_of: parse_any_of(map, pointer)?,
                ignore_case: optional_bool(map, "ignore_case", pointer)?,
            }
        }
        "field-contains" => {
            check_known_keys(map, pointer, &["kind", "field", "any_of"])?;
            Predicate::FieldContains {
                field: require_string(map, "field", pointer)?,
                any_of: parse_any_of(map, pointer)?,
            }
        }
        "field-ends-with" => {
            check_known_keys(map, pointer, &["kind", "field", "any_of"])?;
            Predicate::FieldEndsWith {
                field: require_string(map, "field", pointer)?,
                any_of: parse_any_of(map, pointer)?,
            }
        }
        "field-greater-than" => {
            check_known_keys(map, pointer, &["kind", "field", "value"])?;
            let value_pointer = child_pointer(pointer, "value");
            let value =
                require(map, "value", pointer)?
                    .as_i64()
                    .ok_or_else(|| PolicyError::WrongType {
                        pointer: value_pointer,
                        expected: "an integer".to_string(),
                    })?;
            Predicate::FieldGreaterThan {
                field: require_string(map, "field", pointer)?,
                value,
            }
        }
        other => {
            return Err(PolicyError::UnknownPredicateKind {
                pointer: child_pointer(pointer, "kind"),
                kind: other.to_string(),
            });
        }
    };

    // An arg predicate can only resolve against a Bash invocation's arguments
    // and a field predicate only against a Fields tool's input; a predicate
    // in the wrong place would never hold, so its rule would silently never
    // fire -- fail closed at parse time instead.
    let applies = if tool == ToolKind::Bash {
        predicate.reads_args()
    } else {
        !predicate.reads_args()
    };
    if !applies {
        return Err(PolicyError::PredicateNotApplicable {
            pointer: child_pointer(pointer, "kind"),
            kind,
            tool: tool.as_str().to_string(),
        });
    }
    Ok(predicate)
}

/// A predicate's `any_of` list: one or more non-empty strings. An empty list
/// would never match and an empty string would match everything (every
/// string contains and ends with `""`), so both are rejected as the policy
/// errors they are rather than shipped as a rule that fails open or closed by
/// accident.
fn parse_any_of(
    map: &serde_json::Map<String, Value>,
    pointer: &str,
) -> Result<Vec<String>, PolicyError> {
    let any_of_pointer = child_pointer(pointer, "any_of");
    let values = parse_string_array(require(map, "any_of", pointer)?, &any_of_pointer)?;
    if values.is_empty() || values.iter().any(String::is_empty) {
        return Err(PolicyError::WrongType {
            pointer: any_of_pointer,
            expected: "a non-empty array of non-empty strings".to_string(),
        });
    }
    Ok(values)
}

fn parse_outcome(
    value: &Value,
    pointer: &str,
    sym_job_ids: &[&str],
) -> Result<RuleOutcome, PolicyError> {
    let map = as_object(value, pointer)?;
    let kind = require_string(map, "kind", pointer)?;
    match kind.as_str() {
        "allow" => {
            check_known_keys(map, pointer, &["kind", "note"])?;
            let note = match map.get("note") {
                Some(note_value) => Some(as_string(note_value, &child_pointer(pointer, "note"))?),
                None => None,
            };
            Ok(RuleOutcome::Allow { note })
        }
        "rewrite" => {
            check_known_keys(map, pointer, &["kind", "target", "reason"])?;
            let target = require_string(map, "target", pointer)?;
            let reason = require_string(map, "reason", pointer)?;
            if target.is_empty() || reason.is_empty() {
                return Err(PolicyError::IncompleteRewrite {
                    pointer: root_pointer(pointer),
                });
            }
            Ok(RuleOutcome::Rewrite { target, reason })
        }
        "proxy" => {
            check_known_keys(map, pointer, &["kind", "reason"])?;
            let reason_str = require_string(map, "reason", pointer)?;
            let reason = ProxyReason::try_from(reason_str.as_str()).map_err(|_| {
                PolicyError::UnknownProxyReason {
                    pointer: child_pointer(pointer, "reason"),
                    reason: reason_str,
                }
            })?;
            Ok(RuleOutcome::Proxy { reason })
        }
        "deny" => {
            check_known_keys(map, pointer, &["kind", "reason", "instead"])?;
            let reason = require_string(map, "reason", pointer)?;
            let instead = require_string(map, "instead", pointer)?;
            if reason.is_empty() || instead.is_empty() {
                return Err(PolicyError::IncompleteDeny {
                    pointer: root_pointer(pointer),
                });
            }
            Ok(RuleOutcome::Deny { reason, instead })
        }
        "ask" => {
            check_known_keys(
                map,
                pointer,
                &["kind", "question", "reason", "needs_operator"],
            )?;
            let question = require_string(map, "question", pointer)?;
            let reason = require_string(map, "reason", pointer)?;
            if question.is_empty() || reason.is_empty() {
                return Err(PolicyError::IncompleteAsk {
                    pointer: root_pointer(pointer),
                });
            }
            let needs_operator = optional_bool(map, "needs_operator", pointer)?;
            Ok(RuleOutcome::Ask {
                question,
                reason,
                needs_operator,
            })
        }
        "sym" => {
            check_known_keys(map, pointer, &["kind", "job"])?;
            let job = require_string(map, "job", pointer)?;
            if !sym_job_ids.contains(&job.as_str()) {
                return Err(PolicyError::UnknownSymJobRef {
                    pointer: child_pointer(pointer, "job"),
                    job,
                });
            }
            Ok(RuleOutcome::Sym { job })
        }
        other => Err(PolicyError::UnknownDecision {
            pointer: child_pointer(pointer, "kind"),
            arm: other.to_string(),
        }),
    }
}

// -- sym jobs, wrappers, interpreters, script carriers ------------------------

fn parse_sym_jobs(
    value: &Value,
    pointer: &str,
    seen_ids: &mut HashMap<String, String>,
) -> Result<Vec<SymJob>, PolicyError> {
    let array = as_array(value, pointer)?;
    let mut jobs = Vec::with_capacity(array.len());
    for (index, job_value) in array.iter().enumerate() {
        let job_pointer = child_pointer(pointer, &index.to_string());
        let map = as_object(job_value, &job_pointer)?;
        check_known_keys(
            map,
            &job_pointer,
            &["id", "sym_command", "interpreter_patterns"],
        )?;
        let id = require_string(map, "id", &job_pointer)?;
        if id.is_empty() {
            return Err(PolicyError::EmptySymJobId {
                pointer: child_pointer(&job_pointer, "id"),
            });
        }
        register_id(seen_ids, &id, &child_pointer(&job_pointer, "id"))?;
        let sym_command = require_string(map, "sym_command", &job_pointer)?;
        if sym_command.is_empty() {
            return Err(PolicyError::SymJobMissingCommand {
                pointer: job_pointer.clone(),
            });
        }
        let interpreter_patterns = match map.get("interpreter_patterns") {
            Some(patterns) => parse_string_array(
                patterns,
                &child_pointer(&job_pointer, "interpreter_patterns"),
            )?,
            None => Vec::new(),
        };
        jobs.push(SymJob {
            id,
            sym_command,
            interpreter_patterns,
        });
    }
    Ok(jobs)
}

fn parse_string_array(value: &Value, pointer: &str) -> Result<Vec<String>, PolicyError> {
    let array = as_array(value, pointer)?;
    let mut out = Vec::with_capacity(array.len());
    for (index, item) in array.iter().enumerate() {
        out.push(as_string(
            item,
            &child_pointer(pointer, &index.to_string()),
        )?);
    }
    Ok(out)
}

fn parse_wrappers(value: &Value, pointer: &str) -> Result<Vec<Wrapper>, PolicyError> {
    let array = as_array(value, pointer)?;
    let mut wrappers = Vec::with_capacity(array.len());
    for (index, wrapper_value) in array.iter().enumerate() {
        let wrapper_pointer = child_pointer(pointer, &index.to_string());
        let map = as_object(wrapper_value, &wrapper_pointer)?;
        check_known_keys(map, &wrapper_pointer, &["binary", "required_subcommand"])?;
        let binary = require_string(map, "binary", &wrapper_pointer)?;
        let required_subcommand = match map.get("required_subcommand") {
            Some(sub) => Some(as_string(
                sub,
                &child_pointer(&wrapper_pointer, "required_subcommand"),
            )?),
            None => None,
        };
        wrappers.push(Wrapper {
            binary,
            required_subcommand,
        });
    }
    Ok(wrappers)
}

fn parse_interpreters(value: &Value, pointer: &str) -> Result<Vec<Interpreter>, PolicyError> {
    let array = as_array(value, pointer)?;
    let mut interpreters = Vec::with_capacity(array.len());
    for (index, interpreter_value) in array.iter().enumerate() {
        let interpreter_pointer = child_pointer(pointer, &index.to_string());
        let map = as_object(interpreter_value, &interpreter_pointer)?;
        check_known_keys(map, &interpreter_pointer, &["binary", "flag", "body"])?;
        let binary = require_string(map, "binary", &interpreter_pointer)?;
        let flag = require_string(map, "flag", &interpreter_pointer)?;
        let body_str = require_string(map, "body", &interpreter_pointer)?;
        let body = match body_str.as_str() {
            "shell" => BodyLanguage::Shell,
            "foreign" => BodyLanguage::Foreign,
            other => {
                return Err(PolicyError::UnknownBodyLanguage {
                    pointer: child_pointer(&interpreter_pointer, "body"),
                    language: other.to_string(),
                });
            }
        };
        interpreters.push(Interpreter { binary, flag, body });
    }
    Ok(interpreters)
}

fn parse_script_carriers(value: &Value, pointer: &str) -> Result<Vec<ScriptCarrier>, PolicyError> {
    let array = as_array(value, pointer)?;
    let mut carriers = Vec::with_capacity(array.len());
    for (index, carrier_value) in array.iter().enumerate() {
        let carrier_pointer = child_pointer(pointer, &index.to_string());
        let map = as_object(carrier_value, &carrier_pointer)?;
        check_known_keys(map, &carrier_pointer, &["binary"])?;
        let binary = require_string(map, "binary", &carrier_pointer)?;
        carriers.push(ScriptCarrier { binary });
    }
    Ok(carriers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_minimal_bash_family_rule() {
        let text = r#"{
            "tools": {
                "Bash": {
                    "families": {
                        "git push": {
                            "rules": [
                                {
                                    "id": "git-push-force",
                                    "predicates": [{"kind": "arg-present", "arg": "--force"}],
                                    "outcome": {"kind": "deny", "reason": "force push", "instead": "git push"}
                                }
                            ]
                        }
                    }
                }
            }
        }"#;
        let policy = parse_policy(text).expect("valid policy");
        assert!(!policy.is_empty());
        let ToolRules::Bash { families } = &policy.tools[&ToolKind::Bash] else {
            panic!("expected Bash rules");
        };
        let family = &families["git push"];
        assert_eq!(family.rules.len(), 1);
        assert_eq!(family.rules[0].id, "git-push-force");
        assert_eq!(
            family.rules[0].predicates,
            vec![Predicate::ArgPresent("--force".to_string())]
        );
    }

    #[test]
    fn empty_policy_string_object_is_empty() {
        let policy = parse_policy("{}").expect("valid");
        assert!(policy.is_empty());
    }

    #[test]
    fn a_policy_with_only_sym_jobs_is_empty() {
        let text = r#"{"sym_jobs": [{"id": "find-content", "sym_command": "legion sym find-content", "interpreter_patterns": ["rglob"]}]}"#;
        let policy = parse_policy(text).expect("valid");
        assert!(policy.is_empty());
    }

    #[test]
    fn a_tool_with_no_families_is_empty() {
        let policy = parse_policy(r#"{"tools": {"Bash": {"families": {}}}}"#).expect("valid");
        assert!(policy.is_empty());
    }

    #[test]
    fn a_family_with_no_rules_is_empty() {
        let policy = parse_policy(r#"{"tools": {"Bash": {"families": {"gh": {"rules": []}}}}}"#)
            .expect("valid");
        assert!(policy.is_empty());
    }

    #[test]
    fn unknown_top_level_field_is_rejected_with_its_pointer() {
        let err = parse_policy(r#"{"toolz": {}}"#).expect_err("unknown field");
        assert_eq!(
            err,
            PolicyError::UnknownField {
                pointer: "/toolz".to_string(),
                field: "toolz".to_string(),
            }
        );
    }

    #[test]
    fn unknown_field_inside_a_rule_names_its_pointer() {
        // A typo in `requires_recall` would otherwise drop the lookup
        // requirement and fail the rule open; it must error instead.
        let text = r#"{
            "tools": {"Bash": {"families": {"gh": {"rules": [
                {"id": "x", "outcome": {"kind": "allow"}, "requires_recal": true}
            ]}}}}
        }"#;
        let err = parse_policy(text).expect_err("typo'd field must error");
        assert_eq!(
            err,
            PolicyError::UnknownField {
                pointer: "/tools/Bash/families/gh/rules/0/requires_recal".to_string(),
                field: "requires_recal".to_string(),
            }
        );
    }

    #[test]
    fn unknown_tool_kind_is_rejected() {
        let err = parse_policy(r#"{"tools": {"Shell": {"families": {}}}}"#).expect_err("bad kind");
        assert_eq!(
            err,
            PolicyError::UnknownToolKind {
                pointer: "/tools/Shell".to_string(),
                kind: "Shell".to_string(),
            }
        );
    }

    #[test]
    fn unknown_decision_arm_is_rejected() {
        let text = r#"{"tools": {"Bash": {"families": {"gh": {"rules": [
            {"id": "x", "outcome": {"kind": "block"}}
        ]}}}}}"#;
        let err = parse_policy(text).expect_err("bad arm");
        assert_eq!(
            err,
            PolicyError::UnknownDecision {
                pointer: "/tools/Bash/families/gh/rules/0/outcome/kind".to_string(),
                arm: "block".to_string(),
            }
        );
    }

    #[test]
    fn unknown_proxy_reason_is_rejected() {
        let text = r#"{"tools": {"Bash": {"families": {"gh": {"rules": [
            {"id": "x", "outcome": {"kind": "proxy", "reason": "network"}}
        ]}}}}}"#;
        let err = parse_policy(text).expect_err("bad proxy reason");
        assert_eq!(
            err,
            PolicyError::UnknownProxyReason {
                pointer: "/tools/Bash/families/gh/rules/0/outcome/reason".to_string(),
                reason: "network".to_string(),
            }
        );
    }

    #[test]
    fn ask_rule_without_question_is_rejected() {
        let text = r#"{"tools": {"Bash": {"families": {"gh": {"rules": [
            {"id": "x", "outcome": {"kind": "ask", "question": "", "reason": "why"}}
        ]}}}}}"#;
        let err = parse_policy(text).expect_err("incomplete ask");
        assert_eq!(
            err,
            PolicyError::IncompleteAsk {
                pointer: "/tools/Bash/families/gh/rules/0/outcome".to_string(),
            }
        );
    }

    #[test]
    fn needs_operator_on_non_ask_outcome_is_rejected_as_unknown_field() {
        let text = r#"{"tools": {"Bash": {"families": {"gh": {"rules": [
            {"id": "x", "outcome": {"kind": "allow", "needs_operator": true}}
        ]}}}}}"#;
        let err = parse_policy(text).expect_err("needs_operator only valid on ask");
        assert_eq!(
            err,
            PolicyError::UnknownField {
                pointer: "/tools/Bash/families/gh/rules/0/outcome/needs_operator".to_string(),
                field: "needs_operator".to_string(),
            }
        );
    }

    #[test]
    fn sym_job_without_command_is_rejected() {
        let text = r#"{"sym_jobs": [{"id": "x", "sym_command": ""}]}"#;
        let err = parse_policy(text).expect_err("missing sym command");
        assert_eq!(
            err,
            PolicyError::SymJobMissingCommand {
                pointer: "/sym_jobs/0".to_string(),
            }
        );
    }

    #[test]
    fn rule_without_id_is_rejected() {
        let text = r#"{"tools": {"Bash": {"families": {"gh": {"rules": [
            {"outcome": {"kind": "allow"}}
        ]}}}}}"#;
        let err = parse_policy(text).expect_err("missing id");
        assert_eq!(
            err,
            PolicyError::MissingField {
                pointer: "/tools/Bash/families/gh/rules/0".to_string(),
                field: "id".to_string(),
            }
        );
    }

    #[test]
    fn duplicate_rule_id_across_families_is_rejected() {
        let text = r#"{"tools": {"Bash": {"families": {
            "gh": {"rules": [{"id": "dup", "outcome": {"kind": "allow"}}]},
            "git push": {"rules": [{"id": "dup", "outcome": {"kind": "allow"}}]}
        }}}}"#;
        let err = parse_policy(text).expect_err("duplicate id");
        match err {
            PolicyError::DuplicateRuleId { id, first, .. } => {
                assert_eq!(id, "dup");
                assert_eq!(first, "/tools/Bash/families/gh/rules/0/id");
            }
            other => panic!("expected DuplicateRuleId, got {other:?}"),
        }
    }

    #[test]
    fn sym_action_referencing_unknown_job_is_rejected() {
        let text = r#"{"tools": {"Bash": {"families": {"grep": {"rules": [
            {"id": "x", "outcome": {"kind": "sym", "job": "nope"}}
        ]}}}}}"#;
        let err = parse_policy(text).expect_err("dangling sym ref");
        assert_eq!(
            err,
            PolicyError::UnknownSymJobRef {
                pointer: "/tools/Bash/families/grep/rules/0/outcome/job".to_string(),
                job: "nope".to_string(),
            }
        );
    }

    #[test]
    fn unknown_predicate_kind_is_rejected_with_a_dedicated_error() {
        let text = r#"{"tools": {"Bash": {"families": {"gh": {"rules": [
            {"id": "x", "predicates": [{"kind": "arg-matches", "arg": "y"}],
             "outcome": {"kind": "allow"}}
        ]}}}}}"#;
        let err = parse_policy(text).expect_err("bad predicate kind");
        assert_eq!(
            err,
            PolicyError::UnknownPredicateKind {
                pointer: "/tools/Bash/families/gh/rules/0/predicates/0/kind".to_string(),
                kind: "arg-matches".to_string(),
            }
        );
    }

    #[test]
    fn every_tool_field_kind_parses_as_a_fields_tool() {
        // The ten tool kinds the tool-field hooks are registered under
        // (hooks.json) all parse, each as an ordered Fields rule list.
        let text = r#"{"tools": {
            "Grep": {"rules": []}, "Glob": {"rules": []}, "Read": {"rules": []},
            "Write": {"rules": []}, "Edit": {"rules": []}, "MultiEdit": {"rules": []},
            "Agent": {"rules": []}, "Task": {"rules": []},
            "WebFetch": {"rules": []}, "WebSearch": {"rules": []}
        }}"#;
        let policy = parse_policy(text).expect("valid");
        for kind in [
            ToolKind::Grep,
            ToolKind::Glob,
            ToolKind::Read,
            ToolKind::Write,
            ToolKind::Edit,
            ToolKind::MultiEdit,
            ToolKind::Agent,
            ToolKind::Task,
            ToolKind::WebFetch,
            ToolKind::WebSearch,
        ] {
            assert_eq!(
                policy.tools.get(&kind),
                Some(&ToolRules::Fields { rules: Vec::new() }),
                "{kind:?} must parse as a Fields tool"
            );
        }
    }

    #[test]
    fn every_field_predicate_kind_parses_under_a_fields_tool() {
        let text = r#"{"tools": {"Read": {"rules": [
            {"id": "r", "predicates": [
                {"kind": "field-present", "field": "file_path"},
                {"kind": "field-absent", "field": "limit"},
                {"kind": "field-equals", "field": "a", "any_of": ["x", "y"], "ignore_case": true},
                {"kind": "field-contains", "field": "b", "any_of": ["/memory/"]},
                {"kind": "field-ends-with", "field": "c", "any_of": [".rs"]},
                {"kind": "field-greater-than", "field": "limit", "value": 200}
            ], "outcome": {"kind": "allow"}}
        ]}}}"#;
        let policy = parse_policy(text).expect("valid");
        let ToolRules::Fields { rules } = &policy.tools[&ToolKind::Read] else {
            panic!("expected Fields rules");
        };
        assert_eq!(
            rules[0].predicates,
            vec![
                Predicate::FieldPresent {
                    field: "file_path".to_string()
                },
                Predicate::FieldAbsent {
                    field: "limit".to_string()
                },
                Predicate::FieldEquals {
                    field: "a".to_string(),
                    any_of: vec!["x".to_string(), "y".to_string()],
                    ignore_case: true,
                },
                Predicate::FieldContains {
                    field: "b".to_string(),
                    any_of: vec!["/memory/".to_string()],
                },
                Predicate::FieldEndsWith {
                    field: "c".to_string(),
                    any_of: vec![".rs".to_string()],
                },
                Predicate::FieldGreaterThan {
                    field: "limit".to_string(),
                    value: 200,
                },
            ]
        );
    }

    #[test]
    fn field_equals_ignore_case_defaults_to_false() {
        let text = r#"{"tools": {"Agent": {"rules": [
            {"id": "r", "predicates": [{"kind": "field-equals", "field": "a", "any_of": ["x"]}],
             "outcome": {"kind": "allow"}}
        ]}}}"#;
        let policy = parse_policy(text).expect("valid");
        let ToolRules::Fields { rules } = &policy.tools[&ToolKind::Agent] else {
            panic!("expected Fields rules");
        };
        assert_eq!(
            rules[0].predicates,
            vec![Predicate::FieldEquals {
                field: "a".to_string(),
                any_of: vec!["x".to_string()],
                ignore_case: false,
            }]
        );
    }

    #[test]
    fn a_field_predicate_under_bash_is_rejected() {
        // A field predicate has no Bash argument to resolve against; a rule
        // carrying one would never fire, so it is a policy error.
        let text = r#"{"tools": {"Bash": {"families": {"gh": {"rules": [
            {"id": "x", "predicates": [{"kind": "field-present", "field": "command"}],
             "outcome": {"kind": "allow"}}
        ]}}}}}"#;
        let err = parse_policy(text).expect_err("field predicate under Bash");
        assert_eq!(
            err,
            PolicyError::PredicateNotApplicable {
                pointer: "/tools/Bash/families/gh/rules/0/predicates/0/kind".to_string(),
                kind: "field-present".to_string(),
                tool: "Bash".to_string(),
            }
        );
    }

    #[test]
    fn an_arg_predicate_under_a_fields_tool_is_rejected() {
        let text = r#"{"tools": {"Grep": {"rules": [
            {"id": "x", "predicates": [{"kind": "arg-present", "arg": "secret"}],
             "outcome": {"kind": "allow"}}
        ]}}}"#;
        let err = parse_policy(text).expect_err("arg predicate under a Fields tool");
        assert_eq!(
            err,
            PolicyError::PredicateNotApplicable {
                pointer: "/tools/Grep/rules/0/predicates/0/kind".to_string(),
                kind: "arg-present".to_string(),
                tool: "Grep".to_string(),
            }
        );
    }

    #[test]
    fn an_empty_any_of_list_is_rejected() {
        let text = r#"{"tools": {"Write": {"rules": [
            {"id": "x", "predicates": [{"kind": "field-contains", "field": "content", "any_of": []}],
             "outcome": {"kind": "allow"}}
        ]}}}"#;
        let err = parse_policy(text).expect_err("empty any_of");
        assert_eq!(
            err,
            PolicyError::WrongType {
                pointer: "/tools/Write/rules/0/predicates/0/any_of".to_string(),
                expected: "a non-empty array of non-empty strings".to_string(),
            }
        );
    }

    #[test]
    fn an_empty_string_in_any_of_is_rejected() {
        // `ends_with("")` holds for every string: an empty suffix would turn a
        // narrow deny into a deny of every call.
        let text = r#"{"tools": {"Read": {"rules": [
            {"id": "x", "predicates": [{"kind": "field-ends-with", "field": "file_path", "any_of": [".rs", ""]}],
             "outcome": {"kind": "allow"}}
        ]}}}"#;
        let err = parse_policy(text).expect_err("empty string in any_of");
        assert!(matches!(err, PolicyError::WrongType { .. }), "got {err:?}");
    }

    #[test]
    fn a_non_integer_greater_than_value_is_rejected() {
        let text = r#"{"tools": {"Read": {"rules": [
            {"id": "x", "predicates": [{"kind": "field-greater-than", "field": "limit", "value": "200"}],
             "outcome": {"kind": "allow"}}
        ]}}}"#;
        let err = parse_policy(text).expect_err("string value");
        assert_eq!(
            err,
            PolicyError::WrongType {
                pointer: "/tools/Read/rules/0/predicates/0/value".to_string(),
                expected: "an integer".to_string(),
            }
        );
    }

    #[test]
    fn an_unknown_key_on_a_field_predicate_names_its_pointer() {
        // `ignore_case` is only valid on field-equals; on field-contains it
        // would be a silently ignored typo otherwise.
        let text = r#"{"tools": {"Write": {"rules": [
            {"id": "x", "predicates": [{"kind": "field-contains", "field": "content", "any_of": ["a"], "ignore_case": true}],
             "outcome": {"kind": "allow"}}
        ]}}}"#;
        let err = parse_policy(text).expect_err("unknown key");
        assert_eq!(
            err,
            PolicyError::UnknownField {
                pointer: "/tools/Write/rules/0/predicates/0/ignore_case".to_string(),
                field: "ignore_case".to_string(),
            }
        );
    }

    #[test]
    fn empty_sym_job_id_is_rejected() {
        let text = r#"{"sym_jobs": [{"id": "", "sym_command": "legion sym x"}]}"#;
        let err = parse_policy(text).expect_err("empty sym job id");
        assert_eq!(
            err,
            PolicyError::EmptySymJobId {
                pointer: "/sym_jobs/0/id".to_string(),
            }
        );
    }

    #[test]
    fn an_id_shared_between_a_rule_and_a_sym_job_is_rejected() {
        // Rule ids and sym-job ids share one namespace (Deciding.id carries
        // either), so a collision is ambiguous and must be rejected.
        let text = r#"{
            "sym_jobs": [{"id": "dup", "sym_command": "legion sym x", "interpreter_patterns": ["p"]}],
            "tools": {"Bash": {"families": {"gh": {"rules": [
                {"id": "dup", "outcome": {"kind": "allow"}}
            ]}}}}
        }"#;
        let err = parse_policy(text).expect_err("cross-namespace duplicate id");
        match err {
            PolicyError::DuplicateRuleId { id, first, .. } => {
                assert_eq!(id, "dup");
                assert_eq!(first, "/sym_jobs/0/id");
            }
            other => panic!("expected DuplicateRuleId, got {other:?}"),
        }
    }

    #[test]
    fn interpreter_body_language_is_validated() {
        let text = r#"{"interpreters": [{"binary": "sh", "flag": "-c", "body": "elvish"}]}"#;
        let err = parse_policy(text).expect_err("bad body language");
        assert_eq!(
            err,
            PolicyError::UnknownBodyLanguage {
                pointer: "/interpreters/0/body".to_string(),
                language: "elvish".to_string(),
            }
        );
    }

    #[test]
    fn wrappers_and_interpreters_and_carriers_parse() {
        let text = r#"{
            "wrappers": [
                {"binary": "env"},
                {"binary": "pnpm", "required_subcommand": "exec"}
            ],
            "interpreters": [
                {"binary": "sh", "flag": "-c", "body": "shell"},
                {"binary": "python3", "flag": "-c", "body": "foreign"}
            ],
            "script_carriers": [{"binary": "bash"}]
        }"#;
        let policy = parse_policy(text).expect("valid");
        assert_eq!(policy.wrappers.len(), 2);
        assert_eq!(
            policy.matching_wrapper("pnpm", &["exec".to_string(), "eslint".to_string()]),
            Some(&Wrapper {
                binary: "pnpm".to_string(),
                required_subcommand: Some("exec".to_string()),
            })
        );
        assert!(
            policy
                .matching_wrapper("pnpm", &["grep".to_string()])
                .is_none()
        );
        assert_eq!(
            policy.matching_interpreter("sh").map(|i| i.body),
            Some(BodyLanguage::Shell)
        );
        assert!(policy.matching_script_carrier("bash").is_some());
    }
}
