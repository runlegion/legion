//! The Decision contract: route's closed return type (FR-CMD-001).
//!
//! `route` itself lives in a later slice. This module fixes what it will
//! return: exactly one [`Decision`] from a closed set of five arms, plus the
//! [`Facts`] it extracted while deciding, packaged as [`Routed`]. The input
//! side -- [`ToolCall`] and [`Context`] -- is fixed here too, so the later
//! `route` slice has a stable signature to implement against.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The exact `instead` text a no-go deny carries (FR-CMD-005): no command
/// replaces a no-go command, so every no-go deny names the same fixed text
/// rather than inventing one per call site.
pub const NO_GO_INSTEAD: &str = "none: this command never runs";

/// Errors raised when constructing a value this module validates.
///
/// Every arm exists because some field combination cannot be expressed by
/// the type alone -- an empty string is still a `String`, and an unknown
/// proxy reason is still a JSON string until it is checked.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ContractError {
    /// A [`DenyDetails`] was constructed with an empty reason (FR-CMD-005).
    #[error("deny reason cannot be empty")]
    EmptyDenyReason,

    /// A [`DenyDetails`] was constructed with an empty replacement command
    /// (FR-CMD-005).
    #[error("deny replacement command cannot be empty")]
    EmptyDenyInstead,

    /// A string outside the closed seven-member set was parsed as a
    /// [`ProxyReason`] (FR-CMD-004).
    #[error("unknown proxy reason: {0}")]
    UnknownProxyReason(String),

    /// An [`AskDetails`] was constructed with an empty question (FR-CMD-006).
    #[error("ask question cannot be empty")]
    EmptyAskQuestion,

    /// An [`AskDetails`] was constructed with an empty reason (FR-CMD-006).
    #[error("ask reason cannot be empty")]
    EmptyAskReason,
}

/// The only five outcomes `route` can return (FR-CMD-001). No sixth arm
/// exists, and an enum admits no value outside its declared variants, so a
/// `Decision` outside this set cannot be constructed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Run the command unchanged. `note` is an optional soft nudge delivered
    /// to the agent; it never alters the command (FR-CMD-002).
    Allow { note: Option<String> },

    /// Run legion's managed equivalent instead of the command as issued
    /// (FR-CMD-003). `route` names the target and the reason; it never
    /// builds the replacement command string -- the adapter does that from
    /// the [`Facts`] `route` returns alongside the decision, so the command
    /// is never parsed a second time.
    Rewrite {
        target: ManagedTarget,
        reason: String,
    },

    /// Run the command as-is, on the record, at zero coverage credit
    /// (FR-CMD-004). Carries exactly one reason from the closed
    /// [`ProxyReason`] set.
    Proxy { reason: ProxyReason },

    /// Refuse the command. Always carries a reason and the command to run
    /// instead (FR-CMD-005); see [`DenyDetails::new`] for the invariant.
    Deny(DenyDetails),

    /// Refuse the command and put a question, with a reason, to the agent
    /// first (FR-CMD-006). The agent drops the command or confirms it with
    /// a reason; the operator is prompted only when the matched policy
    /// entry marks the command as needing the operator, and only after the
    /// agent has confirmed, with the agent's reason attached. See
    /// [`AskDetails::new`] for the invariant.
    Ask(AskDetails),
}

impl Decision {
    /// Builds a [`Decision::Deny`], rejecting an empty reason or an empty
    /// replacement command (FR-CMD-005).
    pub fn deny(
        reason: impl Into<String>,
        instead: impl Into<String>,
    ) -> Result<Decision, ContractError> {
        Ok(Decision::Deny(DenyDetails::new(reason, instead)?))
    }

    /// Builds a [`Decision::Deny`] for a no-go match (FR-CMD-005): `instead`
    /// is always [`NO_GO_INSTEAD`], because no command replaces a no-go
    /// command. Building the no-go list itself is out of scope here.
    pub fn no_go(reason: impl Into<String>) -> Result<Decision, ContractError> {
        Ok(Decision::Deny(DenyDetails::no_go(reason)?))
    }

    /// Builds a [`Decision::Ask`], rejecting an empty question or an empty
    /// reason (FR-CMD-006).
    pub fn ask(
        question: impl Into<String>,
        reason: impl Into<String>,
    ) -> Result<Decision, ContractError> {
        Ok(Decision::Ask(AskDetails::new(question, reason)?))
    }
}

/// A denied command's reason and its replacement (FR-CMD-005).
///
/// Fields are private so the invariant -- neither string is empty -- holds
/// for every `DenyDetails` in existence, not just the ones built through
/// [`DenyDetails::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenyDetails {
    reason: String,
    instead: String,
}

impl DenyDetails {
    /// Rejects an empty `reason` or an empty `instead` (FR-CMD-005): a deny
    /// with nothing to tell the agent, or nothing to run in its place,
    /// leaves the agent unable to act on the refusal.
    pub fn new(
        reason: impl Into<String>,
        instead: impl Into<String>,
    ) -> Result<Self, ContractError> {
        let reason = reason.into();
        let instead = instead.into();
        if reason.is_empty() {
            return Err(ContractError::EmptyDenyReason);
        }
        if instead.is_empty() {
            return Err(ContractError::EmptyDenyInstead);
        }
        Ok(Self { reason, instead })
    }

    /// Builds the `DenyDetails` for a no-go match (FR-CMD-005): `instead` is
    /// always [`NO_GO_INSTEAD`], not a caller-supplied string, so every
    /// no-go deny carries the identical text. Only `reason` can be empty
    /// here, since `NO_GO_INSTEAD` is a fixed non-empty constant.
    pub fn no_go(reason: impl Into<String>) -> Result<Self, ContractError> {
        Self::new(reason, NO_GO_INSTEAD)
    }

    /// Why the command was refused.
    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// The command the agent should run instead.
    pub fn instead(&self) -> &str {
        &self.instead
    }
}

/// An asked question and the reason behind it (FR-CMD-006).
///
/// Fields are private so the invariant -- neither string is empty -- holds
/// for every `AskDetails` in existence, not just the ones built through
/// [`AskDetails::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskDetails {
    question: String,
    reason: String,
}

impl AskDetails {
    /// Rejects an empty `question` or an empty `reason` (FR-CMD-006): the
    /// agent cannot act on an ask that does not say what is being asked or
    /// why.
    pub fn new(
        question: impl Into<String>,
        reason: impl Into<String>,
    ) -> Result<Self, ContractError> {
        let question = question.into();
        let reason = reason.into();
        if question.is_empty() {
            return Err(ContractError::EmptyAskQuestion);
        }
        if reason.is_empty() {
            return Err(ContractError::EmptyAskReason);
        }
        Ok(Self { question, reason })
    }

    /// The question put to the agent.
    pub fn question(&self) -> &str {
        &self.question
    }

    /// Why the question is being asked.
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

/// The managed command a [`Decision::Rewrite`] points to (FR-CMD-003).
///
/// `route` only names the target; the adapter resolves it to an actual
/// command from the [`Facts`] `route` returns alongside the decision. The
/// set of valid target names is fixed by the routing policy in a later
/// slice, so this type carries a name and nothing more.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedTarget(String);

impl ManagedTarget {
    /// Names a managed target, e.g. `"legion sym def"`.
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// The target's name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The closed set of seven proxy reasons (FR-CMD-004). No other value can be
/// constructed: [`TryFrom<&str>`] and [`Deserialize`] both search [`ALL`]
/// via [`as_str`], so the wire name for each reason has one source.
///
/// [`ALL`]: ProxyReason::ALL
/// [`as_str`]: ProxyReason::as_str
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyReason {
    Binary,
    Checksum,
    MachineProtocol,
    CompleteLog,
    FullPatch,
    VerbatimSource,
    /// A command whose body `route` cannot see into, e.g. a managed binary
    /// inside a script file or interpreter program (FR-CMD-007).
    Opaque,
}

impl ProxyReason {
    /// Every member of the closed set, for exhaustive iteration and lookup.
    pub const ALL: [ProxyReason; 7] = [
        ProxyReason::Binary,
        ProxyReason::Checksum,
        ProxyReason::MachineProtocol,
        ProxyReason::CompleteLog,
        ProxyReason::FullPatch,
        ProxyReason::VerbatimSource,
        ProxyReason::Opaque,
    ];

    /// The reason's kebab-case wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            ProxyReason::Binary => "binary",
            ProxyReason::Checksum => "checksum",
            ProxyReason::MachineProtocol => "machine-protocol",
            ProxyReason::CompleteLog => "complete-log",
            ProxyReason::FullPatch => "full-patch",
            ProxyReason::VerbatimSource => "verbatim-source",
            ProxyReason::Opaque => "opaque",
        }
    }
}

impl TryFrom<&str> for ProxyReason {
    type Error = ContractError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        ProxyReason::ALL
            .into_iter()
            .find(|reason| reason.as_str() == value)
            .ok_or_else(|| ContractError::UnknownProxyReason(value.to_string()))
    }
}

impl Serialize for ProxyReason {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ProxyReason {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        ProxyReason::try_from(raw.as_str()).map_err(serde::de::Error::custom)
    }
}

/// A lookup the caller may or may not have performed. `NotFetched` is
/// distinct from `Empty`: the first means route has no information either
/// way, the second means the lookup ran and found nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Lookup {
    #[default]
    NotFetched,
    Empty,
    Found(Vec<String>),
}

/// The small caller-supplied context (FR-CMD-001): the repo, whether the
/// code index exists, the allow-list, and any recall or consult results
/// already fetched. Every field is passed in; nothing here is looked up by
/// legion-cmd itself (NFR-CMD-001).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Context {
    pub repo: Option<String>,
    pub index_exists: bool,
    pub allow_list: Vec<String>,
    pub recall: Lookup,
    pub consult: Lookup,
}

/// The complete command: the tool name and its inputs. A Bash call carries
/// the command string inside `input`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub tool: String,
    pub input: serde_json::Value,
}

/// What `route` extracted while deciding, so no caller parses the command a
/// second time (FR-CMD-003).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    pub paths: Vec<String>,
    pub verb: Option<String>,
    pub issue_numbers: Vec<u64>,
    pub keywords: Vec<String>,
}

/// What decided a command: the policy entry `route` matched, an unparsable
/// command, or an FR-CMD-016 default. The adapter (#1229) reads the operator
/// mark; the ledger (#1231, #1237) records the entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Deciding {
    /// A policy rule (or sym job) matched and produced the decision. `id` is
    /// its policy-unique id. `needs_operator` is the matched rule's operator
    /// mark (FR-CMD-006); route never copies it into an ask's own behavior in
    /// this issue -- #1237 adds the path that acts on it.
    Rule { id: String, needs_operator: bool },
    /// The command did not parse, so `route` returned ask (FR-CMD-006).
    ParseError,
    /// No rule matched, so an FR-CMD-016 default (allow, deny, or the
    /// empty-policy deny) produced the decision.
    Default,
}

/// `route`'s return value: exactly one [`Decision`], the [`Facts`] it
/// extracted (FR-CMD-001, FR-CMD-003), and the entry that decided the command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Routed {
    pub decision: Decision,
    pub facts: Facts,
    pub deciding: Deciding,
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- Decision is closed (FR-CMD-001) --------------------------------

    /// Matches every `Decision` arm with no wildcard. This compiles only
    /// while the set stays exactly {allow, rewrite, proxy, deny, ask} --
    /// adding or removing an arm breaks this test at compile time, which is
    /// the point: the closed set is enforced by the compiler, not by a
    /// runtime check.
    #[test]
    fn decision_arms_are_exhaustively_named() {
        let decisions = [
            Decision::Allow { note: None },
            Decision::Rewrite {
                target: ManagedTarget::new("legion sym def"),
                reason: "managed equivalent exists".to_string(),
            },
            Decision::Proxy {
                reason: ProxyReason::Binary,
            },
            Decision::deny("no managed equivalent", "run it manually").expect("valid deny"),
            Decision::ask("which repo?", "the command names no repo").expect("valid ask"),
        ];
        for decision in decisions {
            match decision {
                Decision::Allow { .. } => {}
                Decision::Rewrite { .. } => {}
                Decision::Proxy { .. } => {}
                Decision::Deny(_) => {}
                Decision::Ask(_) => {}
            }
        }
    }

    #[test]
    fn allow_note_is_carried_unchanged() {
        let decision = Decision::Allow {
            note: Some("consider legion sym def instead".to_string()),
        };
        match decision {
            Decision::Allow { note } => {
                assert_eq!(note.as_deref(), Some("consider legion sym def instead"));
            }
            _ => panic!("expected Allow"),
        }
    }

    #[test]
    fn rewrite_names_target_and_reason() {
        let decision = Decision::Rewrite {
            target: ManagedTarget::new("legion recall"),
            reason: "grep over reflections has a managed replacement".to_string(),
        };
        match decision {
            Decision::Rewrite { target, reason } => {
                assert_eq!(target.as_str(), "legion recall");
                assert_eq!(reason, "grep over reflections has a managed replacement");
            }
            _ => panic!("expected Rewrite"),
        }
    }

    // -- ProxyReason is closed (FR-CMD-004) ------------------------------

    /// Matches every `ProxyReason` arm with no wildcard, for the same reason
    /// `decision_arms_are_exhaustively_named` does: the closed set is
    /// enforced by the compiler.
    #[test]
    fn proxy_reasons_are_exhaustively_named() {
        for reason in ProxyReason::ALL {
            match reason {
                ProxyReason::Binary => {}
                ProxyReason::Checksum => {}
                ProxyReason::MachineProtocol => {}
                ProxyReason::CompleteLog => {}
                ProxyReason::FullPatch => {}
                ProxyReason::VerbatimSource => {}
                ProxyReason::Opaque => {}
            }
        }
    }

    #[test]
    fn proxy_reason_parses_from_its_kebab_case_name() {
        assert_eq!(ProxyReason::try_from("binary"), Ok(ProxyReason::Binary));
        assert_eq!(
            ProxyReason::try_from("machine-protocol"),
            Ok(ProxyReason::MachineProtocol)
        );
        assert_eq!(
            ProxyReason::try_from("verbatim-source"),
            Ok(ProxyReason::VerbatimSource)
        );
        assert_eq!(ProxyReason::try_from("opaque"), Ok(ProxyReason::Opaque));
    }

    #[test]
    fn proxy_reason_outside_the_closed_set_is_rejected() {
        let result = ProxyReason::try_from("network");
        assert_eq!(
            result,
            Err(ContractError::UnknownProxyReason("network".to_string()))
        );
    }

    #[test]
    fn proxy_reason_deserializes_from_json_string() {
        let reason: ProxyReason = serde_json::from_str("\"complete-log\"").expect("valid reason");
        assert_eq!(reason, ProxyReason::CompleteLog);
    }

    #[test]
    fn proxy_reason_deserialize_rejects_unknown_string() {
        let result: Result<ProxyReason, _> = serde_json::from_str("\"raw\"");
        let err = result.expect_err("\"raw\" is outside the closed set");
        // Confirms ContractError's Display text -- not a generic serde
        // message -- reaches the caller through `serde::de::Error::custom`.
        assert!(
            err.to_string().contains("unknown proxy reason: raw"),
            "unexpected error message: {err}"
        );
    }

    #[test]
    fn every_proxy_reason_round_trips_through_its_kebab_case_name() {
        for reason in ProxyReason::ALL {
            let json = serde_json::to_string(&reason).expect("serializes");
            assert_eq!(json, format!("\"{}\"", reason.as_str()));
            let parsed: ProxyReason = serde_json::from_str(&json).expect("valid reason");
            assert_eq!(parsed, reason);
        }
    }

    // -- Deny cannot be built without both fields (FR-CMD-005) -----------

    #[test]
    fn deny_details_rejects_empty_reason() {
        let result = DenyDetails::new("", "run it manually");
        assert_eq!(result, Err(ContractError::EmptyDenyReason));
    }

    #[test]
    fn deny_details_rejects_empty_instead() {
        let result = DenyDetails::new("no managed equivalent", "");
        assert_eq!(result, Err(ContractError::EmptyDenyInstead));
    }

    #[test]
    fn deny_details_holds_both_fields_when_valid() {
        let details = DenyDetails::new("no managed equivalent", "run it manually")
            .expect("both fields non-empty");
        assert_eq!(details.reason(), "no managed equivalent");
        assert_eq!(details.instead(), "run it manually");
    }

    #[test]
    fn decision_deny_constructor_rejects_empty_fields() {
        assert_eq!(
            Decision::deny("", "run it manually"),
            Err(ContractError::EmptyDenyReason)
        );
        assert_eq!(
            Decision::deny("no managed equivalent", ""),
            Err(ContractError::EmptyDenyInstead)
        );
    }

    // -- No-go deny always carries the fixed instead text (FR-CMD-005) ---

    #[test]
    fn deny_details_no_go_uses_the_fixed_instead_text() {
        let details = DenyDetails::no_go("matches a no-go rule").expect("non-empty reason");
        assert_eq!(details.reason(), "matches a no-go rule");
        assert_eq!(details.instead(), NO_GO_INSTEAD);
        assert_eq!(details.instead(), "none: this command never runs");
    }

    #[test]
    fn deny_details_no_go_rejects_empty_reason() {
        assert_eq!(DenyDetails::no_go(""), Err(ContractError::EmptyDenyReason));
    }

    #[test]
    fn decision_no_go_builds_a_deny_with_the_fixed_instead_text() {
        let decision = Decision::no_go("matches a no-go rule").expect("non-empty reason");
        match decision {
            Decision::Deny(details) => {
                assert_eq!(details.reason(), "matches a no-go rule");
                assert_eq!(details.instead(), NO_GO_INSTEAD);
            }
            _ => panic!("expected Deny"),
        }
    }

    // -- Ask cannot be built without both fields (FR-CMD-006) ------------

    #[test]
    fn ask_details_rejects_empty_question() {
        let result = AskDetails::new("", "the command names no repo");
        assert_eq!(result, Err(ContractError::EmptyAskQuestion));
    }

    #[test]
    fn ask_details_rejects_empty_reason() {
        let result = AskDetails::new("which repo?", "");
        assert_eq!(result, Err(ContractError::EmptyAskReason));
    }

    #[test]
    fn ask_details_holds_both_fields_when_valid() {
        let details =
            AskDetails::new("which repo?", "the command names no repo").expect("both non-empty");
        assert_eq!(details.question(), "which repo?");
        assert_eq!(details.reason(), "the command names no repo");
    }

    #[test]
    fn decision_ask_constructor_rejects_empty_fields() {
        assert_eq!(
            Decision::ask("", "the command names no repo"),
            Err(ContractError::EmptyAskQuestion)
        );
        assert_eq!(
            Decision::ask("which repo?", ""),
            Err(ContractError::EmptyAskReason)
        );
    }

    // -- Lookup distinguishes not-fetched from fetched-and-empty ---------

    #[test]
    fn lookup_defaults_to_not_fetched() {
        assert_eq!(Lookup::default(), Lookup::NotFetched);
    }

    #[test]
    fn lookup_empty_and_not_fetched_are_distinct() {
        assert_ne!(Lookup::Empty, Lookup::NotFetched);
    }

    // -- Context and Facts are plain data, nothing looked up internally --

    #[test]
    fn context_default_has_no_lookups_performed() {
        let ctx = Context::default();
        assert!(ctx.repo.is_none());
        assert!(!ctx.index_exists);
        assert!(ctx.allow_list.is_empty());
        assert_eq!(ctx.recall, Lookup::NotFetched);
        assert_eq!(ctx.consult, Lookup::NotFetched);
    }

    #[test]
    fn routed_carries_decision_and_facts_together() {
        let routed = Routed {
            decision: Decision::Allow { note: None },
            facts: Facts {
                verb: Some("view".to_string()),
                ..Facts::default()
            },
            deciding: Deciding::Default,
        };
        assert_eq!(routed.decision, Decision::Allow { note: None });
        assert_eq!(routed.facts.verb.as_deref(), Some("view"));
        assert_eq!(routed.deciding, Deciding::Default);
    }
}
