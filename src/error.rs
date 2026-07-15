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

    #[error("delegation refused: {0}")]
    DelegationRefused(String),

    #[error("branch '{branch}' not found in any worktree checkout (searched: {searched})")]
    PushBranchNotFound { branch: String, searched: String },

    #[error("refusing to push '{branch}': {reason}")]
    PushRefused { branch: String, reason: String },

    #[error("git push failed: {stderr}")]
    PushFailed { stderr: String },

    /// Signals that the process should exit with a specific non-zero code.
    ///
    /// Used by CLI handlers that have already printed a user-facing message
    /// and need to propagate a specific exit code back to `main()` without
    /// calling `std::process::exit` at the call site. `main()` intercepts
    /// this variant and calls `std::process::exit(code)` -- the only
    /// legitimate use of `process::exit` in the binary.
    #[error("")]
    ExitWith(i32),
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
}
