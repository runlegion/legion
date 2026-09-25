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

use crate::decision::{ManagedTarget, ProxyReason};

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
/// A rewrite carries its [`RewriteSpec`], so whether it fires is judged from
/// the invocation's arguments, never from the verb alone (FR-CMD-008).
/// [`RuleOutcome::Sym`] is not a sixth Decision: it is a routing action that
/// resolves to a Decision via the referenced [`SymJob`] -- a deny naming the
/// sym command (a lossless rewrite to it, per operator decision 01a0ab48, is
/// not built by any issue yet). It is carried here, rather than as a plain
/// deny, so the evaluator can give a sym job precedence over the
/// strictest-order fold (FR-CMD-007).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleOutcome {
    Allow {
        note: Option<String>,
    },
    Rewrite {
        spec: RewriteSpec,
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

/// A rewrite rule's target, the arguments it can translate, and what applies
/// when an invocation carries an argument it cannot (FR-CMD-008).
///
/// Losslessness is a property of the arguments, not the verb: a rewrite that
/// silently drops a flag runs something the agent did not ask for. So the
/// evaluator yields the rewrite only when every argument of the invocation
/// beyond the family's own subcommand words is covered by `translatable`;
/// otherwise `otherwise` decides.
///
/// Only a Bash rewrite declares `translatable` (and may declare `otherwise`).
/// A Fields rewrite (Agent, Task, ...) replaces the one `tool_input` field the
/// adapter patches and keeps every other field, so it is lossless by
/// construction: its call has no argument list, and the empty [`ArgSpec`] it
/// carries covers that empty list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewriteSpec {
    pub target: ManagedTarget,
    /// Flags and operand shapes the target expresses exactly.
    pub translatable: ArgSpec,
    pub otherwise: FallbackDecision,
}

/// The arguments a rewrite target expresses exactly (FR-CMD-008). Anything
/// not listed -- an undeclared option, a combined short-option cluster, an
/// operand beyond the declared positions or of the wrong shape -- makes the
/// invocation ineligible for the rewrite. An empty spec is a real
/// declaration: the target translates the family's subcommand words and
/// nothing more.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ArgSpec {
    /// Option words that take no value, matched as whole words (`--web`).
    pub flags: Vec<String>,
    /// Option words that take a value, either as the next argument
    /// (`--limit 5`) or joined with `=` (`--limit=5`).
    pub valued_flags: Vec<String>,
    /// The shapes of the operands the target carries, by position. An
    /// invocation may carry fewer operands than listed, never more.
    pub operands: Vec<OperandShape>,
}

/// The shape one positional operand must have to be translatable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperandShape {
    /// Any word.
    Any,
    /// A non-negative decimal integer (an issue or PR number).
    Integer,
}

impl OperandShape {
    /// Every member, so the parser rejects a wire name outside the set.
    pub const ALL: [OperandShape; 2] = [OperandShape::Any, OperandShape::Integer];

    /// The shape's wire name, as it appears in a `translatable.operands` list.
    pub fn as_str(self) -> &'static str {
        match self {
            OperandShape::Any => "any",
            OperandShape::Integer => "integer",
        }
    }
}

/// What a rewrite rule yields when the invocation carries an argument its
/// [`ArgSpec`] does not cover (FR-CMD-008). The default is a deny naming the
/// untranslatable argument and the managed command to run instead
/// (FR-CMD-005); a policy entry may name another arm instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FallbackDecision {
    /// Deny, with a reason naming the untranslatable argument and the target
    /// as the command to run instead. Written `{"kind": "deny"}`, or implied
    /// when the rule declares no `otherwise`.
    DenyNamingTarget,
    Allow {
        note: Option<String>,
    },
    Proxy {
        reason: ProxyReason,
    },
    Ask {
        question: String,
        reason: String,
        /// Carried like [`RuleOutcome::Ask`]'s mark; route never copies it
        /// into its output (FR-CMD-006).
        needs_operator: bool,
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
///
/// The wrapper's own words before the payload are declared here as data
/// (#1286): its valueless options, its options that take a value, and how
/// many leading operands it takes. [`Wrapper::payload_start`] consumes them by
/// that declaration; an option the declaration does not name is a word route
/// cannot account for, so the invocation is proxied opaque rather than read.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Wrapper {
    pub binary: String,
    /// A word that must follow the binary for it to wrap a command, e.g.
    /// `exec`/`dlx` for `pnpm`/`npm`/`yarn`. A bare `pnpm <name>` carries no
    /// required word and is an ordinary invocation, not a runner.
    pub required_subcommand: Option<String>,
    /// The wrapper's own options that take no value, e.g. `--foreground` for
    /// `timeout`. A single-dash one-letter entry also matches inside a
    /// clustered word (`-0r`).
    pub flags: Vec<String>,
    /// The wrapper's own options that take a value, e.g. `-s`/`--signal` for
    /// `timeout`. The value is the next word, or attached (`-oL`,
    /// `--signal=KILL`).
    pub value_options: Vec<String>,
    /// How many operands the wrapper takes after its options and before the
    /// payload, e.g. 1 for `timeout DURATION`.
    pub operands: usize,
}

impl Wrapper {
    /// Whether this wrapper claims an invocation of its binary with `args`.
    /// A wrapper with no required subcommand claims every one. One with a
    /// required subcommand claims it when that word follows the declared
    /// options, since a runner's options precede its subcommand
    /// (`pnpm -r exec`). When an option the wrapper does not declare comes
    /// first, route cannot tell where the subcommand sits, so the wrapper
    /// claims the invocation if the word appears at all and
    /// [`Wrapper::payload_start`] then refuses it -- proxied opaque rather
    /// than read as an ordinary invocation (#1286).
    pub fn wraps(&self, args: &[String]) -> bool {
        let Some(subcommand) = &self.required_subcommand else {
            return true;
        };
        match self.options_end(args, 0) {
            Some(index) => word_at(args, index) == Some(subcommand.as_str()),
            None => args
                .iter()
                .any(|a| crate::evaluate::dequote_outer(a) == subcommand),
        }
    }

    /// The index in `args` where the wrapped command begins, or `None` when
    /// the wrapper's own words cannot be consumed by its declaration: an
    /// option it does not declare, a value option with no value, a missing
    /// required subcommand, or fewer operands than it takes (#1286).
    ///
    /// Declared options are read before and after the required subcommand,
    /// then one `--`, then the declared operands. An index equal to
    /// `args.len()` means the wrapper wraps no command.
    pub fn payload_start(&self, args: &[String]) -> Option<usize> {
        let mut index = self.options_end(args, 0)?;
        if let Some(subcommand) = &self.required_subcommand {
            if word_at(args, index) != Some(subcommand.as_str()) {
                return None;
            }
            index = self.options_end(args, index + 1)?;
        }
        if word_at(args, index) == Some("--") {
            index += 1;
        }
        let start = index + self.operands;
        (start <= args.len()).then_some(start)
    }

    /// The index of the first word from `index` on that is not one of the
    /// wrapper's options, or `None` when an option there is one it does not
    /// declare or lacks its value. Options are read getopt-style; a `--` stops
    /// the read and is left for the caller. Each word is compared as the shell
    /// sees it (one outer quote pair removed), so a quoted `"-u"` is still an
    /// option.
    fn options_end(&self, args: &[String], mut index: usize) -> Option<usize> {
        while let Some(word) = word_at(args, index) {
            // A lone `-` is an operand (stdin) unless the wrapper declares it
            // an option, as `env` does.
            let is_option =
                word != "--" && word.starts_with('-') && (word != "-" || self.declares_flag(word));
            if !is_option {
                break;
            }
            let consumed = self.option_words(word)?;
            if index + consumed > args.len() {
                return None;
            }
            index += consumed;
        }
        Some(index)
    }

    /// How many words the option `word` spans (1, or 2 when its value is the
    /// next word), or `None` when the declaration does not name it.
    fn option_words(&self, word: &str) -> Option<usize> {
        if self.declares_flag(word) {
            return Some(1);
        }
        if self.declares_value_option(word) {
            return Some(2);
        }
        if let Some(long) = word.strip_prefix("--") {
            // `--name=value` is one word when `--name` takes a value.
            let (name, _) = long.split_once('=')?;
            return self
                .declares_value_option(&format!("--{name}"))
                .then_some(1);
        }
        // A short cluster (`-0r`, `-oL`, `-uroot`): every letter is a
        // declared flag until one takes a value, which is the rest of the
        // word when anything follows it and the next word otherwise.
        let cluster = word.strip_prefix('-')?;
        for (pos, letter) in cluster.char_indices() {
            let option = format!("-{letter}");
            if self.declares_flag(&option) {
                continue;
            }
            if !self.declares_value_option(&option) {
                return None;
            }
            let attached = pos + letter.len_utf8() < cluster.len();
            return Some(if attached { 1 } else { 2 });
        }
        Some(1)
    }

    fn declares_flag(&self, word: &str) -> bool {
        self.flags.iter().any(|f| f == word)
    }

    fn declares_value_option(&self, word: &str) -> bool {
        self.value_options.iter().any(|o| o == word)
    }
}

/// The word at `index` as the shell sees it, one outer quote pair removed.
fn word_at(args: &[String], index: usize) -> Option<&str> {
    args.get(index).map(|a| crate::evaluate::dequote_outer(a))
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
    /// one. A wrapper with a required subcommand matches only when
    /// [`Wrapper::wraps`] finds that word.
    pub fn matching_wrapper(&self, binary: &str, args: &[String]) -> Option<&Wrapper> {
        self.wrappers
            .iter()
            .find(|w| w.binary == binary && w.wraps(args))
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
    // `route` holds the adapter's own settings (FR-CMD-009's decision
    // deadline, read from the same file so the policy stays one document).
    // It is not routing data: this crate accepts the key and never reads it.
    // The adapter (`src/cmd/config.rs`) parses and validates it, strictly, so
    // a typo inside it is still an error and never a silent default.
    check_known_keys(
        root,
        "",
        &[
            "tools",
            "sym_jobs",
            "wrappers",
            "interpreters",
            "script_carriers",
            "route",
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
        kind,
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
    tool: ToolKind,
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
            // Only a Bash call carries an argument list, so only a Bash
            // rewrite declares which arguments translate. A Fields rewrite
            // patches one field and keeps the rest -- lossless by
            // construction -- and a `translatable` there would be a key that
            // never takes effect, so it is rejected like any unknown field.
            let is_bash = tool == ToolKind::Bash;
            let known: &[&str] = if is_bash {
                &["kind", "target", "reason", "translatable", "otherwise"]
            } else {
                &["kind", "target", "reason"]
            };
            check_known_keys(map, pointer, known)?;
            let target = require_string(map, "target", pointer)?;
            let reason = require_string(map, "reason", pointer)?;
            if target.is_empty() || reason.is_empty() {
                return Err(PolicyError::IncompleteRewrite {
                    pointer: root_pointer(pointer),
                });
            }
            let (translatable, otherwise) = if is_bash {
                // A Bash rewrite with no declared argument coverage is the
                // verb-only judgment FR-CMD-008 forbids.
                let translatable = parse_arg_spec(
                    require(map, "translatable", pointer)?,
                    &child_pointer(pointer, "translatable"),
                )?;
                let otherwise = match map.get("otherwise") {
                    Some(fallback) => {
                        parse_fallback(fallback, &child_pointer(pointer, "otherwise"))?
                    }
                    None => FallbackDecision::DenyNamingTarget,
                };
                (translatable, otherwise)
            } else {
                (ArgSpec::default(), FallbackDecision::DenyNamingTarget)
            };
            Ok(RuleOutcome::Rewrite {
                spec: RewriteSpec {
                    target: ManagedTarget::new(target),
                    translatable,
                    otherwise,
                },
                reason,
            })
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

/// A Bash rewrite's `translatable` declaration. Every key is optional, so
/// `{}` declares that the target translates the family's subcommand words and
/// nothing more. A declared flag must be an option word (it starts with `-`):
/// the evaluator only ever compares option words against flags, so any other
/// entry would be a declaration that never takes effect.
fn parse_arg_spec(value: &Value, pointer: &str) -> Result<ArgSpec, PolicyError> {
    let map = as_object(value, pointer)?;
    check_known_keys(map, pointer, &["flags", "valued_flags", "operands"])?;
    let option_words = |key: &str| -> Result<Vec<String>, PolicyError> {
        let key_pointer = child_pointer(pointer, key);
        let words = match map.get(key) {
            Some(list) => parse_string_array(list, &key_pointer)?,
            None => Vec::new(),
        };
        if let Some(index) = words
            .iter()
            .position(|w| w.len() < 2 || !w.starts_with('-'))
        {
            return Err(PolicyError::WrongType {
                pointer: child_pointer(&key_pointer, &index.to_string()),
                expected: "an option word starting with '-'".to_string(),
            });
        }
        Ok(words)
    };
    let flags = option_words("flags")?;
    let valued_flags = option_words("valued_flags")?;

    let operands_pointer = child_pointer(pointer, "operands");
    let operand_names = match map.get("operands") {
        Some(list) => parse_string_array(list, &operands_pointer)?,
        None => Vec::new(),
    };
    let mut operands = Vec::with_capacity(operand_names.len());
    for (index, name) in operand_names.iter().enumerate() {
        let shape = OperandShape::ALL
            .into_iter()
            .find(|s| s.as_str() == name)
            .ok_or_else(|| PolicyError::WrongType {
                pointer: child_pointer(&operands_pointer, &index.to_string()),
                expected: "an operand shape: 'any' or 'integer'".to_string(),
            })?;
        operands.push(shape);
    }

    Ok(ArgSpec {
        flags,
        valued_flags,
        operands,
    })
}

/// A Bash rewrite's `otherwise` arm. `{"kind": "deny"}` is the default deny
/// naming the untranslatable argument and the target, and takes no fields: a
/// hand-written reason could not name the argument. allow, proxy and ask are
/// parsed exactly as rule outcomes are. A rewrite or sym fallback is refused:
/// the fallback applies precisely because the rewrite cannot.
fn parse_fallback(value: &Value, pointer: &str) -> Result<FallbackDecision, PolicyError> {
    let map = as_object(value, pointer)?;
    let kind = require_string(map, "kind", pointer)?;
    let not_a_fallback = || PolicyError::WrongType {
        pointer: child_pointer(pointer, "kind"),
        expected: "a fallback arm: deny, allow, proxy or ask".to_string(),
    };
    match kind.as_str() {
        "deny" => {
            check_known_keys(map, pointer, &["kind"])?;
            Ok(FallbackDecision::DenyNamingTarget)
        }
        "rewrite" | "sym" => Err(not_a_fallback()),
        _ => match parse_outcome(value, pointer, ToolKind::Bash, &[])? {
            RuleOutcome::Allow { note } => Ok(FallbackDecision::Allow { note }),
            RuleOutcome::Proxy { reason } => Ok(FallbackDecision::Proxy { reason }),
            RuleOutcome::Ask {
                question,
                reason,
                needs_operator,
            } => Ok(FallbackDecision::Ask {
                question,
                reason,
                needs_operator,
            }),
            // "deny", "rewrite" and "sym" are handled above; parse_outcome
            // yields nothing else for the remaining kinds.
            RuleOutcome::Deny { .. } | RuleOutcome::Rewrite { .. } | RuleOutcome::Sym { .. } => {
                Err(not_a_fallback())
            }
        },
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
        check_known_keys(
            map,
            &wrapper_pointer,
            &[
                "binary",
                "required_subcommand",
                "flags",
                "value_options",
                "operands",
            ],
        )?;
        let binary = require_string(map, "binary", &wrapper_pointer)?;
        let required_subcommand = match map.get("required_subcommand") {
            Some(sub) => Some(as_string(
                sub,
                &child_pointer(&wrapper_pointer, "required_subcommand"),
            )?),
            None => None,
        };
        let optional_strings = |key: &str| match map.get(key) {
            Some(value) => parse_string_array(value, &child_pointer(&wrapper_pointer, key)),
            None => Ok(Vec::new()),
        };
        let flags = optional_strings("flags")?;
        let value_options = optional_strings("value_options")?;
        let operands = match map.get("operands") {
            Some(value) => value
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| PolicyError::WrongType {
                    pointer: child_pointer(&wrapper_pointer, "operands"),
                    expected: "a non-negative integer".to_string(),
                })?,
            None => 0,
        };
        wrappers.push(Wrapper {
            binary,
            required_subcommand,
            flags,
            value_options,
            operands,
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
    fn the_route_settings_key_is_accepted_and_not_interpreted() {
        // `route` carries the adapter's settings (#1229); the policy parser
        // accepts the key so one file holds both, and reads nothing from it.
        let text = r#"{"route": {"deadline_ms": 250},
            "tools": {"Bash": {"families": {"gh": {"rules": [
                {"id": "x", "outcome": {"kind": "allow"}}
            ]}}}}}"#;
        let policy = parse_policy(text).expect("route key is a known root key");
        assert!(!policy.is_empty());
        assert_eq!(
            policy,
            parse_policy(&text.replacen(r#""route": {"deadline_ms": 250},"#, "", 1))
                .expect("valid")
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

    // -- rewrite specs (FR-CMD-008) -------------------------------------------

    #[test]
    fn a_bash_rewrite_without_a_translatable_declaration_is_rejected() {
        // Coverage declared by nobody is the verb-only judgment FR-CMD-008
        // forbids; the error names the rule's pointer.
        let text = r#"{"tools": {"Bash": {"families": {"gh issue list": {"rules": [
            {"id": "x", "outcome": {"kind": "rewrite", "target": "legion issue list", "reason": "r"}}
        ]}}}}}"#;
        let err = parse_policy(text).expect_err("rewrite without translatable");
        assert_eq!(
            err,
            PolicyError::MissingField {
                pointer: "/tools/Bash/families/gh issue list/rules/0/outcome".to_string(),
                field: "translatable".to_string(),
            }
        );
    }

    #[test]
    fn a_bash_rewrite_parses_its_spec_and_defaults_its_fallback_to_the_named_deny() {
        let text = r#"{"tools": {"Bash": {"families": {"gh pr view": {"rules": [
            {"id": "x", "outcome": {"kind": "rewrite", "target": "legion pr view", "reason": "r",
             "translatable": {"flags": ["--web"], "valued_flags": ["--json"], "operands": ["integer", "any"]}}}
        ]}}}}}"#;
        let policy = parse_policy(text).expect("valid");
        let ToolRules::Bash { families } = &policy.tools[&ToolKind::Bash] else {
            panic!("expected Bash rules");
        };
        assert_eq!(
            families["gh pr view"].rules[0].outcome,
            RuleOutcome::Rewrite {
                spec: RewriteSpec {
                    target: ManagedTarget::new("legion pr view"),
                    translatable: ArgSpec {
                        flags: vec!["--web".to_string()],
                        valued_flags: vec!["--json".to_string()],
                        operands: vec![OperandShape::Integer, OperandShape::Any],
                    },
                    otherwise: FallbackDecision::DenyNamingTarget,
                },
                reason: "r".to_string(),
            }
        );
    }

    #[test]
    fn an_empty_translatable_declaration_is_accepted() {
        // `{}` declares that nothing beyond the family's words translates --
        // the exact-arguments rewrite, a real declaration.
        let text = r#"{"tools": {"Bash": {"families": {"gh issue list": {"rules": [
            {"id": "x", "outcome": {"kind": "rewrite", "target": "legion issue list", "reason": "r",
             "translatable": {}}}
        ]}}}}}"#;
        parse_policy(text).expect("an empty ArgSpec is a declaration");
    }

    #[test]
    fn each_fallback_arm_parses() {
        let parse_otherwise = |otherwise: &str| {
            let text = format!(
                r#"{{"tools": {{"Bash": {{"families": {{"gh": {{"rules": [
                {{"id": "x", "outcome": {{"kind": "rewrite", "target": "t", "reason": "r",
                 "translatable": {{}}, "otherwise": {otherwise}}}}}
            ]}}}}}}}}}}"#
            );
            let policy = parse_policy(&text).expect("valid");
            let ToolRules::Bash { families } = &policy.tools[&ToolKind::Bash] else {
                panic!("expected Bash rules");
            };
            match &families["gh"].rules[0].outcome {
                RuleOutcome::Rewrite { spec, .. } => spec.otherwise.clone(),
                other => panic!("expected rewrite, got {other:?}"),
            }
        };
        assert_eq!(
            parse_otherwise(r#"{"kind": "deny"}"#),
            FallbackDecision::DenyNamingTarget
        );
        assert_eq!(
            parse_otherwise(r#"{"kind": "allow", "note": "n"}"#),
            FallbackDecision::Allow {
                note: Some("n".to_string())
            }
        );
        assert_eq!(
            parse_otherwise(r#"{"kind": "proxy", "reason": "binary"}"#),
            FallbackDecision::Proxy {
                reason: ProxyReason::Binary
            }
        );
        assert_eq!(
            parse_otherwise(
                r#"{"kind": "ask", "question": "q", "reason": "r", "needs_operator": true}"#
            ),
            FallbackDecision::Ask {
                question: "q".to_string(),
                reason: "r".to_string(),
                needs_operator: true,
            }
        );
    }

    #[test]
    fn a_rewrite_or_sym_fallback_and_a_deny_fallback_with_text_are_rejected() {
        let with_otherwise = |otherwise: &str| {
            format!(
                r#"{{"sym_jobs": [{{"id": "j", "sym_command": "legion sym x"}}],
                "tools": {{"Bash": {{"families": {{"gh": {{"rules": [
                {{"id": "x", "outcome": {{"kind": "rewrite", "target": "t", "reason": "r",
                 "translatable": {{}}, "otherwise": {otherwise}}}}}
            ]}}}}}}}}}}"#
            )
        };
        for arm in [
            r#"{"kind": "rewrite", "target": "t", "reason": "r"}"#,
            r#"{"kind": "sym", "job": "j"}"#,
        ] {
            assert_eq!(
                parse_policy(&with_otherwise(arm)).expect_err("not a fallback arm"),
                PolicyError::WrongType {
                    pointer: "/tools/Bash/families/gh/rules/0/outcome/otherwise/kind".to_string(),
                    expected: "a fallback arm: deny, allow, proxy or ask".to_string(),
                },
                "{arm}"
            );
        }
        // A hand-written deny reason could not name the argument. One extra
        // key only: which of several is reported first depends on serde_json's
        // map order, which feature unification can change.
        assert_eq!(
            parse_policy(&with_otherwise(r#"{"kind": "deny", "instead": "i"}"#))
                .expect_err("deny fallback takes no fields"),
            PolicyError::UnknownField {
                pointer: "/tools/Bash/families/gh/rules/0/outcome/otherwise/instead".to_string(),
                field: "instead".to_string(),
            }
        );
    }

    #[test]
    fn a_fields_rewrite_parses_without_translatable_and_rejects_one() {
        // Operator, 2026-09-23: a Fields rewrite is lossless by construction,
        // so it needs no declaration -- and a declaration there is a key that
        // never takes effect.
        let ok = r#"{"tools": {"Agent": {"rules": [
            {"id": "x", "outcome": {"kind": "rewrite", "target": "legion:legion-explore", "reason": "r"}}
        ]}}}"#;
        let policy = parse_policy(ok).expect("a Fields rewrite needs no translatable");
        let ToolRules::Fields { rules } = &policy.tools[&ToolKind::Agent] else {
            panic!("expected Fields rules");
        };
        assert_eq!(
            rules[0].outcome,
            RuleOutcome::Rewrite {
                spec: RewriteSpec {
                    target: ManagedTarget::new("legion:legion-explore"),
                    translatable: ArgSpec::default(),
                    otherwise: FallbackDecision::DenyNamingTarget,
                },
                reason: "r".to_string(),
            }
        );

        let declared = r#"{"tools": {"Task": {"rules": [
            {"id": "x", "outcome": {"kind": "rewrite", "target": "t", "reason": "r", "translatable": {}}}
        ]}}}"#;
        assert_eq!(
            parse_policy(declared).expect_err("translatable under a Fields tool"),
            PolicyError::UnknownField {
                pointer: "/tools/Task/rules/0/outcome/translatable".to_string(),
                field: "translatable".to_string(),
            }
        );
    }

    #[test]
    fn a_malformed_translatable_declaration_names_its_pointer() {
        let with_spec = |spec: &str| {
            format!(
                r#"{{"tools": {{"Bash": {{"families": {{"gh": {{"rules": [
                {{"id": "x", "outcome": {{"kind": "rewrite", "target": "t", "reason": "r",
                 "translatable": {spec}}}}}
            ]}}}}}}}}}}"#
            )
        };
        let base = "/tools/Bash/families/gh/rules/0/outcome/translatable";
        assert_eq!(
            parse_policy(&with_spec(r#"{"operands": ["integer", "path"]}"#)).expect_err("shape"),
            PolicyError::WrongType {
                pointer: format!("{base}/operands/1"),
                expected: "an operand shape: 'any' or 'integer'".to_string(),
            }
        );
        assert_eq!(
            parse_policy(&with_spec(r#"{"flags": ["--web", "web"]}"#)).expect_err("not a flag"),
            PolicyError::WrongType {
                pointer: format!("{base}/flags/1"),
                expected: "an option word starting with '-'".to_string(),
            }
        );
        assert_eq!(
            parse_policy(&with_spec(r#"{"valued_flags": ["-"]}"#)).expect_err("bare dash"),
            PolicyError::WrongType {
                pointer: format!("{base}/valued_flags/0"),
                expected: "an option word starting with '-'".to_string(),
            }
        );
        assert_eq!(
            parse_policy(&with_spec(r#"{"flag": ["--web"]}"#)).expect_err("typo'd key"),
            PolicyError::UnknownField {
                pointer: format!("{base}/flag"),
                field: "flag".to_string(),
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
                ..Wrapper::default()
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

    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn wrapper_arguments_parse_from_the_declaration() {
        let text = r#"{"wrappers": [{"binary": "timeout", "flags": ["-v"],
            "value_options": ["-s"], "operands": 1}]}"#;
        let policy = parse_policy(text).expect("valid");
        assert_eq!(
            policy.wrappers[0],
            Wrapper {
                binary: "timeout".to_string(),
                required_subcommand: None,
                flags: vec!["-v".to_string()],
                value_options: vec!["-s".to_string()],
                operands: 1,
            }
        );
    }

    #[test]
    fn wrapper_arguments_of_the_wrong_type_are_rejected() {
        for (text, pointer, expected) in [
            (
                r#"{"wrappers": [{"binary": "timeout", "operands": -1}]}"#,
                "/wrappers/0/operands",
                "a non-negative integer",
            ),
            (
                r#"{"wrappers": [{"binary": "timeout", "operands": "1"}]}"#,
                "/wrappers/0/operands",
                "a non-negative integer",
            ),
            (
                r#"{"wrappers": [{"binary": "sudo", "flags": "-E"}]}"#,
                "/wrappers/0/flags",
                "an array",
            ),
            (
                r#"{"wrappers": [{"binary": "sudo", "value_options": [1]}]}"#,
                "/wrappers/0/value_options/0",
                "a string",
            ),
        ] {
            assert_eq!(
                parse_policy(text).expect_err("wrong type"),
                PolicyError::WrongType {
                    pointer: pointer.to_string(),
                    expected: expected.to_string(),
                },
                "{text}"
            );
        }
    }

    #[test]
    fn payload_start_consumes_the_declared_words_getopt_style() {
        let timeout = Wrapper {
            binary: "timeout".to_string(),
            flags: vec!["-v".to_string(), "--foreground".to_string()],
            value_options: vec!["-s".to_string(), "--signal".to_string()],
            operands: 1,
            ..Wrapper::default()
        };
        for (args, start) in [
            ("5 cmd", 1),
            ("-s KILL 5 cmd", 3),
            ("-sKILL 5 cmd", 2),
            ("--signal=KILL 5 cmd", 2),
            ("--signal KILL --foreground 5 cmd", 4),
            ("-vs KILL 5 cmd", 3),
            ("-- 5 cmd", 2),
            // Options stop at the first operand: `-v` here is the command's.
            ("5 cmd -v", 1),
            // Consumed to the end: the wrapper wraps no command.
            ("5", 1),
        ] {
            assert_eq!(timeout.payload_start(&words(args)), Some(start), "{args}");
        }
        for args in [
            "",
            "-s",
            "-x 5 cmd",
            "--bogus 5 cmd",
            "--foreground=1 5 cmd",
            "-vx 5",
        ] {
            assert_eq!(timeout.payload_start(&words(args)), None, "{args}");
        }
    }

    #[test]
    fn payload_start_skips_the_required_subcommand_and_reads_a_lone_dash_by_declaration() {
        let pnpm_exec = Wrapper {
            binary: "pnpm".to_string(),
            required_subcommand: Some("exec".to_string()),
            flags: vec!["-r".to_string()],
            value_options: vec!["-C".to_string()],
            ..Wrapper::default()
        };
        // A runner's options precede its subcommand.
        for (args, start) in [
            ("exec eslint", 1),
            ("-r exec eslint", 2),
            ("-C pkg -r exec eslint", 4),
            ("exec -- eslint", 2),
        ] {
            assert!(pnpm_exec.wraps(&words(args)), "{args}");
            assert_eq!(pnpm_exec.payload_start(&words(args)), Some(start), "{args}");
        }
        // Not the runner: the subcommand is not the word after the options.
        for args in ["grep", "-r install", "run exec"] {
            assert!(!pnpm_exec.wraps(&words(args)), "{args}");
        }
        // An undeclared option before the subcommand: claimed, then refused.
        assert!(pnpm_exec.wraps(&words("--bogus exec eslint")));
        assert_eq!(pnpm_exec.payload_start(&words("--bogus exec eslint")), None);

        let env = Wrapper {
            binary: "env".to_string(),
            flags: vec!["-".to_string()],
            ..Wrapper::default()
        };
        assert_eq!(env.payload_start(&words("- grep foo")), Some(1));
        // Undeclared, a lone `-` is an operand, not an option.
        assert_eq!(Wrapper::default().payload_start(&words("- grep")), Some(0));
    }
}
