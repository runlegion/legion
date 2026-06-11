//! Signal detection and the wake gate: pending-signal lookup, the
//! wake-worthy verb cut, and the wake prompt built for a woken agent.

use crate::db::Database;
use crate::error::Result;
use crate::signal;

// -- Signal Detection --------------------------------------------------------

/// Find unhandled signals targeting a specific repo.
///
/// `names` is the full addressable name set for this repo: the repo's
/// `recipient()` value plus any `broadcast_tags`. Signals addressed to any
/// name in this set are returned, along with reserved `@all` / `@everyone`
/// broadcasts. The DB handles per-repo dedup via `watch_handled` so each
/// broadcast wakes a repo exactly once regardless of which names it carries.
///
/// Returns signal reflection IDs and their text, filtered to only actual
/// signals (text starts with @).
pub fn find_pending_signals(
    db: &Database,
    repo_name: &str,
    names: &[String],
    since: Option<&str>,
) -> Result<Vec<(String, String, String)>> {
    let reflections = db.get_unhandled_signals_for_repo(repo_name, names, since)?;

    let mut signals: Vec<(String, String, String)> = Vec::new();
    for r in reflections {
        if signal::is_signal(&r.text) {
            signals.push((r.id, r.text, r.repo));
        }
    }

    Ok(signals)
}

/// Verbs whose presence in a directed signal triggers a watch wake (#404, #586).
///
/// `--verb` was designed to express intent; this set is the subsection of
/// intents that warrant spawning an asleep recipient. Other verbs
/// (`announce`, `ack`, `info`, `answer`, bare `review`) deliver to live
/// sessions via the channel push but do not page an asleep agent.
///
/// The canon (#586) is the set of verbs that actually mean "I need you to act":
/// `question`, `request`, `handoff`, `correction`, `proposal`, `decision`,
/// `rfc`, `routing`. The original #404 set carried `help` and `blocker`, but
/// those are *statuses* (`request:help`, `review:blocker`), not bare verbs --
/// they decorated the status slot, never the verb slot, so they never matched a
/// real signal's verb and only crowded the wake set. The verbs added here are
/// the ones bullpen mining shows the team actually uses to page each other.
///
/// Posts (text without a leading `@recipient`) never wake -- posts are
/// broadcasts; the `legion signal --to <agent> --verb <wake-worthy>`
/// primitive is the agent equivalent of a tweet at someone.
pub const WAKE_WORTHY_VERBS: &[&str] = &[
    "question",
    "request",
    "handoff",
    "correction",
    "proposal",
    "decision",
    "rfc",
    "routing",
];

/// Whether a directed `legion signal --to <to> --verb <verb>` will fail to wake
/// its recipient, so the sender can be warned at send time (#586).
///
/// True when the target is a specific agent (not the `@all`/`@everyone`
/// broadcast, which is never a directed page) AND the verb is not wake-worthy
/// in the active manifest. Such a signal still delivers to a live session via
/// the channel push, but it does not page an asleep agent -- surfacing that
/// avoids the silent "I signaled but nobody woke" trap. Reserved broadcast
/// names are matched case-insensitively.
pub fn directed_verb_will_not_wake(to: &str, verb: &str) -> bool {
    let to_normalized = to.trim().to_ascii_lowercase();
    let is_broadcast = matches!(to_normalized.as_str(), "all" | "everyone");
    !is_broadcast && !crate::verbs::active_manifest().is_wake_worthy(verb)
}

/// Whether a signal text triggers a wake under the verb-driven gate.
///
/// Returns `false` for posts, malformed signals, and signals whose verb is not
/// wake-worthy in the active manifest. The decision is verb-only -- status is
/// decoration that downstream tools may use, but the wake gate ignores it.
///
/// The active manifest defaults to `builtin_default()`, which reproduces the
/// #586 canon exactly. Operators can overlay additional verbs via TOML files
/// in the verbs directory without a legion release.
pub fn is_wake_worthy(text: &str) -> bool {
    match signal::parse_signal(text) {
        Some(sig) => crate::verbs::active_manifest().is_wake_worthy(&sig.verb),
        None => false,
    }
}

/// Signals that require the recipient to reply, even if the reply is a refusal.
///
/// Same gate as [`is_wake_worthy`] -- the wake decision and the
/// REQUIRES A REPLY routing in [`build_wake_prompt`] use one verb cut so a
/// signal that woke the agent always lands in the section the agent must
/// answer.
pub fn signal_requires_reply(text: &str) -> bool {
    is_wake_worthy(text)
}

/// Maximum reply-required signals rendered inline in the wake prompt; the
/// rest collapse into a tail line pointing at `legion bullpen --signals`.
/// Caps prevent the SessionStart additionalContext block from being drowned
/// by deep backlogs (rafters' pending block was 100KB pre-cap).
const PENDING_REPLY_CAP: usize = 10;
const PENDING_INFORMATIONAL_CAP: usize = 5;

/// Build the prompt context for a woken agent from pending signals.
///
/// Signals are split into two buckets:
/// - **Requires reply**: directed questions and requests. The agent MUST reply,
///   even if the reply is "no", "can't help", or "handing to X". Ghosting is not
///   an option -- it breaks team trust.
/// - **Informational**: announcements, updates, approvals. Silence is
///   acknowledgment; only reply if there is new information, concern, or dissent.
pub fn build_wake_prompt(repo_name: &str, signals: &[(String, String, String)]) -> String {
    let (must_reply, informational): (Vec<_>, Vec<_>) = signals
        .iter()
        .partition(|(_, text, _)| signal_requires_reply(text));

    let mut prompt = format!(
        "You were auto-woken by legion watch. The following signal(s) are directed at you ({}).\n",
        repo_name
    );

    let mut append_section = |header: &str, bucket: &[&(String, String, String)], cap: usize| {
        if bucket.is_empty() {
            return;
        }
        prompt.push('\n');
        prompt.push_str(header);
        prompt.push_str("\n\n");
        for (id, text, from_repo) in bucket.iter().take(cap) {
            prompt.push_str(&format!("- [from {}] {} (id: {})\n", from_repo, text, id));
        }
        if bucket.len() > cap {
            let extra = bucket.len() - cap;
            prompt.push_str(&format!(
                "- ... and {} more. Run `legion bullpen --repo {} --signals` to see them all.\n",
                extra, repo_name
            ));
        }
    };

    append_section(
        "REQUIRES A REPLY -- these are directed questions or requests. \
         You MUST reply to each one, even if the reply is \"no\", \"can't help\", \
         or \"handing to X\". Silence on a directed question is ghosting, not \
         acknowledgment. A short refusal is a valid reply; no reply is not.",
        &must_reply,
        PENDING_REPLY_CAP,
    );

    append_section(
        "INFORMATIONAL -- announcements, updates, approvals. \
         Silence is acknowledgment. Only reply if you have NEW information, \
         a concern, dissent, or an action item. Empty acknowledgments like \
         \"acknowledged, no action needed\" waste tokens and trigger wake storms.",
        &informational,
        PENDING_INFORMATIONAL_CAP,
    );

    prompt.push_str(
        "\nUse `legion signal` to reply, `legion bullpen` for broader context, and \
         `legion reflect` to store any learnings before you exit.",
    );

    prompt
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::test_storage;

    #[test]
    fn find_pending_signals_detects_targeted_signals() {
        let (db, _index, _dir) = test_storage();

        // Post a signal from kelex to legion
        db.insert_reflection("kelex", "@legion review:approved", "team")
            .expect("insert signal");

        // Post a non-signal
        db.insert_reflection("rafters", "just a musing", "team")
            .expect("insert musing");

        // Post a signal to all
        db.insert_reflection("rafters", "@all announce: shipped", "team")
            .expect("insert broadcast");

        let signals = find_pending_signals(&db, "legion", &["legion".to_string()], None)
            .expect("find signals");
        assert_eq!(signals.len(), 2);

        // Verify the targeted signal is found
        assert!(
            signals
                .iter()
                .any(|(_, text, _)| text == "@legion review:approved")
        );
        // Verify the broadcast is found
        assert!(
            signals
                .iter()
                .any(|(_, text, _)| text == "@all announce: shipped")
        );
    }

    #[test]
    fn find_pending_signals_detects_multi_recipient() {
        let (db, _index, _dir) = test_storage();

        // Multi-recipient signal: @shingle @huttspawn -- message
        db.insert_reflection(
            "legion",
            "@shingle @huttspawn -- build draft sites from current content",
            "team",
        )
        .expect("insert multi-recipient");

        // Both shingle and huttspawn should see it
        let shingle =
            find_pending_signals(&db, "shingle", &["shingle".to_string()], None).expect("shingle");
        let huttspawn = find_pending_signals(&db, "huttspawn", &["huttspawn".to_string()], None)
            .expect("huttspawn");
        assert_eq!(
            shingle.len(),
            1,
            "shingle should see multi-recipient signal"
        );
        assert_eq!(
            huttspawn.len(),
            1,
            "huttspawn should see multi-recipient signal"
        );

        // legion (sender) should NOT see it
        let legion =
            find_pending_signals(&db, "legion", &["legion".to_string()], None).expect("legion");
        assert!(legion.is_empty(), "sender should not see own signal");

        // unrelated repo should NOT see it
        let kelex =
            find_pending_signals(&db, "kelex", &["kelex".to_string()], None).expect("kelex");
        assert!(kelex.is_empty(), "unmentioned repo should not see signal");
    }

    #[test]
    fn find_pending_signals_excludes_self_signals() {
        let (db, _index, _dir) = test_storage();

        // Signal from legion to legion should not be returned
        db.insert_reflection("legion", "@legion review:approved", "team")
            .expect("insert self-signal");

        let signals = find_pending_signals(&db, "legion", &["legion".to_string()], None)
            .expect("find signals");
        assert!(signals.is_empty(), "self-signals should be excluded");
    }

    #[test]
    fn find_pending_signals_with_agent_excludes_self_signals_via_repo_name() {
        // Regression guard: when agent != name (e.g., platform agent watches ledger repo),
        // a signal posted FROM the ledger repo targeting @platform must still be excluded
        // by the repo_name key, not by the recipient. Previously only recipient was passed,
        // so `r.repo != 'platform'` vs. `r.repo = 'ledger'` produced a self-wake.
        let (db, _index, _dir) = test_storage();

        db.insert_reflection("ledger", "@platform review:approved", "team")
            .expect("insert self-signal");

        let signals = find_pending_signals(&db, "ledger", &["platform".to_string()], None)
            .expect("find signals");
        assert!(
            signals.is_empty(),
            "self-signals must be excluded by repo_name, not by recipient"
        );
    }

    #[test]
    fn find_pending_signals_with_agent_accepts_signals_from_other_repos() {
        let (db, _index, _dir) = test_storage();

        db.insert_reflection("kelex", "@platform review:approved", "team")
            .expect("insert external signal");

        let signals = find_pending_signals(&db, "ledger", &["platform".to_string()], None)
            .expect("find signals");
        assert_eq!(signals.len(), 1, "external signal to agent should be found");
    }

    #[test]
    fn mark_handled_prevents_re_detection() {
        let (db, _index, _dir) = test_storage();

        db.insert_reflection("kelex", "@legion review:approved", "team")
            .expect("insert signal");

        let signals =
            find_pending_signals(&db, "legion", &["legion".to_string()], None).expect("first poll");
        assert_eq!(signals.len(), 1);

        // Mark as handled for legion
        let (id, _, _) = &signals[0];
        db.mark_signal_handled_for_repo(id, "legion")
            .expect("mark handled");

        // Should not appear again for legion
        let signals = find_pending_signals(&db, "legion", &["legion".to_string()], None)
            .expect("second poll");
        assert!(signals.is_empty());
    }

    #[test]
    fn build_wake_prompt_formats_signals() {
        let signals = vec![
            (
                "id-1".to_string(),
                "@legion review:approved".to_string(),
                "kelex".to_string(),
            ),
            (
                "id-2".to_string(),
                "@all announce: shipped".to_string(),
                "rafters".to_string(),
            ),
        ];

        let prompt = build_wake_prompt("legion", &signals);
        assert!(prompt.contains("auto-woken by legion watch"));
        assert!(prompt.contains("@legion review:approved"));
        assert!(prompt.contains("@all announce: shipped"));
        assert!(prompt.contains("from kelex"));
        assert!(prompt.contains("from rafters"));
    }

    #[test]
    fn build_wake_prompt_splits_questions_from_announcements() {
        let signals = vec![
            (
                "id-q".to_string(),
                "@kessel question:help -- can you extract geometry?".to_string(),
                "huttspawn".to_string(),
            ),
            (
                "id-a".to_string(),
                "@all announce:ready -- dev tracker RSS unlocked".to_string(),
                "huttspawn".to_string(),
            ),
            (
                "id-r".to_string(),
                "@platform request:help -- need embeddings review".to_string(),
                "kelex".to_string(),
            ),
        ];

        let prompt = build_wake_prompt("kessel", &signals);

        // Directed questions/requests MUST be flagged as reply-required.
        assert!(
            prompt.contains("REQUIRES A REPLY"),
            "directed question should trigger reply-required section"
        );
        assert!(
            prompt.contains("id-q"),
            "question signal missing from prompt"
        );
        assert!(
            prompt.contains("id-r"),
            "request signal missing from prompt"
        );

        // Announcements belong in the informational section.
        assert!(
            prompt.contains("INFORMATIONAL"),
            "announcement should trigger informational section"
        );
        assert!(
            prompt.contains("id-a"),
            "announce signal missing from prompt"
        );

        // The reply-required section must appear BEFORE the informational one so
        // the agent reads its obligations first.
        let reply_idx = prompt.find("REQUIRES A REPLY").expect("reply section");
        let info_idx = prompt.find("INFORMATIONAL").expect("info section");
        assert!(reply_idx < info_idx, "reply-required must come first");
    }

    #[test]
    fn build_wake_prompt_routes_by_verb_only() {
        // #404: wake/reply decision is verb-only. Status is decoration.
        // The pre-#404 fallback that treated `status=request` or `status=help`
        // on any verb as reply-required was a workaround for the broken
        // text-prefix wake gate. Senders who want a reply now use a
        // wake-worthy verb directly: `--verb request`, `--verb handoff`,
        // `--verb question`, `--verb decision` (#586 canon -- `help` and
        // `blocker` are statuses, not wake verbs).
        let signals = vec![
            (
                "id-rev-req".to_string(),
                "@platform review:request {doc: rfc.md} -- review please".to_string(),
                "smugglr".to_string(),
            ),
            (
                "id-handoff-verb".to_string(),
                "@platform handoff -- taking the rfc over to you".to_string(),
                "kelex".to_string(),
            ),
            (
                "id-request-verb".to_string(),
                "@platform request -- could you review this".to_string(),
                "kelex".to_string(),
            ),
            (
                "id-approved".to_string(),
                "@platform review:approved -- LGTM".to_string(),
                "smugglr".to_string(),
            ),
        ];

        let prompt = build_wake_prompt("platform", &signals);

        assert!(prompt.contains("REQUIRES A REPLY"));
        assert!(prompt.contains("INFORMATIONAL"));
        let reply_section = &prompt
            [prompt.find("REQUIRES A REPLY").unwrap()..prompt.find("INFORMATIONAL").unwrap()];

        assert!(
            reply_section.contains("id-handoff-verb"),
            "verb=handoff must require reply"
        );
        assert!(
            reply_section.contains("id-request-verb"),
            "verb=request must require reply"
        );
        assert!(
            !reply_section.contains("id-rev-req"),
            "verb=review with status=request is no longer wake-worthy (#404 verb-only gate)"
        );
        assert!(
            !reply_section.contains("id-approved"),
            "verb=review with status=approved must not require reply"
        );
    }

    #[test]
    fn is_wake_worthy_recognizes_designated_verbs() {
        for verb in WAKE_WORTHY_VERBS {
            let text = format!("@kessel {verb} -- something");
            assert!(
                is_wake_worthy(&text),
                "verb `{verb}` should be wake-worthy: {text}"
            );
        }
    }

    #[test]
    fn is_wake_worthy_rejects_informational_verbs() {
        // `help` and `blocker` are included here deliberately (#586): they are
        // statuses (`request:help`, `review:blocker`), not bare wake verbs, and
        // must not wake when used in the verb slot.
        for verb in [
            "announce", "ack", "info", "answer", "review", "help", "blocker",
        ] {
            let text = format!("@kessel {verb} -- fyi");
            assert!(
                !is_wake_worthy(&text),
                "verb `{verb}` must not be wake-worthy: {text}"
            );
        }
    }

    /// `signal_requires_reply` is a deliberate alias of `is_wake_worthy`:
    /// one verb cut so a signal that woke the agent always lands in the
    /// section the agent must answer. Pin the equivalence so the two names
    /// cannot drift apart silently -- an intentional divergence must come
    /// here and change this test.
    #[test]
    fn signal_requires_reply_matches_is_wake_worthy() {
        let samples = [
            "@kessel question -- can you help?",
            "@kessel request:help -- pls",
            "@kessel handoff -- yours now",
            "@all announce -- shipped",
            "@kessel ack -- got it",
            "@kessel review:approved -- LGTM",
            "not a signal at all",
        ];
        for text in samples {
            assert_eq!(
                signal_requires_reply(text),
                is_wake_worthy(text),
                "wake decision and reply-required routing diverged for: {text}"
            );
        }
    }

    #[test]
    fn is_wake_worthy_rejects_posts() {
        // Posts don't start with @recipient -- never wake.
        assert!(!is_wake_worthy("just a musing about something"));
        assert!(!is_wake_worthy("checking in @kessel did you see #401"));
    }

    #[test]
    fn is_wake_worthy_ignores_status_decoration() {
        // Status no longer gates wake; only the verb does.
        assert!(
            !is_wake_worthy("@platform review:request {doc: rfc.md}"),
            "verb=review with status=request must not wake under verb-only gate"
        );
        assert!(
            is_wake_worthy("@platform request:help -- pls"),
            "verb=request stays wake-worthy regardless of status"
        );
    }

    #[test]
    fn directed_non_wake_verb_warns() {
        // A directed signal with an informational verb should warn.
        assert!(directed_verb_will_not_wake("kessel", "announce"));
        assert!(directed_verb_will_not_wake("kessel", "ack"));
        // `help`/`blocker` are statuses, not wake verbs (#586) -- they warn too.
        assert!(directed_verb_will_not_wake("kessel", "help"));
        assert!(directed_verb_will_not_wake("kessel", "blocker"));
    }

    #[test]
    fn directed_wake_verb_does_not_warn() {
        for verb in WAKE_WORTHY_VERBS {
            assert!(
                !directed_verb_will_not_wake("kessel", verb),
                "wake-worthy verb `{verb}` must not warn"
            );
        }
    }

    #[test]
    fn broadcast_target_never_warns() {
        // @all / @everyone are not directed pages, so no warning regardless of verb.
        assert!(!directed_verb_will_not_wake("all", "announce"));
        assert!(!directed_verb_will_not_wake("everyone", "announce"));
        assert!(!directed_verb_will_not_wake("All", "ack")); // case-insensitive
    }

    #[test]
    fn build_wake_prompt_caps_long_buckets() {
        // 15 reply-required questions: cap is 10, expect 10 rendered + tail.
        let questions: Vec<(String, String, String)> = (0..15)
            .map(|i| {
                (
                    format!("id-q-{i}"),
                    format!("@legion question: thing {i}?"),
                    "rafters".to_string(),
                )
            })
            .collect();
        let prompt = build_wake_prompt("legion", &questions);
        let rendered = prompt.matches("(id: id-q-").count();
        assert_eq!(
            rendered, PENDING_REPLY_CAP,
            "expected exactly {PENDING_REPLY_CAP} rendered ids, got {rendered}: {prompt}"
        );
        assert!(
            prompt.contains("... and 5 more"),
            "expected tail line for 5 overflow signals: {prompt}"
        );
        assert!(
            prompt.contains("legion bullpen --repo legion --signals"),
            "tail should point at bullpen --signals: {prompt}"
        );

        // 8 informational announcements: cap is 5.
        let announcements: Vec<(String, String, String)> = (0..8)
            .map(|i| {
                (
                    format!("id-a-{i}"),
                    format!("@all announce: thing {i}"),
                    "rafters".to_string(),
                )
            })
            .collect();
        let prompt = build_wake_prompt("legion", &announcements);
        let rendered = prompt.matches("(id: id-a-").count();
        assert_eq!(
            rendered, PENDING_INFORMATIONAL_CAP,
            "expected exactly {PENDING_INFORMATIONAL_CAP} rendered ids, got {rendered}: {prompt}"
        );
        assert!(
            prompt.contains("... and 3 more"),
            "expected tail line for 3 overflow announcements: {prompt}"
        );
    }

    #[test]
    fn build_wake_prompt_omits_sections_when_empty() {
        let announce_only = vec![(
            "id-a".to_string(),
            "@all announce: shipped v0.9.3".to_string(),
            "rafters".to_string(),
        )];
        let prompt = build_wake_prompt("legion", &announce_only);
        assert!(!prompt.contains("REQUIRES A REPLY"));
        assert!(prompt.contains("INFORMATIONAL"));

        let question_only = vec![(
            "id-q".to_string(),
            "@eavesdrop question: when does the feed index?".to_string(),
            "huttspawn".to_string(),
        )];
        let prompt = build_wake_prompt("eavesdrop", &question_only);
        assert!(prompt.contains("REQUIRES A REPLY"));
        assert!(!prompt.contains("INFORMATIONAL"));
    }

    #[test]
    fn broadcast_signals_visible_to_all_repos() {
        let (db, _index, _dir) = test_storage();

        // Post an @all signal from kelex
        db.insert_reflection("kelex", "@all RFC:help -- discover proposal", "team")
            .expect("insert broadcast");

        // Both legion and rafters should see it
        let legion_signals =
            find_pending_signals(&db, "legion", &["legion".to_string()], None).expect("legion");
        let rafters_signals =
            find_pending_signals(&db, "rafters", &["rafters".to_string()], None).expect("rafters");
        assert_eq!(legion_signals.len(), 1);
        assert_eq!(rafters_signals.len(), 1);

        // Mark handled for legion using per-repo tracking
        for (id, _, _) in &legion_signals {
            db.mark_signal_handled_for_repo(id, "legion")
                .expect("mark handled for legion");
        }

        // legion should NOT see it anymore
        let legion_after = find_pending_signals(&db, "legion", &["legion".to_string()], None)
            .expect("legion after");
        assert!(
            legion_after.is_empty(),
            "legion should not see broadcast after handling"
        );

        // rafters should STILL see the broadcast
        let rafters_after = find_pending_signals(&db, "rafters", &["rafters".to_string()], None)
            .expect("rafters after");
        assert_eq!(
            rafters_after.len(),
            1,
            "broadcast should remain visible to other repos"
        );

        // Mark handled for rafters too
        for (id, _, _) in &rafters_signals {
            db.mark_signal_handled_for_repo(id, "rafters")
                .expect("mark handled for rafters");
        }

        // Now rafters should not see it either
        let rafters_final = find_pending_signals(&db, "rafters", &["rafters".to_string()], None)
            .expect("rafters final");
        assert!(
            rafters_final.is_empty(),
            "rafters should not see broadcast after handling"
        );
    }

    #[test]
    fn at_everyone_wakes_repo_same_as_at_all() {
        // @everyone must behave identically to @all: both repos should see the
        // broadcast, each exactly once. This is the regression guard for the
        // new reserved word.
        let (db, _index, _dir) = test_storage();

        db.insert_reflection("kelex", "@everyone RFC:help -- discover proposal", "team")
            .expect("insert @everyone broadcast");

        let legion_signals =
            find_pending_signals(&db, "legion", &["legion".to_string()], None).expect("legion");
        let rafters_signals =
            find_pending_signals(&db, "rafters", &["rafters".to_string()], None).expect("rafters");

        assert_eq!(
            legion_signals.len(),
            1,
            "@everyone must wake legion just like @all"
        );
        assert_eq!(
            rafters_signals.len(),
            1,
            "@everyone must wake rafters just like @all"
        );

        // Mark handled for legion; rafters must still see it.
        for (id, _, _) in &legion_signals {
            db.mark_signal_handled_for_repo(id, "legion")
                .expect("mark legion handled");
        }

        let rafters_after = find_pending_signals(&db, "rafters", &["rafters".to_string()], None)
            .expect("rafters after legion handled");
        assert_eq!(
            rafters_after.len(),
            1,
            "@everyone dedup must be per-repo: rafters must still see the signal"
        );
    }

    #[test]
    fn broadcast_tag_signal_wakes_only_tagged_repos() {
        // A signal addressed @eng-team should wake repos whose broadcast_tags
        // includes "eng-team", but NOT repos that do not carry that tag.
        let (db, _index, _dir) = test_storage();

        // tagged: subscribes to "eng-team"
        let tagged_names: Vec<String> = vec!["tagged".to_string(), "eng-team".to_string()];
        // untagged: no broadcast_tags
        let untagged_names: Vec<String> = vec!["untagged".to_string()];

        db.insert_reflection("other", "@eng-team question:help -- need input", "team")
            .expect("insert tag signal");

        let tagged_sigs = find_pending_signals(&db, "tagged", &tagged_names, None).expect("tagged");
        let untagged_sigs =
            find_pending_signals(&db, "untagged", &untagged_names, None).expect("untagged");

        assert_eq!(
            tagged_sigs.len(),
            1,
            "repo with matching broadcast_tag must see the @eng-team signal"
        );
        assert!(
            untagged_sigs.is_empty(),
            "repo without the tag must NOT see the @eng-team signal"
        );
    }

    #[test]
    fn directed_signal_wakes_only_target_repo() {
        // A @<name> signal wakes only the repo whose recipient() equals name,
        // not repos that happen to share a tag.
        let (db, _index, _dir) = test_storage();

        db.insert_reflection("other", "@direct question:help -- for you only", "team")
            .expect("insert directed signal");

        let direct_sigs =
            find_pending_signals(&db, "direct", &["direct".to_string()], None).expect("direct");
        let bystander_sigs =
            find_pending_signals(&db, "bystander", &["bystander".to_string()], None)
                .expect("bystander");

        assert_eq!(direct_sigs.len(), 1, "target repo must see directed signal");
        assert!(
            bystander_sigs.is_empty(),
            "unaddressed repo must not see directed signal"
        );
    }
}
