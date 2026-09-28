//! The routing policy as one declarative data structure (FR-CMD-011, #1337).
//!
//! For Bash the policy holds exactly four lists: the names that go to a
//! legion proxy (`proxy`), the commands that never run (`never_run`), the
//! commands the operator is asked about (`ask`), and the three power
//! switches that also need the operator (`power_switches`). Every entry in
//! the last three is data -- a name set and argument predicates -- never a
//! branch in Rust code. The `tools` section holds the rules for tools other
//! than Bash, matched on the call's own input fields. [`parse_policy`] reads
//! the JSON the adapter loads and builds a [`Policy`]; any other top-level
//! key is an error.

use std::collections::{BTreeMap, HashMap};

use serde_json::Value;

use crate::decision::ManagedTarget;
use crate::nogo::{self, NoGoEntry, NoGoPredicate};
use crate::splitter::Position;

/// A tool other than Bash whose calls the `tools` section governs. Bash is
/// not a member: a Bash command is routed by the four lists alone, so a
/// `Bash` key under `tools` is a policy error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ToolKind {
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
    /// Every member, so the parser rejects a wire name outside the set.
    pub const ALL: [ToolKind; 10] = [
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

    /// The kind whose wire name is `raw`, if any.
    pub fn parse(raw: &str) -> Option<ToolKind> {
        ToolKind::ALL.into_iter().find(|k| k.as_str() == raw)
    }
}

/// One rule for a tool other than Bash: predicates over the call's input
/// fields that must all hold, the lookups it requires, and the action it
/// yields when matched. Data only (FR-CMD-011).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// Unique across the whole policy: the incident records name it (#1237).
    pub id: String,
    /// All must hold for the rule to match.
    pub predicates: Vec<Predicate>,
    /// The rule matches only when `ctx.recall` is not [`crate::Lookup::NotFetched`].
    pub requires_recall: bool,
    /// The rule matches only when `ctx.consult` is not [`crate::Lookup::NotFetched`].
    pub requires_consult: bool,
    pub outcome: RuleOutcome,
}

/// A rule predicate over one top-level key of a tool call's `tool_input`.
///
/// A predicate that READS a value (`FieldEquals`, `FieldContains`,
/// `FieldEndsWith`, `FieldGreaterThan`) is unresolvable when the call carries
/// no such field, or carries it with the wrong JSON type; the evaluator turns
/// that into the FR-CMD-016 deny. `FieldPresent`/`FieldAbsent` only test
/// presence and are always resolvable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Predicate {
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

/// What a matched rule yields: one of the four Decision arms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleOutcome {
    Allow {
        note: Option<String>,
    },
    /// Put `target` in place of the call's rewritable field and keep every
    /// other field.
    Rewrite {
        target: ManagedTarget,
        reason: String,
    },
    Deny {
        reason: String,
        instead: String,
    },
    Ask {
        question: String,
        reason: String,
        /// Whether the call needs the operator after the agent confirms
        /// (FR-CMD-006).
        needs_operator: bool,
    },
}

/// The parsed policy (FR-CMD-011, #1337).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Policy {
    /// Command names that get `legion ` inserted before them.
    pub proxy: Vec<String>,
    /// Never-run entries the policy file adds (FR-CMD-025). They extend the
    /// built-in list and can never remove or weaken it: see
    /// [`Policy::never_run_entries`].
    pub never_run: Vec<NoGoEntry>,
    /// Commands the operator is asked about.
    pub ask: Vec<NoGoEntry>,
    /// The power switches, matched on the command after proxy insertion and
    /// asked the same way.
    pub power_switches: Vec<NoGoEntry>,
    /// Rules for tools other than Bash.
    pub tools: BTreeMap<ToolKind, Vec<Rule>>,
}

impl Policy {
    /// Every never-run entry route checks (FR-CMD-025): the built-in entries
    /// first, then the entries this policy adds. The built-ins are not
    /// policy data, so no policy file can remove one; listing them first
    /// means an added entry that reuses a built-in id never takes its place.
    pub fn never_run_entries(&self) -> Vec<NoGoEntry> {
        let mut entries = nogo::builtin_no_go();
        entries.extend(self.never_run.iter().cloned());
        entries
    }
}

/// Every failure `parse_policy` can raise (FR-CMD-011 Error Handling). Each
/// carries the JSON pointer of the entry that failed, so an operator editing
/// the policy is told exactly where the error is.
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

    #[error("{pointer}: ask rule must carry a non-empty question and reason")]
    IncompleteAsk { pointer: String },

    #[error("{pointer}: deny rule must carry a non-empty reason and instead")]
    IncompleteDeny { pointer: String },

    #[error("{pointer}: rewrite rule must carry a non-empty target and reason")]
    IncompleteRewrite { pointer: String },

    #[error("{pointer}: id '{id}' is empty")]
    EmptyRuleId { pointer: String, id: String },

    #[error("{pointer}: unknown command position '{position}'")]
    UnknownPosition { pointer: String, position: String },

    #[error("{pointer}: entry must name at least one command name or name prefix")]
    EntryWithoutName { pointer: String },

    #[error("{pointer}: entry must carry a non-empty reason")]
    EntryWithoutReason { pointer: String },

    #[error("{pointer}: duplicate id '{id}', first defined at {first}")]
    DuplicateRuleId {
        pointer: String,
        id: String,
        first: String,
    },
}

/// The top-level keys a policy file may carry: the four Bash lists and the
/// rules for other tools. Anything else fails to parse.
const TOP_LEVEL_KEYS: [&str; 5] = ["proxy", "never_run", "ask", "power_switches", "tools"];

/// Parses policy text into a [`Policy`] (FR-CMD-011). Pure: the caller reads
/// the file and passes its contents; route never opens it.
///
/// Every entry is validated. An unknown field anywhere -- including a
/// top-level key other than the four lists and `tools` -- is an error rather
/// than silently ignored. Ids are unique across the whole policy.
pub fn parse_policy(text: &str) -> Result<Policy, PolicyError> {
    let root: Value = serde_json::from_str(text).map_err(|e| PolicyError::Json {
        detail: e.to_string(),
    })?;
    let root = as_object(&root, "")?;
    check_known_keys(root, "", &TOP_LEVEL_KEYS)?;

    // Ids are unique across the whole policy: an incident record names the
    // id (#1237), whether an entry or a rule produced it.
    let mut seen_ids: HashMap<String, String> = HashMap::new();

    let proxy: Vec<String> = match root.get("proxy") {
        Some(value) => parse_names(value, "/proxy")?,
        None => Vec::new(),
    };
    let mut entries = |key: &str| -> Result<Vec<NoGoEntry>, PolicyError> {
        match root.get(key) {
            Some(value) => parse_entries(value, &format!("/{key}"), &mut seen_ids),
            None => Ok(Vec::new()),
        }
    };
    let never_run: Vec<NoGoEntry> = entries("never_run")?;
    let ask: Vec<NoGoEntry> = entries("ask")?;
    let power_switches: Vec<NoGoEntry> = entries("power_switches")?;
    let tools = match root.get("tools") {
        Some(value) => parse_tools(value, "/tools", &mut seen_ids)?,
        None => BTreeMap::new(),
    };

    Ok(Policy {
        proxy,
        never_run,
        ask,
        power_switches,
        tools,
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

fn optional_string_array(
    map: &serde_json::Map<String, Value>,
    key: &str,
    pointer: &str,
) -> Result<Vec<String>, PolicyError> {
    match map.get(key) {
        Some(value) => parse_string_array(value, &child_pointer(pointer, key)),
        None => Ok(Vec::new()),
    }
}

/// A list of command names: every entry a non-empty string. An empty name
/// would match no command word, so it is a policy error rather than a name
/// that silently never fires.
fn parse_names(value: &Value, pointer: &str) -> Result<Vec<String>, PolicyError> {
    let names = parse_string_array(value, pointer)?;
    if let Some(index) = names.iter().position(String::is_empty) {
        return Err(PolicyError::WrongType {
            pointer: child_pointer(pointer, &index.to_string()),
            expected: "a non-empty command name".to_string(),
        });
    }
    Ok(names)
}

/// Registers an id in the policy-wide id set, rejecting a duplicate.
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

/// A non-empty id at `map`'s `id` key, registered as unique.
fn parse_id(
    map: &serde_json::Map<String, Value>,
    pointer: &str,
    seen_ids: &mut HashMap<String, String>,
) -> Result<String, PolicyError> {
    let id = require_string(map, "id", pointer)?;
    let id_pointer = child_pointer(pointer, "id");
    if id.is_empty() {
        return Err(PolicyError::EmptyRuleId {
            pointer: id_pointer,
            id,
        });
    }
    register_id(seen_ids, &id, &id_pointer)?;
    Ok(id)
}

// -- the never-run, ask and power-switch lists --------------------------------

/// Parses one of the three entry lists: the same predicate shape as the
/// built-in never-run entries (FR-CMD-025). Each entry is validated like
/// every other policy entry -- an unknown field is an error, never a
/// silently weaker entry.
fn parse_entries(
    value: &Value,
    pointer: &str,
    seen_ids: &mut HashMap<String, String>,
) -> Result<Vec<NoGoEntry>, PolicyError> {
    let array = as_array(value, pointer)?;
    let mut entries = Vec::with_capacity(array.len());
    for (index, entry_value) in array.iter().enumerate() {
        let entry_pointer = child_pointer(pointer, &index.to_string());
        let map = as_object(entry_value, &entry_pointer)?;
        check_known_keys(
            map,
            &entry_pointer,
            &[
                "id",
                "names",
                "name_prefixes",
                "position",
                "predicates",
                "reason",
            ],
        )?;
        let id = parse_id(map, &entry_pointer, seen_ids)?;
        let names = match map.get("names") {
            Some(raw) => parse_names(raw, &child_pointer(&entry_pointer, "names"))?,
            None => Vec::new(),
        };
        let name_prefixes = match map.get("name_prefixes") {
            Some(raw) => parse_names(raw, &child_pointer(&entry_pointer, "name_prefixes"))?,
            None => Vec::new(),
        };
        if names.is_empty() && name_prefixes.is_empty() {
            return Err(PolicyError::EntryWithoutName {
                pointer: entry_pointer,
            });
        }
        let position = match map.get("position") {
            Some(raw) => {
                let position_pointer = child_pointer(&entry_pointer, "position");
                let name = as_string(raw, &position_pointer)?;
                Some(parse_position(&name).ok_or(PolicyError::UnknownPosition {
                    pointer: position_pointer,
                    position: name,
                })?)
            }
            None => None,
        };
        let predicates = match map.get("predicates") {
            Some(raw) => parse_entry_predicates(raw, &child_pointer(&entry_pointer, "predicates"))?,
            None => Vec::new(),
        };
        let reason = require_string(map, "reason", &entry_pointer)?;
        if reason.is_empty() {
            return Err(PolicyError::EntryWithoutReason {
                pointer: entry_pointer,
            });
        }
        entries.push(NoGoEntry {
            id,
            names,
            name_prefixes,
            position,
            predicates,
            reason,
        });
    }
    Ok(entries)
}

fn parse_entry_predicates(value: &Value, pointer: &str) -> Result<Vec<NoGoPredicate>, PolicyError> {
    let array = as_array(value, pointer)?;
    let mut predicates = Vec::with_capacity(array.len());
    for (index, item) in array.iter().enumerate() {
        let item_pointer = child_pointer(pointer, &index.to_string());
        let map = as_object(item, &item_pointer)?;
        let kind = require_string(map, "kind", &item_pointer)?;
        let predicate = match kind.as_str() {
            "flag" => {
                check_known_keys(map, &item_pointer, &["kind", "short", "long"])?;
                let short_pointer = child_pointer(&item_pointer, "short");
                let mut short = Vec::new();
                for (i, flag) in optional_string_array(map, "short", &item_pointer)?
                    .into_iter()
                    .enumerate()
                {
                    let mut chars = flag.chars();
                    match (chars.next(), chars.next()) {
                        (Some(c), None) => short.push(c),
                        _ => {
                            return Err(PolicyError::WrongType {
                                pointer: child_pointer(&short_pointer, &i.to_string()),
                                expected: "a one-character flag".to_string(),
                            });
                        }
                    }
                }
                NoGoPredicate::Flag {
                    short,
                    long: optional_string_array(map, "long", &item_pointer)?,
                }
            }
            "operand" => {
                check_known_keys(
                    map,
                    &item_pointer,
                    &["kind", "equals", "prefixes", "suffixes"],
                )?;
                NoGoPredicate::Operand {
                    equals: optional_string_array(map, "equals", &item_pointer)?,
                    prefixes: optional_string_array(map, "prefixes", &item_pointer)?,
                    suffixes: optional_string_array(map, "suffixes", &item_pointer)?,
                }
            }
            "forced-refspec" => {
                check_known_keys(map, &item_pointer, &["kind", "names"])?;
                NoGoPredicate::ForcedRefspec {
                    names: optional_string_array(map, "names", &item_pointer)?,
                }
            }
            _ => {
                return Err(PolicyError::UnknownPredicateKind {
                    pointer: child_pointer(&item_pointer, "kind"),
                    kind,
                });
            }
        };
        predicates.push(predicate);
    }
    Ok(predicates)
}

/// A [`Position`] by its kebab-case wire name.
fn parse_position(name: &str) -> Option<Position> {
    match name {
        "first" => Some(Position::First),
        "after-operator" => Some(Position::AfterOperator),
        "after-assignment" => Some(Position::AfterAssignment),
        "substitution" => Some(Position::Substitution),
        "function-body" => Some(Position::FunctionBody),
        _ => None,
    }
}

// -- tools other than Bash ----------------------------------------------------

fn parse_tools(
    value: &Value,
    pointer: &str,
    seen_ids: &mut HashMap<String, String>,
) -> Result<BTreeMap<ToolKind, Vec<Rule>>, PolicyError> {
    let map = as_object(value, pointer)?;
    let mut tools = BTreeMap::new();
    for (key, tool_value) in map {
        let tool_pointer = child_pointer(pointer, key);
        let kind = ToolKind::parse(key).ok_or_else(|| PolicyError::UnknownToolKind {
            pointer: tool_pointer.clone(),
            kind: key.clone(),
        })?;
        let tool_map = as_object(tool_value, &tool_pointer)?;
        check_known_keys(tool_map, &tool_pointer, &["rules"])?;
        let rules = match tool_map.get("rules") {
            Some(rules_value) => parse_rules(
                rules_value,
                &child_pointer(&tool_pointer, "rules"),
                seen_ids,
            )?,
            None => Vec::new(),
        };
        tools.insert(kind, rules);
    }
    Ok(tools)
}

fn parse_rules(
    value: &Value,
    pointer: &str,
    seen_ids: &mut HashMap<String, String>,
) -> Result<Vec<Rule>, PolicyError> {
    let array = as_array(value, pointer)?;
    let mut rules = Vec::with_capacity(array.len());
    for (index, rule_value) in array.iter().enumerate() {
        let rule_pointer = child_pointer(pointer, &index.to_string());
        rules.push(parse_rule(rule_value, &rule_pointer, seen_ids)?);
    }
    Ok(rules)
}

fn parse_rule(
    value: &Value,
    pointer: &str,
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
    let id = parse_id(map, pointer, seen_ids)?;
    let predicates = match map.get("predicates") {
        Some(preds) => parse_predicates(preds, &child_pointer(pointer, "predicates"))?,
        None => Vec::new(),
    };
    let requires_recall = optional_bool(map, "requires_recall", pointer)?;
    let requires_consult = optional_bool(map, "requires_consult", pointer)?;
    let outcome = parse_outcome(
        require(map, "outcome", pointer)?,
        &child_pointer(pointer, "outcome"),
    )?;
    Ok(Rule {
        id,
        predicates,
        requires_recall,
        requires_consult,
        outcome,
    })
}

fn parse_predicates(value: &Value, pointer: &str) -> Result<Vec<Predicate>, PolicyError> {
    let array = as_array(value, pointer)?;
    let mut predicates = Vec::with_capacity(array.len());
    for (index, predicate_value) in array.iter().enumerate() {
        let predicate_pointer = child_pointer(pointer, &index.to_string());
        predicates.push(parse_predicate(predicate_value, &predicate_pointer)?);
    }
    Ok(predicates)
}

fn parse_predicate(value: &Value, pointer: &str) -> Result<Predicate, PolicyError> {
    let map = as_object(value, pointer)?;
    let kind = require_string(map, "kind", pointer)?;
    let predicate = match kind.as_str() {
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
    Ok(predicate)
}

/// A predicate's `any_of` list: one or more non-empty strings. An empty list
/// would never match and an empty string would match everything, so both are
/// rejected as the policy errors they are.
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

fn parse_outcome(value: &Value, pointer: &str) -> Result<RuleOutcome, PolicyError> {
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
            Ok(RuleOutcome::Rewrite {
                target: ManagedTarget::new(target),
                reason,
            })
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
        other => Err(PolicyError::UnknownDecision {
            pointer: child_pointer(pointer, "kind"),
            arm: other.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Policy, PolicyError> {
        parse_policy(text)
    }

    #[test]
    fn an_empty_object_is_an_empty_policy() {
        assert_eq!(parse("{}").expect("parses"), Policy::default());
    }

    #[test]
    fn the_four_lists_and_tools_parse() {
        let policy = parse(
            r#"{
            "proxy": ["git", "gh"],
            "never_run": [{"id": "n1", "names": ["sqlite3"], "reason": "raw store access",
                "predicates": [{"kind": "operand", "equals": ["legion.db"]}]}],
            "ask": [{"id": "a1", "names": ["curl"], "reason": "reaches the network"}],
            "power_switches": [{"id": "p1", "names": ["legion"], "reason": "forced push",
                "predicates": [{"kind": "operand", "equals": ["push"]},
                               {"kind": "flag", "short": ["f"], "long": ["force"]}]}],
            "tools": {"WebSearch": {"rules": [{"id": "w1", "outcome": {"kind": "allow"}}]}}
        }"#,
        )
        .expect("parses");
        assert_eq!(policy.proxy, vec!["git".to_string(), "gh".to_string()]);
        assert_eq!(policy.never_run[0].id, "n1");
        assert_eq!(policy.never_run[0].reason, "raw store access");
        assert_eq!(policy.ask[0].names, vec!["curl".to_string()]);
        assert_eq!(policy.power_switches[0].predicates.len(), 2);
        assert_eq!(policy.tools[&ToolKind::WebSearch][0].id, "w1");
    }

    #[test]
    fn any_other_top_level_key_fails_to_parse() {
        for key in [
            "route",
            "no_go",
            "sym_jobs",
            "wrappers",
            "interpreters",
            "script_carriers",
            "global_options",
        ] {
            let text = format!("{{\"{key}\": []}}");
            assert_eq!(
                parse(&text),
                Err(PolicyError::UnknownField {
                    pointer: format!("/{key}"),
                    field: key.to_string(),
                }),
                "{key}"
            );
        }
    }

    #[test]
    fn a_bash_key_under_tools_fails_to_parse() {
        let err = parse(r#"{"tools": {"Bash": {"rules": []}}}"#).expect_err("Bash is not a tool");
        assert_eq!(
            err,
            PolicyError::UnknownToolKind {
                pointer: "/tools/Bash".to_string(),
                kind: "Bash".to_string(),
            }
        );
    }

    #[test]
    fn an_entry_without_a_name_or_a_reason_fails_to_parse() {
        assert_eq!(
            parse(r#"{"ask": [{"id": "a", "reason": "r"}]}"#),
            Err(PolicyError::EntryWithoutName {
                pointer: "/ask/0".to_string()
            })
        );
        assert_eq!(
            parse(r#"{"ask": [{"id": "a", "names": ["curl"], "reason": ""}]}"#),
            Err(PolicyError::EntryWithoutReason {
                pointer: "/ask/0".to_string()
            })
        );
        assert!(matches!(
            parse(r#"{"ask": [{"id": "a", "names": ["curl"]}]}"#),
            Err(PolicyError::MissingField { .. })
        ));
    }

    #[test]
    fn an_empty_proxy_name_fails_to_parse() {
        assert!(matches!(
            parse(r#"{"proxy": ["git", ""]}"#),
            Err(PolicyError::WrongType { .. })
        ));
    }

    #[test]
    fn ids_are_unique_across_every_list_and_rule() {
        let err = parse(
            r#"{"ask": [{"id": "x", "names": ["curl"], "reason": "r"}],
                "tools": {"WebSearch": {"rules": [{"id": "x", "outcome": {"kind": "allow"}}]}}}"#,
        )
        .expect_err("duplicate id");
        assert!(matches!(err, PolicyError::DuplicateRuleId { .. }));
    }

    #[test]
    fn a_sym_or_proxy_outcome_is_not_a_decision_arm() {
        for kind in ["sym", "proxy"] {
            let text = format!(
                r#"{{"tools": {{"Grep": {{"rules": [{{"id": "g", "outcome": {{"kind": "{kind}"}}}}]}}}}}}"#
            );
            assert!(
                matches!(parse(&text), Err(PolicyError::UnknownDecision { .. })),
                "{kind}"
            );
        }
    }

    #[test]
    fn never_run_entries_list_the_builtins_first() {
        let policy = parse(
            r#"{"never_run": [{"id": "extra", "names": ["sqlite3"], "reason": "raw store access"}]}"#,
        )
        .expect("parses");
        let entries = policy.never_run_entries();
        let builtins = nogo::builtin_no_go();
        assert_eq!(entries.len(), builtins.len() + 1);
        assert_eq!(entries[..builtins.len()], builtins[..]);
        assert_eq!(entries[builtins.len()].id, "extra");
    }

    #[test]
    fn the_shipped_policy_parses() {
        let text = include_str!("../../../plugin/legion-cmd/policy.json");
        parse(text).expect("the shipped policy parses");
    }
}
