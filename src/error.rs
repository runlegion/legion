use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LegionError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("search index error: {0}")]
    Search(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("no reflection text provided (use --text or --transcript)")]
    NoReflectionInput,

    #[error("transcript file not found: {0}")]
    TranscriptNotFound(PathBuf),

    #[error("one or more repos failed during compound reflect")]
    ReflectPartialFailure,

    #[error("home directory not available")]
    NoHomeDir,

    #[error("daemon did not stop: {0}")]
    DaemonStopFailed(String),

    #[error("daemon port in use: {0}")]
    DaemonPortInUse(String),

    #[error("embedding error: {0}")]
    Embedding(String),

    #[error("task not found: {0}")]
    TaskNotFound(String),

    #[error("invalid state transition: cannot {action} a task with status '{current}'")]
    InvalidTaskTransition { action: String, current: String },

    #[error("reflection not found: {0}")]
    ReflectionNotFound(String),

    #[error(
        "repo safety check failed: reflection {id} belongs to '{actual}', not '{expected}' -- re-run without --repo or with the correct value"
    )]
    ReflectionRepoMismatch {
        id: String,
        actual: String,
        expected: String,
    },

    #[error(
        "repo '{repo}' already has a live identity root ({existing_id}) -- a second, \
         unparented identity write is exactly the leak vector that let a stray checkpoint \
         outrank the real identity in the boot banner. Either chain onto the existing root \
         (`legion reflect --whoami --follows {existing_id} --text \"...\"`) or replace it \
         deliberately (`legion forget --id {existing_id}` then re-run `legion reflect --whoami`). \
         Find the root again any time via `legion recall --repo {repo} --domain identity --limit 1`."
    )]
    IdentityRootExists { repo: String, existing_id: String },

    #[error(
        "refusing to retag {id}: it is the last live identity root for '{repo}'. Retagging it \
         would leave the repo with zero identity -- the same failure #785 guards against on \
         insert, which never fires on UPDATE; this refusal closes that third path. To replace \
         the identity deliberately, use `legion whoami --repo {repo} --generate` (gather, then \
         --apply), which swaps the root atomically."
    )]
    RetagLastIdentityRoot { id: String, repo: String },

    #[error(
        "refusing to retag {id}: it is the last live workflow root for '{repo}'. Retagging it \
         would leave the repo with zero operating contract (an empty whatami banner). If the \
         content is genuinely dead, archive it instead (`legion forget --id {id} --persist`); \
         if you are replacing the contract, store the new root first (`legion reflect --repo \
         {repo} --domain workflow --text \"...\"`), then retag this one."
    )]
    RetagLastWorkflowRoot { id: String, repo: String },

    #[error("work source error: {0}")]
    WorkSource(String),

    #[error("server error: {0}")]
    Server(String),

    #[error("invalid cron expression: {0}")]
    InvalidCron(String),

    #[error("schedule not found: {0}")]
    ScheduleNotFound(String),

    #[error(
        "signal note too long ({len} chars, max {max}). Signals are pings, not essays. Post the content first with `legion post`, then signal with a short note."
    )]
    SignalNoteTooLong { len: usize, max: usize },

    #[error(
        "--repo and --to must differ: '{repo}' is the authoring repo context, not the recipient. \
         To signal {repo}, use: legion signal --repo <your-repo> --to {repo} ..."
    )]
    SignalSelfAddressed { repo: String },

    #[error("watch config error: {0}")]
    WatchConfig(String),

    #[error("etc error: {0}")]
    Etc(String),

    #[error("whoami --generate error: {0}")]
    WhoamiGenerate(String),

    #[error("watch already running (pid {0})")]
    WatchAlreadyRunning(u32),

    #[error("index already running for {repo} (pid {pid})")]
    IndexAlreadyRunning { repo: String, pid: u32 },

    #[error(
        "signal verb '{verb}' requires the following detail field(s) that are missing: {missing}. \
         Pass them via --details '{missing_example}:value'"
    )]
    SignalMissingRequiredFields {
        verb: String,
        missing: String,
        missing_example: String,
    },

    #[error("TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("health error: {0}")]
    Health(String),

    #[error("cluster config error: {0}")]
    Config(String),

    #[error("mesh error: {0}")]
    Mesh(String),

    #[error("css error: {0}")]
    Css(String),

    #[error(
        "indexer not found: '{binary}' is not on PATH (required for {lang} indexing). For Rust, install rust-analyzer (`rustup component add rust-analyzer`) which provides `rust-analyzer scip`; the legacy scip-rust repo is archived."
    )]
    IndexerNotFound { lang: String, binary: String },

    #[error("indexer failed for {lang}: {stderr}")]
    IndexerFailed { lang: String, stderr: String },

    #[error("telemetry error: {0}")]
    Telemetry(String),

    #[error("not implemented: {feature}")]
    #[allow(dead_code)] // general-purpose stub variant; constructors come and go
    NotImplemented { feature: String },

    #[error("pty spawn failed for {bin:?}: {source}")]
    PtySpawnFailed {
        bin: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("pty allocation failed: {0}")]
    PtyAllocFailed(String),

    #[error("pty write failed: {0}")]
    PtyWriteFailed(String),

    #[error("pty wait failed: {0}")]
    PtyWaitFailed(String),

    #[error("cannot send keystrokes to a non-interactive (print-mode) child")]
    PtyControlUnsupported,

    #[error(
        "illegal wake attempt transition for {attempt_id}: {from} -> {to} (current state: {current})"
    )]
    IllegalWakeAttemptTransition {
        attempt_id: String,
        from: String,
        to: String,
        current: String,
    },

    #[error("wake attempt not found: {0}")]
    WakeAttemptNotFound(String),

    #[error("wake attempt state decode error: {0}")]
    WakeAttemptStateDecodeError(String),

    #[error("invalid gate result: '{0}' (expected 'clean' or 'issues')")]
    InvalidGateResult(String),

    #[error("invalid gate provenance: '{0}' (expected 'validated' or 'asserted')")]
    InvalidGateProvenance(String),

    #[error(
        "quality gate row not found: {0} \
         (`quality-gate list --json` carries the full id)"
    )]
    QualityGateNotFound(String),

    #[error("invalid finding severity: '{0}' (expected 'high', 'med', or 'low')")]
    InvalidFindingSeverity(String),

    #[error(
        "invalid finding status: '{0}' (expected 'pending', 'resolved', 'dispositioned', or \
         'voided')"
    )]
    InvalidFindingStatus(String),

    #[error(
        "quality gate finding not found: {0} \
         (`quality-gate finding-list --json` carries the full id)"
    )]
    FindingNotFound(String),

    /// A partial finding id matched more than one row. Never resolved by
    /// picking one: disposition is a state change, and silently retiring the
    /// wrong row is worse than making the caller disambiguate.
    ///
    /// Each candidate string carries the full id AND its `file:line`, because
    /// findings recorded inside one millisecond share their leading 24
    /// characters -- a list of ids alone would be seven near-identical UUIDs
    /// the caller still could not choose between, which is the same dead end
    /// this error exists to end.
    #[error(
        "finding id '{prefix}' is ambiguous ({} matches):\n  - {}",
        .candidates.len(),
        .candidates.join("\n  - ")
    )]
    FindingIdAmbiguous {
        prefix: String,
        candidates: Vec<String>,
    },

    /// A partial gate id matched more than one row. Same rule as
    /// `FindingIdAmbiguous`; each candidate carries the full id plus its
    /// skill and commit, the two fields `quality-gate list` already shows.
    #[error(
        "gate id '{prefix}' is ambiguous ({} matches):\n  - {}",
        .candidates.len(),
        .candidates.join("\n  - ")
    )]
    QualityGateIdAmbiguous {
        prefix: String,
        candidates: Vec<String>,
    },

    #[error(
        "finding {0} is already RESOLVED (a later commit demonstrably touched the flagged \
         file) -- a resolved finding needs no disposition"
    )]
    FindingAlreadyResolved(String),

    /// #1126 review MED1: `dispose_finding`'s terminal-status guard refused
    /// only RESOLVED, so an operator could disposition a VOIDED finding and
    /// clobber `disposition_reason` -- which was holding the void reason --
    /// with a fabricated "someone judged this and waived it" story. A
    /// distinct variant from `FindingAlreadyResolved` rather than reusing it
    /// or a shared/generic terminal-status error, because "already
    /// RESOLVED" on a voided finding would itself be a small lie: the two
    /// terminal states are voided for different reasons and the message
    /// must name which one actually blocked the call.
    #[error(
        "finding {0} is already VOIDED (the gate run that raised it was declared not-evidence) \
         -- a voided finding needs no disposition"
    )]
    FindingAlreadyVoided(String),

    #[error("branch '{branch}' not found in any worktree checkout (searched: {searched})")]
    PushBranchNotFound { branch: String, searched: String },

    #[error("refusing to push '{branch}': {reason}")]
    PushRefused { branch: String, reason: String },

    #[error("git push failed: {stderr}")]
    PushFailed { stderr: String },

    /// `legion commit` (#854) declined before the commit ran: bad
    /// arguments, no git repo, a message-convention violation, or git
    /// itself refusing the signer probe (an unresolvable committer identity
    /// is the ordinary way -- git dies there without reaching the signer,
    /// which makes it a refusal rather than a signing failure). The reason
    /// is always specific enough to fix without a second look at the docs --
    /// a refusal the caller cannot act on is just a failure.
    #[error("refusing to commit: {reason}")]
    CommitRefused { reason: String },

    /// The configured commit signer could not sign (#854). Raised by the
    /// preflight probe BEFORE the commit runs, so a locked signer costs one
    /// named error instead of a retry loop against a hardware key.
    ///
    /// The remedy names two possibilities on purpose. A signer that exits
    /// non-zero has not told us WHY -- a locked agent and a misconfigured
    /// key look identical from here -- and a message that asserts "unlock
    /// your signer" sends everyone whose real problem is the configuration
    /// to go poke at an agent that was never locked. `detail` carries git's
    /// own output, which is the part that actually discriminates.
    #[error(
        "signing unavailable ({program}): {detail}\nthe configured signer could not sign; \
         unlock your signer or fix the signing configuration (git's output above names the cause)"
    )]
    CommitSigningUnavailable { program: String, detail: String },

    #[error("git commit failed: {stderr}")]
    CommitFailed { stderr: String },

    /// Signals that the process should exit with a specific non-zero code.
    ///
    /// Used by CLI handlers that have already printed a user-facing message
    /// and need to propagate a specific exit code back to `main()` without
    /// calling `std::process::exit` at the call site. `main()` intercepts
    /// this variant and calls `std::process::exit(code)` -- the only
    /// legitimate use of `process::exit` in the binary.
    #[error("")]
    ExitWith(i32),

    /// A `--since`/`--until`/`--on` date filter value did not match the
    /// accepted grammar (#786). Raised at the CLI/API boundary by
    /// `TimeRange::parse` -- nothing reaches the query layer with an
    /// unparsed date.
    #[error("unparseable date '{input}' -- accepted: YYYY-MM-DD, <N>d, <N>w, today, yesterday")]
    InvalidDateFilter { input: String },

    /// `legion kanban defer --until <input>` parsed to a time that is not
    /// after now (#816). Names both the raw input and the resolved wake_at
    /// so a stale absolute date or a same-day `today` is diagnosable from
    /// the error alone.
    #[error("cannot defer: '{input}' resolved to {wake_at}, which is not in the future")]
    DeferWakeAtInPast { input: String, wake_at: String },

    /// A document payload failed JSON Schema validation for its `doc_type`
    /// (#1062). Distinct from `WorkSource` so the daemon can answer 422 with
    /// the violation list instead of the blanket 500 an opaque failure would
    /// get -- `channel.rs`'s per-endpoint error mapper matches on this
    /// variant before falling back to the `WorkSource`/blanket rules.
    #[error("document payload violates schema {schema_id}: {} error(s)", .errors.len())]
    SchemaViolation {
        schema_id: String,
        /// One line per violation: "<json pointer>: <message>", e.g.
        /// "/verification/acceptance: expected array, got string". The
        /// pointer is empty for a violation rooted at the payload itself
        /// (e.g. a missing required top-level property).
        errors: Vec<String>,
    },

    /// `legion cmd-check`'s embedded route table failed to load (#1042).
    /// Should never fire in practice -- the embedded TOML is covered by
    /// legion-cmd's own tests -- but `Router::new`/`RouteTable::embedded`
    /// both return `Result`, so this closes the path without an `unwrap`.
    #[error("legion-cmd router error: {0}")]
    CmdCheckRouter(#[from] legion_cmd::TableError),

    /// `legion cmd-check` could not resolve `--tool`/`<input>` to a
    /// `ToolCall` (#1042): an unrecognized `--tool` name, or `<input>` that
    /// does not parse as the JSON a non-Bash tool's input requires.
    #[error("cmd-check error: {0}")]
    CmdCheck(#[from] legion_cmd::CmdCheckError),
}

pub type Result<T> = std::result::Result<T, LegionError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_messages() {
        let err = LegionError::NoReflectionInput;
        assert_eq!(
            err.to_string(),
            "no reflection text provided (use --text or --transcript)"
        );
    }

    #[test]
    fn error_from_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        let err: LegionError = io_err.into();
        assert!(matches!(err, LegionError::Io(_)));
    }

    #[test]
    fn error_from_json() {
        let json_err = serde_json::from_str::<String>("not json").unwrap_err();
        let err: LegionError = json_err.into();
        assert!(matches!(err, LegionError::Json(_)));
    }

    #[test]
    fn error_from_rusqlite() {
        let db_err = rusqlite::Error::InvalidParameterName("bad".to_string());
        let err: LegionError = db_err.into();
        assert!(matches!(err, LegionError::Database(_)));
    }

    #[test]
    fn error_display_transcript_not_found() {
        let err = LegionError::TranscriptNotFound(PathBuf::from("/tmp/missing.jsonl"));
        assert_eq!(
            err.to_string(),
            "transcript file not found: /tmp/missing.jsonl"
        );
    }

    #[test]
    fn error_display_search() {
        let err = LegionError::Search("index corrupted".to_string());
        assert_eq!(err.to_string(), "search index error: index corrupted");
    }

    #[test]
    fn error_display_signal_note_too_long() {
        let err = LegionError::SignalNoteTooLong { len: 500, max: 280 };
        let msg = err.to_string();
        assert!(msg.contains("500 chars"));
        assert!(msg.contains("max 280"));
        assert!(msg.contains("legion post"));
    }

    #[test]
    fn result_type_alias_works() {
        let ok: Result<i32> = Ok(42);
        assert!(ok.is_ok());

        let err: Result<i32> = Err(LegionError::NoHomeDir);
        assert!(err.is_err());
    }

    #[test]
    fn exit_with_displays_empty() {
        // main() intercepts ExitWith and exits without printing; the empty
        // Display is the contract that keeps a stray eprintln!("{e}") from
        // emitting a blank-prefixed error line if a future refactor reorders
        // the intercept below the generic printer.
        assert_eq!(LegionError::ExitWith(1).to_string(), "");
        assert_eq!(LegionError::ExitWith(2).to_string(), "");
    }

    #[test]
    fn error_display_etc() {
        let err = LegionError::Etc("field 'foo' not found in 'x.json'".to_string());
        assert_eq!(
            err.to_string(),
            "etc error: field 'foo' not found in 'x.json'"
        );
    }

    #[test]
    fn invalid_gate_result_display() {
        let err = LegionError::InvalidGateResult("bad".to_string());
        assert!(err.to_string().contains("bad"));
        assert!(err.to_string().contains("clean"));
        assert!(err.to_string().contains("issues"));
    }

    #[test]
    fn invalid_gate_provenance_display() {
        let err = LegionError::InvalidGateProvenance("bad".to_string());
        assert!(err.to_string().contains("bad"));
        assert!(err.to_string().contains("validated"));
        assert!(err.to_string().contains("asserted"));
    }

    #[test]
    fn quality_gate_not_found_display() {
        let err = LegionError::QualityGateNotFound("gate-id-1".to_string());
        assert!(err.to_string().contains("gate-id-1"));
    }

    #[test]
    fn invalid_finding_severity_display() {
        let err = LegionError::InvalidFindingSeverity("critical".to_string());
        assert!(err.to_string().contains("critical"));
        assert!(err.to_string().contains("high"));
    }

    /// Pins the full enumerated set in the message, not just one member --
    /// the prior wording silently dropped "voided" after `FindingStatus`
    /// grew that variant (#1126 review MED2) and a weaker assertion here
    /// (checking only "pending") did not catch it across two review passes.
    #[test]
    fn invalid_finding_status_display() {
        let err = LegionError::InvalidFindingStatus("waived".to_string());
        let msg = err.to_string();
        assert!(msg.contains("waived"));
        for status in ["pending", "resolved", "dispositioned", "voided"] {
            assert!(
                msg.contains(status),
                "expected the error to name every valid status, missing '{status}': {msg}"
            );
        }
    }

    #[test]
    fn finding_not_found_display() {
        let err = LegionError::FindingNotFound("finding-1".to_string());
        assert!(err.to_string().contains("finding-1"));
    }

    #[test]
    fn finding_already_resolved_display() {
        let err = LegionError::FindingAlreadyResolved("finding-2".to_string());
        assert!(err.to_string().contains("finding-2"));
        assert!(err.to_string().contains("RESOLVED"));
    }

    #[test]
    fn finding_already_voided_display() {
        let err = LegionError::FindingAlreadyVoided("finding-3".to_string());
        assert!(err.to_string().contains("finding-3"));
        assert!(err.to_string().contains("VOIDED"));
    }

    #[test]
    fn push_branch_not_found_display() {
        let err = LegionError::PushBranchNotFound {
            branch: "feat/x".to_string(),
            searched: "/a, /b".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("feat/x"));
        assert!(msg.contains("/a, /b"));
    }

    #[test]
    fn push_refused_display() {
        let err = LegionError::PushRefused {
            branch: "main".to_string(),
            reason: "agents never push main".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("main"));
        assert!(msg.contains("agents never push main"));
    }

    #[test]
    fn push_failed_display() {
        let err = LegionError::PushFailed {
            stderr: "! [rejected]".to_string(),
        };
        assert!(err.to_string().contains("! [rejected]"));
    }

    #[test]
    fn commit_refused_display() {
        let err = LegionError::CommitRefused {
            reason: "commit message is empty".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "refusing to commit: commit message is empty"
        );
    }

    #[test]
    fn commit_signing_unavailable_display() {
        let err = LegionError::CommitSigningUnavailable {
            program: "/usr/bin/op-ssh-sign".to_string(),
            detail: "agent refused operation".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.starts_with("signing unavailable (/usr/bin/op-ssh-sign):"));
        assert!(msg.contains("agent refused operation"));
        // Both remedies, not just the lock: the probe cannot tell a locked
        // agent from a misconfigured signer, so naming only one of them
        // sends half the callers to the wrong place.
        assert!(msg.contains("unlock your signer"), "got: {msg}");
        assert!(msg.contains("fix the signing configuration"), "got: {msg}");
        // The remedy starts its own line: `detail` is relayed git stderr,
        // often multi-line, and a remedy glued to its last line reads as
        // part of git's output rather than as ours.
        assert!(
            msg.contains("agent refused operation\nthe configured signer"),
            "got: {msg}"
        );
    }

    #[test]
    fn commit_failed_display() {
        let err = LegionError::CommitFailed {
            stderr: "nothing to commit".to_string(),
        };
        assert!(err.to_string().contains("nothing to commit"));
    }

    #[test]
    fn defer_wake_at_in_past_display() {
        let err = LegionError::DeferWakeAtInPast {
            input: "yesterday".to_string(),
            wake_at: "2026-07-01T00:00:00+00:00".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("yesterday"));
        assert!(msg.contains("2026-07-01T00:00:00+00:00"));
        assert!(msg.contains("not in the future"));
    }
}
