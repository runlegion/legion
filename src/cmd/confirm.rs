//! The confirmation store behind `legion cmd confirm` (#1237, FR-CMD-026).
//!
//! `legion cmd confirm --reason <why> -- <command>` records a one-time
//! confirmation for exactly `<command>` in the current session: the command's
//! [`CommandKey`], the session, the reason, and the time. A confirmation is
//! matched on the parsed key, bound to the session that recorded it, used up
//! by one run, and expires [`CONFIRMATION_TTL`] after it is recorded. It is
//! never a marker typed into the asked command itself.
//!
//! The adapter (`crate::cmd::hook`) reads this session's live confirmations
//! into `Context` through [`live_confirmations`] and, when route reports it
//! used one, uses it up through [`use_confirmation`]. route, not the adapter,
//! decides what a confirmation does (FR-CMD-011).

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use legion_cmd::{CommandKey, Context, Deciding, Policy, ScanError, ToolCall, command_key, route};

use crate::cmd::incident::{CONFIRMATION_TTL, IncidentLog, Origin};
use crate::db::{CmdConfirmation, Database};

/// Every way `legion cmd confirm` refuses. None records a confirmation.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ConfirmError {
    #[error("--reason is required and must not be empty: say why this command should run")]
    MissingReason,
    #[error(
        "this command matches the no-go entry `{entry}`; a no-go command can never be confirmed"
    )]
    NoGo { entry: String },
    #[error("the command does not parse, so it cannot be confirmed: {0}")]
    Parse(ScanError),
    #[error("no session to bind the confirmation to: {0} is not set")]
    NoSession(&'static str),
    #[error("the policy file could not be read: {0}")]
    Policy(String),
    #[error("the confirmation could not be recorded: {0}")]
    Store(String),
}

/// What `legion cmd confirm` was asked to confirm, and by whom.
#[derive(Debug, Clone)]
pub(crate) struct ConfirmRequest {
    /// The command as the agent typed it after `--`.
    pub(crate) origin: Origin,
    pub(crate) reason: Option<String>,
}

/// Records a confirmation for `request`'s command (FR-CMD-026) and its
/// incident record (FR-CMD-027). Writes pending drop rows first, before its
/// own work. `policy` supplies any policy-file no-go entries; the built-in
/// ones hold without it.
pub(crate) fn confirm(
    request: &ConfirmRequest,
    policy: &Policy,
    db: &Database,
    log: &IncidentLog,
    now: DateTime<Utc>,
) -> Result<CmdConfirmation, ConfirmError> {
    log.record_pending_drops(&|id: &str| was_used(db, id), now)
        .map_err(|e| ConfirmError::Store(e.to_string()))?;

    let reason: String = request
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .ok_or(ConfirmError::MissingReason)?
        .to_string();
    let command: &str = &request.origin.command;
    let key: CommandKey = command_key(command).map_err(ConfirmError::Parse)?;

    // The one routing call names the entry the command matches: a no-go
    // entry refuses the confirmation, and a policy entry is the incident
    // record's matched entry.
    let call = ToolCall {
        tool: "Bash".to_string(),
        input: serde_json::json!({ "command": command }),
    };
    let entry: Option<String> = match route(policy, &call, &Context::default()).deciding {
        Deciding::NoGo { id } => return Err(ConfirmError::NoGo { entry: id }),
        Deciding::Rule { id, .. } => Some(id),
        Deciding::ParseError | Deciding::Default => None,
    };

    let confirmation = CmdConfirmation {
        id: uuid::Uuid::now_v7().to_string(),
        session_id: request.origin.session_id.clone(),
        command_key: key.as_str().to_string(),
        reason,
        recorded_at: now,
        used_at: None,
    };
    // The incident record is written first, so a record that cannot be
    // written leaves no usable confirmation behind. If the store insert then
    // fails, the record names a confirmation that never existed; the store
    // reports it unused, so it becomes a drop once it expires.
    log.record_confirmation(
        &confirmation.id,
        &request.origin,
        entry.as_deref(),
        &confirmation.command_key,
        &confirmation.reason,
        now,
    )
    .map_err(|e| ConfirmError::Store(e.to_string()))?;
    db.insert_cmd_confirmation(&confirmation)
        .map_err(|e| ConfirmError::Store(e.to_string()))?;
    Ok(confirmation)
}

/// Whether the confirmation `id` was used. A confirmation the store no longer
/// holds was never used: its incident record becomes a drop.
pub(crate) fn was_used(db: &Database, id: &str) -> crate::error::Result<bool> {
    Ok(db
        .cmd_confirmation(id)?
        .is_some_and(|c| c.used_at.is_some()))
}

/// The earliest `recorded_at` a confirmation can have and still be live at
/// `now`.
fn live_since(now: DateTime<Utc>) -> DateTime<Utc> {
    now - CONFIRMATION_TTL
}

/// This session's unexpired, unused confirmations, keyed by command key, each
/// with the agent's reason -- the map `Context::confirmations` holds.
pub(crate) fn live_confirmations(
    db: &Database,
    session_id: &str,
    now: DateTime<Utc>,
) -> crate::error::Result<HashMap<CommandKey, String>> {
    Ok(db
        .live_cmd_confirmations(session_id, live_since(now))?
        .into_iter()
        .map(|c| (CommandKey::from_stored(c.command_key), c.reason))
        .collect())
}

/// Uses up one live confirmation for `key` in this session. False when none
/// was left, which the caller treats as a failure to record the run.
pub(crate) fn use_confirmation(
    db: &Database,
    session_id: &str,
    key: &CommandKey,
    now: DateTime<Utc>,
) -> crate::error::Result<bool> {
    db.use_cmd_confirmation(session_id, key.as_str(), live_since(now), now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil::test_db;
    use crate::telemetry::CmdIncidentKind;
    use chrono::Duration;
    use legion_cmd::parse_policy;

    fn request(command: &str, reason: Option<&str>, session: &str) -> ConfirmRequest {
        ConfirmRequest {
            origin: Origin {
                command: command.to_string(),
                agent: "legion".to_string(),
                repo: "legion".to_string(),
                session_id: session.to_string(),
                cwd: "/repo/legion".to_string(),
            },
            reason: reason.map(str::to_string),
        }
    }

    fn policy() -> Policy {
        parse_policy(
            r#"{
            "wrappers": [{"binary": "sudo"}],
            "no_go": [{"id": "shred-disk", "binaries": ["shred"],
                "predicates": [{"kind": "operand", "prefixes": ["/dev/"]}]}],
            "tools": {"Bash": {"families": {
                "curl": {"rules": [{"id": "curl-ask", "outcome": {"kind": "ask",
                    "question": "fetch?", "reason": "network"}}]}
            }}}}"#,
        )
        .expect("policy")
    }

    fn log() -> (IncidentLog, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        (IncidentLog::at(dir.path().join("cmd-incidents.jsonl")), dir)
    }

    #[test]
    fn a_missing_or_empty_reason_fails_and_records_nothing() {
        let db = test_db();
        let (log, _dir) = log();
        let now = Utc::now();
        for reason in [None, Some(""), Some("   ")] {
            let err = confirm(
                &request("curl example.com", reason, "s1"),
                &policy(),
                &db,
                &log,
                now,
            )
            .expect_err("refused");
            assert!(matches!(err, ConfirmError::MissingReason));
        }
        assert!(live_confirmations(&db, "s1", now).expect("read").is_empty());
        assert!(log.records().expect("read").is_empty());
    }

    #[test]
    fn confirming_records_exactly_that_command_with_its_reason() {
        let db = test_db();
        let (log, _dir) = log();
        let now = Utc::now();
        let stored = confirm(
            &request("curl example.com", Some("release notes"), "s1"),
            &policy(),
            &db,
            &log,
            now,
        )
        .expect("confirmed");
        let live = live_confirmations(&db, "s1", now).expect("read");
        let key = command_key("curl example.com").expect("key");
        assert_eq!(live.get(&key).map(String::as_str), Some("release notes"));
        assert_eq!(live.len(), 1);

        let records = log.records().expect("read");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].kind, CmdIncidentKind::Confirmation);
        assert_eq!(records[0].id, stored.id);
        assert_eq!(records[0].reason.as_deref(), Some("release notes"));
        assert_eq!(records[0].entry.as_deref(), Some("curl-ask"));
    }

    #[test]
    fn confirming_a_no_go_command_fails_and_records_nothing() {
        let db = test_db();
        let (log, _dir) = log();
        let now = Utc::now();
        for (command, entry) in [
            ("rm -rf /", "rm-recursive-force-root"),
            ("sudo rm -rf /", "rm-recursive-force-root"),
            ("shred /dev/sda", "shred-disk"),
        ] {
            let err = confirm(
                &request(command, Some("please"), "s1"),
                &policy(),
                &db,
                &log,
                now,
            )
            .expect_err("refused");
            match err {
                ConfirmError::NoGo { entry: got } => assert_eq!(got, entry, "{command}"),
                other => panic!("`{command}` expected NoGo, got {other:?}"),
            }
        }
        assert!(live_confirmations(&db, "s1", now).expect("read").is_empty());
        assert!(log.records().expect("read").is_empty());
    }

    #[test]
    fn a_command_that_does_not_parse_cannot_be_confirmed() {
        let db = test_db();
        let (log, _dir) = log();
        let err = confirm(
            &request("curl 'unterminated", Some("r"), "s1"),
            &policy(),
            &db,
            &log,
            Utc::now(),
        )
        .expect_err("refused");
        assert!(matches!(err, ConfirmError::Parse(_)));
    }

    #[test]
    fn a_confirmation_is_bound_to_its_session_single_use_and_expires() {
        let db = test_db();
        let (log, _dir) = log();
        let now = Utc::now();
        confirm(
            &request("curl example.com", Some("r"), "s1"),
            &policy(),
            &db,
            &log,
            now,
        )
        .expect("confirmed");
        let key = command_key("curl example.com").expect("key");

        assert!(live_confirmations(&db, "s2", now).expect("read").is_empty());
        assert!(
            live_confirmations(&db, "s1", now + Duration::minutes(11))
                .expect("read")
                .is_empty()
        );
        assert!(use_confirmation(&db, "s1", &key, now).expect("use"));
        assert!(!use_confirmation(&db, "s1", &key, now).expect("use"));
    }

    #[test]
    fn a_record_that_cannot_be_written_leaves_no_confirmation() {
        let db = test_db();
        // A read-only log: the pending-drop read succeeds and the append
        // fails. The record is written before the store insert, so nothing
        // is inserted.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cmd-incidents.jsonl");
        std::fs::write(&path, "").expect("write");
        let mut perms = std::fs::metadata(&path).expect("meta").permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&path, perms).expect("chmod");
        let log = IncidentLog::at(path);
        let now = Utc::now();
        let err = confirm(
            &request("curl example.com", Some("r"), "s1"),
            &policy(),
            &db,
            &log,
            now,
        )
        .expect_err("refused");
        assert!(matches!(err, ConfirmError::Store(_)));
        assert!(live_confirmations(&db, "s1", now).expect("read").is_empty());
    }

    #[test]
    fn a_store_insert_that_fails_leaves_no_confirmation_and_its_record_drops() {
        let db = test_db();
        db.conn
            .execute_batch(
                "CREATE TRIGGER refuse_insert BEFORE INSERT ON cmd_confirmations \
                 BEGIN SELECT RAISE(ABORT, 'store is read-only'); END;",
            )
            .expect("trigger");
        let (log, _dir) = log();
        let now = Utc::now();
        let err = confirm(
            &request("curl example.com", Some("r"), "s1"),
            &policy(),
            &db,
            &log,
            now,
        )
        .expect_err("refused");
        match err {
            ConfirmError::Store(text) => assert!(text.contains("store is read-only"), "{text}"),
            other => panic!("expected Store, got {other:?}"),
        }
        assert!(live_confirmations(&db, "s1", now).expect("read").is_empty());
        // The record names a confirmation the store never held: once it
        // expires it is recorded as a drop.
        let later = now + chrono::Duration::minutes(11);
        let dropped = log
            .record_pending_drops(&|id: &str| was_used(&db, id), later)
            .expect("drops");
        assert_eq!(dropped, 1);
    }

    #[test]
    fn confirm_writes_pending_drops_before_its_own_work() {
        let db = test_db();
        let (log, _dir) = log();
        let now = Utc::now();
        let mut stale = request("curl example.org", None, "s1").origin;
        stale.command = "curl example.org".to_string();
        log.record_ask(
            &stale,
            Some("curl-ask"),
            Some("k"),
            now - Duration::minutes(12),
        )
        .expect("record");
        // Even a refused confirmation writes the pending drop first.
        let _ = confirm(
            &request("curl example.com", None, "s1"),
            &policy(),
            &db,
            &log,
            now,
        );
        let drops = log
            .records()
            .expect("read")
            .into_iter()
            .filter(|r| r.kind == CmdIncidentKind::Drop)
            .count();
        assert_eq!(drops, 1);
    }
}
