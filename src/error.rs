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

    #[error("card not found: {0}")]
    CardNotFound(String),

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

    #[error("invalid card transition: cannot {action} a card in status '{current}'")]
    InvalidCardTransition { action: String, current: String },

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

    #[error("invalid card status: {0}")]
    InvalidCardStatus(String),

    #[error("invalid priority: {0} (expected low, med, high, or critical)")]
    InvalidPriority(String),

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

    #[error("MCP invalid argument: {0}")]
    McpInvalidArgument(String),

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

    #[error("quality gate row not found: {0}")]
    QualityGateNotFound(String),

    #[error("invalid finding severity: '{0}' (expected 'high', 'med', or 'low')")]
    InvalidFindingSeverity(String),

    #[error("invalid finding status: '{0}' (expected 'pending', 'resolved', or 'dispositioned')")]
    InvalidFindingStatus(String),

    #[error("quality gate finding not found: {0}")]
    FindingNotFound(String),

    #[error(
        "finding {0} is already RESOLVED (a later commit demonstrably touched the flagged \
         file) -- a resolved finding needs no disposition"
    )]
    FindingAlreadyResolved(String),

    #[error("delegation refused: {0}")]
    DelegationRefused(String),

    #[error("branch '{branch}' not found in any worktree checkout (searched: {searched})")]
    PushBranchNotFound { branch: String, searched: String },

    #[error("refusing to push '{branch}': {reason}")]
    PushRefused { branch: String, reason: String },

    #[error("git push failed: {stderr}")]
    PushFailed { stderr: String },

    /// `legion push --delete` refused to delete `main`/`master` (#799). No
    /// override exists for this refusal -- unlike the unmerged-branch case,
    /// there is no flag that recovers this path.
    #[error(
        "refusing to delete '{branch}': agents never delete main/master -- no override exists \
         for this refusal"
    )]
    PushDeleteRefusedProtectedRef { branch: String },

    /// `legion push --delete` refused a branch not fully merged (via
    /// `git merge-base --is-ancestor`) into the remote default branch (#799).
    /// `tips` names the commits reachable from the branch but not the
    /// default branch (`git rev-list <default>..<branch>`, char-bounded).
    /// `--force-unmerged` overrides this refusal.
    #[error(
        "refusing to delete '{branch}': not fully merged into the default branch (commits not \
         in default: {tips}) -- pass --force-unmerged to override (operator-facing, not agent \
         doctrine)"
    )]
    PushDeleteRefusedUnmerged { branch: String, tips: String },

    /// The underlying `git push origin --delete` failed (#799); `stderr` is
    /// git's own relayed failure text.
    #[error("git push --delete failed: {stderr}")]
    PushDeleteRemoteFailure { stderr: String },

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

    #[test]
    fn invalid_finding_status_display() {
        let err = LegionError::InvalidFindingStatus("waived".to_string());
        assert!(err.to_string().contains("waived"));
        assert!(err.to_string().contains("pending"));
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
    fn delegation_refused_display() {
        let err = LegionError::DelegationRefused("no live attempt".to_string());
        assert_eq!(err.to_string(), "delegation refused: no live attempt");
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
    fn push_delete_refused_protected_ref_display() {
        let err = LegionError::PushDeleteRefusedProtectedRef {
            branch: "main".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("main"));
        assert!(msg.contains("no override exists"));
    }

    #[test]
    fn push_delete_refused_unmerged_display() {
        let err = LegionError::PushDeleteRefusedUnmerged {
            branch: "feat/x".to_string(),
            tips: "abc1234, def5678".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("feat/x"));
        assert!(msg.contains("abc1234, def5678"));
        assert!(msg.contains("--force-unmerged"));
    }

    #[test]
    fn push_delete_remote_failure_display() {
        let err = LegionError::PushDeleteRemoteFailure {
            stderr: "! [remote rejected]".to_string(),
        };
        assert!(err.to_string().contains("! [remote rejected]"));
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
