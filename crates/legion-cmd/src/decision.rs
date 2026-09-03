//! What the router decided, and what it saw (FR-CMD-001, FR-CMD-002).

use serde::{Deserialize, Serialize};

/// The five-way decision (FR-CMD-002).
///
/// FIVE, exactly. FR-CMD-001 rev 6 explicitly refuses a sixth `Warn`/`Nudge`
/// arm: the advisory case is `Allow` plus `Routed::note`, because an advisory
/// that can block is not advisory, and an arm that sometimes blocks is the
/// thing the note field exists to prevent.
///
/// `Serialize`/`Deserialize` (FR-CMD-008): `cmd-check --json` embeds this
/// enum verbatim in `CmdCheckOutput`, and a test parses that output back --
/// so this type needs both directions, not just the crate-internal `Debug`
/// comparisons slices 1-3 used it for. Default (externally tagged) serde
/// representation: a unit variant like `Allow` serializes as the JSON string
/// `"Allow"`; a variant carrying data serializes as `{"Deny": "reason"}` /
/// `{"Rewrite": {"command": ..., "reason": ..., "carry": [...]}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Decision {
    /// Run it as typed.
    Allow,
    /// Run this instead, and say why.
    Rewrite {
        command: String,
        reason: String,
        carry: Vec<Carry>,
    },
    /// No managed equivalent exists: run it, track it, credit nothing.
    Proxy(String),
    /// Refuse, and say why.
    Deny(String),
    /// Escalate to the operator, and say why.
    Ask(String),
}

/// A byte payload lifted out of a rewritten command and reinserted verbatim.
///
/// Lossless-is-a-property-of-the-argument: a rewrite that would mangle a
/// literal carries it around the transformation instead of through it.
///
/// `Serialize`/`Deserialize` for the same reason as `Decision`: it nests
/// inside `Decision::Rewrite`, which `cmd-check --json` embeds whole.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub placeholder: String,
    pub bytes: Vec<u8>,
}

/// The managed binary a call was recognized as, and where in the command it sat.
///
/// `stage_span` indexes the pipeline stage list the tokenizer produces
/// (slice 2), so a binary hidden behind a pipe or an env prefix still reports
/// where it was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Matched {
    pub binary: String,
    pub stage_span: std::ops::Range<usize>,
}

/// What the call was about, for the record and for pre-load (FR-CMD-014).
///
/// `paths` is `String`, not `PathBuf`, and the tradeoff is worth stating
/// exactly rather than loosely. `PathBuf`'s `Serialize` FAILS on a non-UTF-8
/// path, which would abort the whole command-record write over one odd path.
/// `String` keeps the record writable. What it does NOT do is preserve the
/// original bytes: a `String` cannot hold invalid UTF-8 at all, so a non-UTF-8
/// path is lossily replaced at the boundary before it ever reaches this field.
/// The record therefore stays honest about the call happening and lossy about
/// that one path -- deliberately chosen over losing the record entirely.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Targets {
    pub paths: Vec<String>,
    pub verb: Option<String>,
    pub issues: Vec<u64>,
    pub prs: Vec<u64>,
    pub repo: Option<String>,
    pub words: Vec<String>,
}

/// The router's answer.
///
/// A struct rather than a tuple (FR-CMD-001): the caller reads fields by name,
/// so adding one later does not silently re-bind an existing destructure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Routed {
    pub decision: Decision,
    pub targets: Targets,
    pub matched: Option<Matched>,
    pub opaque: bool,
    /// The advisory arm (FR-CMD-001 rev 6, NFR-CMD-005 rev 6).
    ///
    /// Set from the matched route's `note`, and `Some` ONLY alongside
    /// `Decision::Allow`. `Deny` and `Proxy` carry their own reason strings;
    /// a note beside them would be a second, competing explanation.
    pub note: Option<String>,
}

impl Routed {
    /// An `Allow` with nothing matched -- the shape every no-match answer takes.
    pub fn allow() -> Routed {
        Routed {
            decision: Decision::Allow,
            targets: Targets::default(),
            matched: None,
            opaque: false,
            note: None,
        }
    }

    /// The answer a matched route produces.
    ///
    /// THE ADVISORY ARM LIVES HERE, and it is the one place a `note` is ever
    /// attached. A route carrying a `note` produces `Allow` plus that text --
    /// the command runs and the agent is told something. `Deny` and `Proxy`
    /// carry their own reason strings and take NO note, because two competing
    /// explanations on one decision is how an advisory quietly becomes a
    /// block (FR-CMD-005 rev 6 warns the next issue writer about exactly
    /// that conversion).
    ///
    /// Nothing calls this with a real match yet -- matching is slices 2 and 3.
    /// It exists now so the note has a carrier the moment a route can be
    /// matched, rather than being retrofitted onto a decision path that has
    /// already grown arms without it.
    pub fn from_route(decision: Decision, note: Option<String>) -> Routed {
        let note = match decision {
            Decision::Allow => note,
            _ => None,
        };
        Routed {
            decision,
            targets: Targets::default(),
            matched: None,
            opaque: false,
            note,
        }
    }

    /// A `Deny` with nothing matched.
    pub fn deny(reason: impl Into<String>) -> Routed {
        Routed {
            decision: Decision::Deny(reason.into()),
            targets: Targets::default(),
            matched: None,
            opaque: false,
            note: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_advisory_route_note_reaches_routed_note_alongside_allow() {
        let routed = Routed::from_route(Decision::Allow, Some("prefer legion recall".into()));
        assert_eq!(routed.decision, Decision::Allow);
        assert_eq!(routed.note.as_deref(), Some("prefer legion recall"));
    }

    #[test]
    fn a_deny_and_a_proxy_drop_the_note_and_keep_their_own_reason() {
        let denied = Routed::from_route(
            Decision::Deny("no sanctioned use".into()),
            Some("this note must not survive".into()),
        );
        assert_eq!(
            denied.note, None,
            "a Deny must not also carry an advisory note"
        );
        assert_eq!(denied.decision, Decision::Deny("no sanctioned use".into()));

        let proxied = Routed::from_route(
            Decision::Proxy("no managed equivalent".into()),
            Some("this note must not survive".into()),
        );
        assert_eq!(
            proxied.note, None,
            "a Proxy must not also carry an advisory note"
        );
        assert_eq!(
            proxied.decision,
            Decision::Proxy("no managed equivalent".into())
        );
    }

    #[test]
    fn a_rewrite_and_an_ask_drop_the_note_too() {
        for d in [
            Decision::Rewrite {
                command: "legion issue list".into(),
                reason: "work-source actions go through legion".into(),
                carry: vec![],
            },
            Decision::Ask("needs an operator".into()),
        ] {
            assert_eq!(Routed::from_route(d, Some("nope".into())).note, None);
        }
    }

    #[test]
    fn an_allow_with_no_note_stays_noteless() {
        assert_eq!(Routed::from_route(Decision::Allow, None).note, None);
        assert_eq!(Routed::allow().note, None);
    }

    #[test]
    fn carry_holds_bytes_verbatim_including_non_utf8() {
        let c = Carry {
            placeholder: "{{TEXT0}}".into(),
            bytes: vec![0xff, 0xfe, 0x00, 0x41],
        };
        assert_eq!(c.bytes, vec![0xff, 0xfe, 0x00, 0x41]);
    }

    #[test]
    fn a_non_utf8_path_is_lossily_replaced_but_the_record_still_serializes() {
        // Genuinely invalid UTF-8, not ASCII dressed up as a lossy conversion:
        // 0xff and 0xfe are not valid in any UTF-8 sequence.
        let raw = [0x2f, 0x74, 0x6d, 0x70, 0x2f, 0xff, 0xfe];
        let lossy = String::from_utf8_lossy(&raw).into_owned();
        assert!(
            lossy.contains('\u{fffd}'),
            "the fixture must actually exercise replacement, got {lossy:?}"
        );
        assert!(
            String::from_utf8(raw.to_vec()).is_err(),
            "the fixture bytes must really be invalid UTF-8"
        );

        let t = Targets {
            paths: vec![lossy],
            ..Targets::default()
        };
        let json = serde_json::to_string(&t).expect("targets serialize");
        let back: Targets = serde_json::from_str(&json).expect("targets deserialize");
        assert_eq!(t, back, "the replaced form round-trips unchanged");
    }
}
