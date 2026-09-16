//! Bounded shell tokenizer: finds every command position in a Bash command
//! string, to depth 2, and marks every region it cannot see as opaque
//! (FR-CMD-007, RESEARCH-CMD-bounded-tokenizer).
//!
//! This is a hand-written, bounded scanner, not a shell interpreter. It
//! performs no I/O (NFR-CMD-001): it never reads a file, never executes
//! anything, and its output depends only on the string it is given. Its
//! value is not enforcement -- `permissions.deny` already applies to every
//! subcommand -- but rewrite eligibility, a precise prescription, and a
//! truthful ledger row.

use std::fmt;

use serde::Deserialize;

mod lexer;
mod resolve;
mod tables;
#[cfg(test)]
mod tests;

/// The deepest a substitution, inline shell, heredoc shell, `find -exec`, or
/// function body may nest before the region becomes [`Opaque::TooDeep`]
/// (FR-CMD-007).
pub const MAX_DEPTH: u8 = 2;

/// How a command position was reached (FR-CMD-007).
///
/// Declaration order doubles as a strength ordering (via the derived
/// [`Ord`]): when a single command position is reached through more than
/// one route at once (e.g. a wrapped command inside a `find -exec`), the
/// stronger label names the most advanced feature in play.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Position {
    First,
    AfterOperator,
    AfterAssignment,
    Wrapper,
    InlineShell,
    HeredocShell,
    FindExec,
    FunctionBody,
    Substitution,
}

impl Position {
    fn strongest(self, other: Position) -> Position {
        self.max(other)
    }
}

impl fmt::Display for Position {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Position::First => "first",
            Position::AfterOperator => "after-operator",
            Position::AfterAssignment => "after-assignment",
            Position::Wrapper => "wrapper",
            Position::InlineShell => "inline-shell",
            Position::HeredocShell => "heredoc-shell",
            Position::FindExec => "find-exec",
            Position::FunctionBody => "function-body",
            Position::Substitution => "substitution",
        };
        f.write_str(s)
    }
}

/// A region the scanner cannot see into (FR-CMD-007). Recorded, never
/// silently dropped: route proxies it with reason opaque (#1227) and
/// records it coverage-unknown, never a silent allow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opaque {
    /// An interpreter program body (`python -c`, `node -e`, an `awk`
    /// program, ...). `body` is kept so route's search-shaped patterns can
    /// match against it (FR-CMD-007, #1227); the scanner itself never
    /// interprets it.
    Interpreter { interpreter: String, body: String },
    /// A separate script file (`bash script.sh`, `./x.sh`, `node x.mjs`).
    ScriptFile,
    /// `eval` and its argument.
    Eval,
    /// A command name built at runtime: a bare `$VAR`, `$@`, `$*`, or a
    /// whole-word substitution used as the command itself.
    DynamicCommand,
    /// An interpreter or shell reading its program from stdin or a heredoc,
    /// with no inline code and no script file named.
    StdinScript,
    /// `source` / `.` of a file or a process substitution.
    Sourced,
    /// A git `-c alias.<name>=<value>` shell alias, or an `alias` builtin.
    Alias,
    /// A launcher this scanner recognizes as wrapper-shaped but does not
    /// carry in FR-CMD-007's wrapper table: recorded opaque rather than
    /// silently resolved into the wrong position.
    UnknownWrapper,
    /// Beyond [`MAX_DEPTH`].
    TooDeep,
}

/// A resolved command invocation (FR-CMD-007).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// Basename of the executable: quotes and a leading backslash escape
    /// are already removed by the scanner.
    pub binary: String,
    pub args: Vec<String>,
    pub position: Position,
    pub depth: u8,
}

/// `scan`'s result: every invocation it resolved and every region it could
/// not see into.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    pub invocations: Vec<Invocation>,
    pub opaque: Vec<Opaque>,
}

/// A malformed command. `scan` returns this instead of a partial [`Scan`]
/// (FR-CMD-007): a parse error routes ask, never a silent guess.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum ScanError {
    #[error("unterminated single quote starting at byte {offset}")]
    UnterminatedSingleQuote { offset: usize },

    #[error("unterminated double quote starting at byte {offset}")]
    UnterminatedDoubleQuote { offset: usize },

    #[error("unterminated backtick substitution starting at byte {offset}")]
    UnterminatedBacktick { offset: usize },

    #[error("unterminated command or process substitution starting at byte {offset}")]
    UnterminatedSubstitution { offset: usize },

    #[error("heredoc `{delimiter}` starting at byte {offset} was never closed")]
    UnterminatedHeredoc { delimiter: String, offset: usize },

    #[error("a `<<` at byte {offset} names no heredoc delimiter")]
    MissingHeredocDelimiter { offset: usize },

    #[error("a function body opened with `{{` at byte {offset} was never closed")]
    UnterminatedFunctionBody { offset: usize },
}

/// Resolves every command position in `command`, to [`MAX_DEPTH`], and
/// records every region it cannot see as [`Opaque`] (FR-CMD-007). Pure: no
/// filesystem, network, database, environment, or process access
/// (NFR-CMD-001). Never panics on any input, including invalid UTF-8
/// slicing boundaries and unterminated constructs -- it returns
/// [`ScanError`] instead.
pub fn scan(command: &str) -> Result<Scan, ScanError> {
    let mut out = Scan::default();
    resolve::walk(command, 0, None, &mut out)?;
    Ok(out)
}
