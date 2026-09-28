//! The Decision contract: route's closed return type (FR-CMD-001).
//!
//! This module fixes what [`crate::route`] returns: exactly one [`Decision`]
//! from a closed set of four arms, the [`Facts`] it extracted while deciding,
//! and the [`Deciding`] entry that produced the decision, packaged as
//! [`Routed`]. The input side -- [`ToolCall`] and [`Context`] -- is fixed here
//! too. `route` (in `crate::route`) consumes exactly this signature.

use std::collections::HashMap;

use serde::Serialize;

use crate::nogo::CommandKey;

/// The exact `instead` text every deny of a Bash command carries: the router
/// never tells the agent to run a different command (#1337), so a refused
/// command names no replacement.
pub const NO_GO_INSTEAD: &str = "none: this command never runs";

/// Errors raised when constructing a value this module validates.
///
/// Every arm exists because some field combination cannot be expressed by
/// the type alone -- an empty string is still a `String`.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ContractError {
    /// A [`DenyDetails`] was constructed with an empty reason (FR-CMD-005).
    #[error("deny reason cannot be empty")]
    EmptyDenyReason,

    /// A [`DenyDetails`] was constructed with an empty replacement command
    /// (FR-CMD-005).
    #[error("deny replacement command cannot be empty")]
    EmptyDenyInstead,

    /// An [`AskDetails`] was constructed with an empty question (FR-CMD-006).
    #[error("ask question cannot be empty")]
    EmptyAskQuestion,

    /// An [`AskDetails`] was constructed with an empty reason (FR-CMD-006).
    #[error("ask reason cannot be empty")]
    EmptyAskReason,
}

/// The only four outcomes `route` can return (FR-CMD-001). An enum admits no
/// value outside its declared variants, so a `Decision` outside this set
/// cannot be constructed.
///
/// Serializes tagged by `kind` (`{"kind": "deny", "reason": ..., "instead":
/// ...}`), the shape a policy rule's `outcome` already uses, so `legion
/// cmd-check --json` (#1230) can print it for scripts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Decision {
    /// Run the command unchanged. `note` is an optional soft nudge a
    /// non-Bash tool rule delivers to the agent; it never alters the call
    /// (FR-CMD-002). A Bash command that runs untouched carries none.
    Allow { note: Option<String> },

    /// Run a changed call in place of the one issued (FR-CMD-003). For a
    /// Bash command the change is `legion ` inserted before each proxied
    /// name, and the command to run is [`Facts::rewritten`]; `target` names
    /// legion. For a non-Bash tool rule, `target` is the value the rule puts
    /// in place of the call's rewritable field.
    Rewrite {
        target: ManagedTarget,
        reason: String,
    },

    /// Refuse the command. Always carries a reason (FR-CMD-005); see
    /// [`DenyDetails::new`] for the invariant.
    Deny(DenyDetails),

    /// Refuse the command and put a question, with a reason, to the agent
    /// first (FR-CMD-006). The agent drops the command or confirms it with
    /// a reason; the operator is prompted only when the matched entry marks
    /// the command as needing the operator, and only after the agent has
    /// confirmed, with the agent's reason attached. See [`AskDetails::new`]
    /// for the invariant.
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

    /// Builds a [`Decision::Deny`] for a refused Bash command (FR-CMD-005):
    /// `instead` is always [`NO_GO_INSTEAD`], because the router names no
    /// command to run in place of a refused one.
    pub fn no_go(reason: impl Into<String>) -> Result<Decision, ContractError> {
        Ok(Decision::Deny(DenyDetails::no_go(reason)?))
    }

    /// The deny for a refused Bash command whose reason is fixed text the
    /// router wrote. Infallible: an empty `reason` is replaced by a fixed
    /// non-empty one, so the invariant [`DenyDetails::new`] checks holds
    /// without a fallible path and a refusal can never degrade into any
    /// other arm.
    pub fn refuse(reason: impl Into<String>) -> Decision {
        Decision::fixed_deny(reason, NO_GO_INSTEAD)
    }

    /// A deny from fixed text: either string, if empty, is replaced by a
    /// fixed non-empty one so the construction cannot fail.
    pub(crate) fn fixed_deny(reason: impl Into<String>, instead: impl Into<String>) -> Decision {
        let reason: String = reason.into();
        let instead: String = instead.into();
        Decision::Deny(DenyDetails {
            reason: if reason.is_empty() {
                "legion-cmd refused this command".to_string()
            } else {
                reason
            },
            instead: if instead.is_empty() {
                NO_GO_INSTEAD.to_string()
            } else {
                instead
            },
        })
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DenyDetails {
    reason: String,
    instead: String,
}

impl DenyDetails {
    /// Rejects an empty `reason` or an empty `instead` (FR-CMD-005): a deny
    /// with nothing to tell the agent leaves the agent unable to act on the
    /// refusal.
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

    /// Builds the `DenyDetails` for a refused Bash command (FR-CMD-005):
    /// `instead` is always [`NO_GO_INSTEAD`], not a caller-supplied string.
    pub fn no_go(reason: impl Into<String>) -> Result<Self, ContractError> {
        Self::new(reason, NO_GO_INSTEAD)
    }

    /// Why the command was refused.
    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// What runs instead: [`NO_GO_INSTEAD`] for every Bash refusal.
    pub fn instead(&self) -> &str {
        &self.instead
    }
}

/// An asked question and the reason behind it (FR-CMD-006).
///
/// Fields are private so the invariant -- neither string is empty -- holds
/// for every `AskDetails` in existence, not just the ones built through
/// [`AskDetails::new`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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

/// The managed target a [`Decision::Rewrite`] points to (FR-CMD-003).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ManagedTarget(String);

impl ManagedTarget {
    /// Names a managed target, e.g. `"legion:legion-explore"`.
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// The target's name.
    pub fn as_str(&self) -> &str {
        &self.0
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
    /// This session's unexpired, unused confirmations (FR-CMD-026), keyed by
    /// the confirmed command's [`CommandKey`], each with the reason the agent
    /// gave. The adapter reads them from the confirmation store; route decides
    /// what one does (FR-CMD-011).
    pub confirmations: HashMap<CommandKey, String>,
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Facts {
    /// The canonical parsed form of a Bash command as typed (FR-CMD-026),
    /// when it parsed: the key a confirmation for it is stored and matched
    /// under.
    pub command_key: Option<CommandKey>,
    /// The Bash command to run in place of the typed one: the typed command
    /// with `legion ` inserted before each proxied name (#1337). `None` when
    /// nothing was inserted. The adapter puts it in the call's `command`
    /// field on a rewrite, and on an operator ask, so the command the
    /// operator approves is the one that runs.
    pub rewritten: Option<String>,
}

/// What decided a command: the policy entry `route` matched, an unparsable
/// command, a never-run entry, or the no-match default. The adapter (#1229)
/// reads the operator mark; the incident records (#1237) name the entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Deciding {
    /// A policy entry matched and produced the decision. `id` is its
    /// policy-unique id (a rule id, an ask or power-switch entry id, or
    /// `proxy` for an insertion). `needs_operator` is set only when a
    /// confirmation in `Context` answered an ask whose entry marks the
    /// command as needing the operator (FR-CMD-006, FR-CMD-026); an
    /// unconfirmed ask never sets it.
    Rule { id: String, needs_operator: bool },
    /// A never-run entry matched (FR-CMD-025). `id` is the entry's stable
    /// id, the incident record's matched entry and its repeat key
    /// (FR-CMD-027).
    NoGo { id: String },
    /// The command did not parse, so `route` denied it.
    ParseError,
    /// No entry matched, so the command runs as typed (or, for a non-Bash
    /// tool, an unresolvable rule's default deny decided it).
    Default,
}

/// `route`'s return value: exactly one [`Decision`], the [`Facts`] it
/// extracted (FR-CMD-001, FR-CMD-003), and the entry that decided the command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Routed {
    pub decision: Decision,
    pub facts: Facts,
    pub deciding: Deciding,
    /// True when route treated an ask as answered by a confirmation in
    /// `Context` and the command now goes to the operator prompt
    /// (FR-CMD-026). The adapter consumes that confirmation; it holds no
    /// routing branch of its own.
    pub confirmed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- Decision is closed (FR-CMD-001) --------------------------------

    /// Matches every `Decision` arm with no wildcard. This compiles only
    /// while the set stays exactly {allow, rewrite, deny, ask} -- adding or
    /// removing an arm breaks this test at compile time, which is the point:
    /// the closed set is enforced by the compiler, not by a runtime check.
    #[test]
    fn decision_arms_are_exhaustively_named() {
        let decisions = [
            Decision::Allow { note: None },
            Decision::Rewrite {
                target: ManagedTarget::new("legion"),
                reason: "a proxied name".to_string(),
            },
            Decision::deny("no managed equivalent", "run it manually").expect("valid deny"),
            Decision::ask("which repo?", "the command names no repo").expect("valid ask"),
        ];
        for decision in decisions {
            match decision {
                Decision::Allow { .. } => {}
                Decision::Rewrite { .. } => {}
                Decision::Deny(_) => {}
                Decision::Ask(_) => {}
            }
        }
    }

    #[test]
    fn allow_note_is_carried_unchanged() {
        let decision = Decision::Allow {
            note: Some("recall ran first".to_string()),
        };
        match decision {
            Decision::Allow { note } => {
                assert_eq!(note.as_deref(), Some("recall ran first"));
            }
            _ => panic!("expected Allow"),
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

    // -- A Bash refusal always carries the fixed instead text ------------

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
    fn refuse_never_builds_an_empty_deny() {
        let Decision::Deny(details) = Decision::refuse("") else {
            panic!("refuse is always a deny");
        };
        assert!(!details.reason().is_empty());
        assert_eq!(details.instead(), NO_GO_INSTEAD);
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

    // -- Lookup distinguishes not-fetched from fetched-and-empty ---------

    #[test]
    fn lookup_defaults_to_not_fetched() {
        assert_eq!(Lookup::default(), Lookup::NotFetched);
    }

    #[test]
    fn lookup_empty_and_not_fetched_are_distinct() {
        assert_ne!(Lookup::Empty, Lookup::NotFetched);
    }

    // -- serialized shape (#1230: legion cmd-check --json) ---------------

    #[test]
    fn every_decision_arm_serializes_tagged_by_kind() {
        let cases = [
            (
                Decision::Allow {
                    note: Some("prefer sym".to_string()),
                },
                serde_json::json!({"kind": "allow", "note": "prefer sym"}),
            ),
            (
                Decision::Rewrite {
                    target: ManagedTarget::new("legion"),
                    reason: "a proxied name".to_string(),
                },
                serde_json::json!({"kind": "rewrite", "target": "legion",
                                   "reason": "a proxied name"}),
            ),
            (
                Decision::deny("unrecoverable", "trash it").expect("valid deny"),
                serde_json::json!({"kind": "deny", "reason": "unrecoverable", "instead": "trash it"}),
            ),
            (
                Decision::ask("merge it?", "PRs are the orchestrator's").expect("valid ask"),
                serde_json::json!({"kind": "ask", "question": "merge it?",
                                   "reason": "PRs are the orchestrator's"}),
            ),
        ];
        for (decision, expected) in cases {
            assert_eq!(
                serde_json::to_value(&decision).expect("serializes"),
                expected
            );
        }
    }

    #[test]
    fn facts_serialize_field_by_field() {
        let key = crate::nogo::command_key("git status").expect("key");
        let facts = Facts {
            command_key: Some(key.clone()),
            rewritten: Some("legion git status".to_string()),
        };
        assert_eq!(
            serde_json::to_value(&facts).expect("serializes"),
            serde_json::json!({"command_key": key.as_str(), "rewritten": "legion git status"})
        );
    }
}
