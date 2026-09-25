//! legion-cmd incident records (#1237, FR-CMD-027), written through legion's
//! existing local telemetry (`crate::telemetry`, the `cmd-incidents.jsonl`
//! sibling of `bypass.jsonl`), not a new store.
//!
//! A no-go hit, an ask, a confirmation, and a drop each write one record
//! holding the command as issued, the agent, the repo, the session, the
//! working directory, the time, and the entry matched; a confirmation also
//! holds its reason. A drop is an ask with no confirmation within
//! [`CONFIRMATION_TTL`] of it, or a confirmation that expired unused: the
//! adapter and `legion cmd confirm` write pending drop rows before their own
//! work ([`IncidentLog::record_pending_drops`]), so a drop is recorded the next
//! time either runs on the node.
//!
//! Unlike the other telemetry logs, a failed write here is never swallowed:
//! the caller refuses the command (FR-CMD-027, FR-CMD-009).

use std::collections::HashSet;
use std::path::PathBuf;

use chrono::{DateTime, Duration, Utc};

use crate::error;
use crate::telemetry::{self, CmdIncidentKind, CmdIncidentRecord};

/// How long a confirmation stays usable, and how long an ask waits for one
/// before it is a drop (FR-CMD-026, FR-CMD-027).
pub(crate) const CONFIRMATION_TTL: Duration = Duration::minutes(10);

/// Who issued a command, and where: the fields every incident record holds
/// besides its kind, time, and entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Origin {
    pub(crate) command: String,
    pub(crate) agent: String,
    pub(crate) repo: String,
    pub(crate) session_id: String,
    pub(crate) cwd: String,
}

/// The agent a repo's commands are recorded under: the repo's watch.toml
/// recipient (its `agent`, else its name), or the repo itself when it is not
/// watched or watch.toml cannot be read.
pub(crate) fn agent_for(repo: &str) -> String {
    crate::cli::datadir::data_dir()
        .ok()
        .and_then(|dir| crate::watch::list_repos_in_config(&dir.join("watch.toml")).ok())
        .and_then(|repos| {
            repos
                .iter()
                .find(|r| r.name == repo)
                .map(|r| r.recipient().to_string())
        })
        .unwrap_or_else(|| repo.to_string())
}

/// The operator notice's recipient: the prime legion session, whose inbox
/// the operator reads (FR-CMD-027).
pub(crate) const NOTICE_RECIPIENT: &str = "legion";

/// The notice's fixed sender: the component raising it. Never equal to
/// [`NOTICE_RECIPIENT`], so no notice is self-addressed.
pub(crate) const NOTICE_SENDER: &str = "legion-cmd";

/// The notice's verb: informational, so it delivers without waking anyone.
const NOTICE_VERB: &str = "info";

/// The directed signal text for a first no-go hit: the command in the note,
/// the entry, agent, repo and session in the details.
pub(crate) fn notice_text(record: &CmdIncidentRecord) -> error::Result<String> {
    let mut note = format!("no-go hit refused: {}", record.command);
    if note.len() > crate::signal::MAX_SIGNAL_NOTE_LENGTH {
        let mut end = crate::signal::MAX_SIGNAL_NOTE_LENGTH;
        while !note.is_char_boundary(end) {
            end -= 1;
        }
        note.truncate(end);
    }
    // The details wire format splits pairs on commas.
    let value = |text: &str| text.replace(',', " ");
    let details = format!(
        "entry:{},agent:{},repo:{},session:{}",
        value(record.entry.as_deref().unwrap_or("")),
        value(&record.agent),
        value(&record.repo),
        value(&record.session_id)
    );
    crate::signal::compose(
        NOTICE_RECIPIENT,
        NOTICE_VERB,
        None,
        Some(&note),
        Some(&details),
        crate::verbs::active_manifest(),
    )
}

/// Sends a first no-go hit's operator notice as a directed legion signal,
/// through the same in-process post the `legion signal` command uses. The
/// sender is always [`NOTICE_SENDER`], never the session's repo: a hit in the
/// operator's own `legion` sessions would otherwise address its sender, which
/// is never delivered. The repo stays in the details.
pub(crate) fn send_notice(record: &CmdIncidentRecord) -> error::Result<()> {
    let (db, index) = crate::cli::util::open_db_and_index()?;
    post_notice(&db, &index, record)?;
    Ok(())
}

/// Posts the notice to the board of `db` and `index`; returns the post id.
fn post_notice(
    db: &crate::db::Database,
    index: &crate::search::SearchIndex,
    record: &CmdIncidentRecord,
) -> error::Result<String> {
    let text = notice_text(record)?;
    crate::board::post_from_text_with_meta(
        db,
        index,
        NOTICE_SENDER,
        &text,
        &crate::db::ReflectionMeta::default(),
    )
}

/// The incident log: one JSONL file in legion's telemetry directory.
#[derive(Debug, Clone)]
pub(crate) struct IncidentLog {
    path: PathBuf,
}

impl IncidentLog {
    /// The log at its canonical telemetry path.
    pub(crate) fn production() -> Self {
        Self {
            path: telemetry::cmd_incident_log_path(),
        }
    }

    /// A log at `path`, for tests.
    #[cfg(test)]
    pub(crate) fn at(path: PathBuf) -> Self {
        Self { path }
    }

    pub(crate) fn records(&self) -> error::Result<Vec<CmdIncidentRecord>> {
        telemetry::list_cmd_incidents(&self.path)
    }

    fn append(&self, record: CmdIncidentRecord) -> error::Result<CmdIncidentRecord> {
        telemetry::append_cmd_incident(&self.path, &record)?;
        Ok(record)
    }

    /// Records a no-go hit. `hit_count` is 1 for the first hit on `entry` in
    /// this session and one more for each repeat (FR-CMD-027's repeat key is
    /// the session and the entry). The first hit sends the operator notice
    /// through `notify` before the row is written; a notice that fails is
    /// logged to stderr and recorded on the row as `notice_error`, and never
    /// fails the record. A repeat sends nothing.
    pub(crate) fn record_no_go(
        &self,
        origin: &Origin,
        entry: &str,
        command_key: Option<&str>,
        now: DateTime<Utc>,
        notify: &dyn Fn(&CmdIncidentRecord) -> error::Result<()>,
    ) -> error::Result<CmdIncidentRecord> {
        let prior: u64 = self
            .records()?
            .iter()
            .filter(|r| {
                r.kind == CmdIncidentKind::NoGo
                    && r.session_id == origin.session_id
                    && r.entry.as_deref() == Some(entry)
            })
            .count() as u64;
        let mut record = new_record(CmdIncidentKind::NoGo, origin, Some(entry), command_key, now);
        record.hit_count = Some(prior + 1);
        if prior == 0
            && let Err(e) = notify(&record)
        {
            eprintln!("[legion] could not send the operator notice for no-go entry `{entry}`: {e}");
            record.notice_error = Some(e.to_string());
        }
        self.append(record)
    }

    /// Records an ask put to the agent.
    pub(crate) fn record_ask(
        &self,
        origin: &Origin,
        entry: Option<&str>,
        command_key: Option<&str>,
        now: DateTime<Utc>,
    ) -> error::Result<CmdIncidentRecord> {
        self.append(new_record(
            CmdIncidentKind::Ask,
            origin,
            entry,
            command_key,
            now,
        ))
    }

    /// Records a confirmation under the confirmation-store row's own `id`, so
    /// a drop can ask the store whether it was used.
    pub(crate) fn record_confirmation(
        &self,
        id: &str,
        origin: &Origin,
        entry: Option<&str>,
        command_key: &str,
        reason: &str,
        now: DateTime<Utc>,
    ) -> error::Result<CmdIncidentRecord> {
        let mut record = new_record(
            CmdIncidentKind::Confirmation,
            origin,
            entry,
            Some(command_key),
            now,
        );
        record.id = id.to_string();
        record.reason = Some(reason.to_string());
        self.append(record)
    }

    /// Writes a drop row for every ask and confirmation that has become a
    /// drop and has none yet: an ask older than [`CONFIRMATION_TTL`] with no
    /// confirmation for the same command in the same session within that
    /// window, and a confirmation older than the TTL that `was_used` says was
    /// never used. Returns how many drops it wrote.
    pub(crate) fn record_pending_drops(
        &self,
        was_used: &dyn Fn(&str) -> error::Result<bool>,
        now: DateTime<Utc>,
    ) -> error::Result<usize> {
        let records = self.records()?;
        let dropped: HashSet<&str> = records
            .iter()
            .filter(|r| r.kind == CmdIncidentKind::Drop)
            .filter_map(|r| r.drop_of.as_deref())
            .collect();
        let mut drops: Vec<CmdIncidentRecord> = Vec::new();
        for record in &records {
            if record.ts + CONFIRMATION_TTL > now || dropped.contains(record.id.as_str()) {
                continue;
            }
            let is_drop = match record.kind {
                CmdIncidentKind::Ask => !records.iter().any(|c| answers(c, record)),
                CmdIncidentKind::Confirmation => !was_used(&record.id)?,
                CmdIncidentKind::NoGo | CmdIncidentKind::Drop => false,
            };
            if is_drop {
                drops.push(CmdIncidentRecord {
                    id: uuid::Uuid::now_v7().to_string(),
                    ts: now,
                    kind: CmdIncidentKind::Drop,
                    reason: None,
                    hit_count: None,
                    drop_of: Some(record.id.clone()),
                    notice_error: None,
                    ..record.clone()
                });
            }
        }
        let written = drops.len();
        for drop in drops {
            self.append(drop)?;
        }
        Ok(written)
    }
}

/// True when `confirmation` answers `ask`: the same command in the same
/// session, confirmed within [`CONFIRMATION_TTL`] of the ask.
fn answers(confirmation: &CmdIncidentRecord, ask: &CmdIncidentRecord) -> bool {
    confirmation.kind == CmdIncidentKind::Confirmation
        && confirmation.session_id == ask.session_id
        && ask.command_key.is_some()
        && confirmation.command_key == ask.command_key
        && confirmation.ts >= ask.ts
        && confirmation.ts <= ask.ts + CONFIRMATION_TTL
}

fn new_record(
    kind: CmdIncidentKind,
    origin: &Origin,
    entry: Option<&str>,
    command_key: Option<&str>,
    now: DateTime<Utc>,
) -> CmdIncidentRecord {
    CmdIncidentRecord {
        id: uuid::Uuid::now_v7().to_string(),
        ts: now,
        kind,
        command: origin.command.clone(),
        agent: origin.agent.clone(),
        repo: origin.repo.clone(),
        session_id: origin.session_id.clone(),
        cwd: origin.cwd.clone(),
        entry: entry.map(str::to_string),
        command_key: command_key.map(str::to_string),
        reason: None,
        hit_count: None,
        drop_of: None,
        notice_error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin(session: &str) -> Origin {
        Origin {
            command: "curl example.com".to_string(),
            agent: "legion".to_string(),
            repo: "legion".to_string(),
            session_id: session.to_string(),
            cwd: "/repo/legion".to_string(),
        }
    }

    fn log() -> (IncidentLog, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        (IncidentLog::at(dir.path().join("cmd-incidents.jsonl")), dir)
    }

    fn never_used(_id: &str) -> error::Result<bool> {
        Ok(false)
    }

    #[test]
    fn a_no_go_hit_writes_one_full_record_and_repeats_count_per_session_and_entry() {
        let (log, _dir) = log();
        let now = Utc::now();
        let sent: std::cell::RefCell<Vec<(String, String)>> = std::cell::RefCell::new(Vec::new());
        let notify = |r: &CmdIncidentRecord| -> error::Result<()> {
            sent.borrow_mut()
                .push((r.session_id.clone(), r.entry.clone().unwrap_or_default()));
            Ok(())
        };
        let hit = |session: &str, entry: &str| {
            log.record_no_go(&origin(session), entry, Some("k"), now, &notify)
                .expect("record")
        };
        let first = hit("s1", "rm-recursive-force-root");
        assert_eq!(first.kind, CmdIncidentKind::NoGo);
        assert_eq!(first.command, "curl example.com");
        assert_eq!(first.agent, "legion");
        assert_eq!(first.repo, "legion");
        assert_eq!(first.session_id, "s1");
        assert_eq!(first.cwd, "/repo/legion");
        assert_eq!(first.ts, now);
        assert_eq!(first.entry.as_deref(), Some("rm-recursive-force-root"));
        assert_eq!(first.hit_count, Some(1));
        assert_eq!(first.notice_error, None);

        // A repeat in the same session on the same entry counts and sends no
        // second notice; another entry or another session notifies again.
        assert_eq!(hit("s1", "rm-recursive-force-root").hit_count, Some(2));
        assert_eq!(hit("s1", "fork-bomb").hit_count, Some(1));
        assert_eq!(hit("s2", "rm-recursive-force-root").hit_count, Some(1));
        assert_eq!(log.records().expect("read").len(), 4);
        assert_eq!(
            sent.borrow().clone(),
            vec![
                ("s1".to_string(), "rm-recursive-force-root".to_string()),
                ("s1".to_string(), "fork-bomb".to_string()),
                ("s2".to_string(), "rm-recursive-force-root".to_string()),
            ]
        );
    }

    #[test]
    fn a_notice_that_fails_is_recorded_on_the_row_and_the_record_still_stands() {
        let (log, _dir) = log();
        let failing = |_r: &CmdIncidentRecord| -> error::Result<()> {
            Err(error::LegionError::Telemetry("inbox down".to_string()))
        };
        let record = log
            .record_no_go(&origin("s1"), "fork-bomb", None, Utc::now(), &failing)
            .expect("the record is written even when the notice fails");
        assert!(
            record
                .notice_error
                .as_deref()
                .is_some_and(|e| e.contains("inbox down"))
        );
        assert_eq!(
            log.records().expect("read")[0].notice_error,
            record.notice_error
        );
    }

    #[test]
    fn the_notice_is_an_info_signal_to_legion_naming_the_entry_and_origin() {
        let (log, _dir) = log();
        let mut other = origin("s9");
        other.repo = "rafters".to_string();
        other.agent = "rafters".to_string();
        let record = log
            .record_no_go(&other, "fork-bomb", None, Utc::now(), &|_| Ok(()))
            .expect("record");
        let text = notice_text(&record).expect("composes");
        let parsed = crate::signal::parse_signal(&text).expect("a signal");
        assert_eq!(parsed.recipient, NOTICE_RECIPIENT);
        assert_eq!(parsed.verb, "info");
        assert!(text.contains("curl example.com"), "{text}");
        for (key, want) in [
            ("entry", "fork-bomb"),
            ("agent", "rafters"),
            ("repo", "rafters"),
            ("session", "s9"),
        ] {
            assert_eq!(
                parsed.details.get(key).map(String::as_str),
                Some(want),
                "{key} in {text}"
            );
        }
    }

    #[test]
    fn a_hit_from_the_legion_repo_produces_a_sendable_notice() {
        // The operator's own sessions run in the `legion` repo. The notice is
        // sent as `legion-cmd`, so it never addresses its own sender, and the
        // repo still travels in the details.
        let (log, _dir) = log();
        let record = log
            .record_no_go(&origin("s1"), "fork-bomb", None, Utc::now(), &|_| Ok(()))
            .expect("record");
        assert_eq!(record.repo, NOTICE_RECIPIENT);
        assert!(!crate::signal::is_self_address(
            &[NOTICE_SENDER.to_string()],
            NOTICE_RECIPIENT
        ));
        let text = notice_text(&record).expect("composes");
        let parsed = crate::signal::parse_signal(&text).expect("a signal");
        assert_eq!(parsed.recipient, NOTICE_RECIPIENT);
        assert_eq!(
            parsed.details.get("repo").map(String::as_str),
            Some("legion")
        );
    }

    #[test]
    fn a_notice_is_posted_as_legion_cmd_to_legion() {
        // The real send path against a temporary data dir: the post lands on
        // the board authored by `legion-cmd`, addressed to `legion`.
        let data = tempfile::tempdir().expect("tempdir");
        let db = crate::db::Database::open(&data.path().join("legion.db")).expect("db");
        let index = crate::search::SearchIndex::open(&data.path().join("index")).expect("index");
        let (log, _dir) = log();
        let record = log
            .record_no_go(&origin("s1"), "fork-bomb", None, Utc::now(), &|_| Ok(()))
            .expect("record");
        let id = post_notice(&db, &index, &record).expect("posted");
        let posted = db.get_reflection_by_id(&id).expect("read").expect("row");
        assert_eq!(posted.repo, NOTICE_SENDER);
        assert!(posted.text.starts_with("@legion info"), "{}", posted.text);
    }

    #[test]
    fn an_ask_and_a_confirmation_write_records_and_the_confirmation_holds_its_reason() {
        let (log, _dir) = log();
        let now = Utc::now();
        let ask = log
            .record_ask(&origin("s1"), Some("curl-ask"), Some("k"), now)
            .expect("record");
        assert_eq!(ask.kind, CmdIncidentKind::Ask);
        assert_eq!(ask.entry.as_deref(), Some("curl-ask"));
        let confirmation = log
            .record_confirmation("c1", &origin("s1"), Some("curl-ask"), "k", "why", now)
            .expect("record");
        assert_eq!(confirmation.id, "c1");
        assert_eq!(confirmation.reason.as_deref(), Some("why"));
    }

    #[test]
    fn an_unanswered_ask_becomes_a_drop_once_after_ten_minutes() {
        let (log, _dir) = log();
        let asked = Utc::now() - Duration::minutes(11);
        let ask = log
            .record_ask(&origin("s1"), Some("curl-ask"), Some("k"), asked)
            .expect("record");
        let now = Utc::now();
        assert_eq!(
            log.record_pending_drops(&never_used, now).expect("drops"),
            1
        );
        // Already recorded: a second run writes nothing.
        assert_eq!(
            log.record_pending_drops(&never_used, now).expect("drops"),
            0
        );
        let records = log.records().expect("read");
        let drop = records
            .iter()
            .find(|r| r.kind == CmdIncidentKind::Drop)
            .expect("drop row");
        assert_eq!(drop.drop_of.as_deref(), Some(ask.id.as_str()));
        assert_eq!(drop.command, ask.command);
        assert_eq!(drop.session_id, "s1");
        assert_eq!(drop.entry.as_deref(), Some("curl-ask"));
    }

    #[test]
    fn an_ask_is_not_a_drop_before_ten_minutes_or_when_answered_in_time() {
        let (log, _dir) = log();
        let now = Utc::now();
        log.record_ask(&origin("s1"), None, Some("k"), now - Duration::minutes(5))
            .expect("record");
        let asked = now - Duration::minutes(20);
        log.record_ask(&origin("s2"), None, Some("k"), asked)
            .expect("record");
        log.record_confirmation(
            "c1",
            &origin("s2"),
            None,
            "k",
            "why",
            asked + Duration::minutes(3),
        )
        .expect("record");
        // The confirmation itself was used, so it is not a drop either.
        let used = |_id: &str| -> error::Result<bool> { Ok(true) };
        assert_eq!(log.record_pending_drops(&used, now).expect("drops"), 0);
    }

    #[test]
    fn a_confirmation_that_expired_unused_becomes_a_drop() {
        let (log, _dir) = log();
        let now = Utc::now();
        log.record_confirmation(
            "c1",
            &origin("s1"),
            None,
            "k",
            "why",
            now - Duration::minutes(11),
        )
        .expect("record");
        assert_eq!(
            log.record_pending_drops(&never_used, now).expect("drops"),
            1
        );
    }

    #[test]
    fn a_record_that_cannot_be_written_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        // The log path is a directory, so the append cannot open it.
        let log = IncidentLog::at(dir.path().to_path_buf());
        assert!(
            log.record_ask(&origin("s1"), None, None, Utc::now())
                .is_err()
        );
    }
}
