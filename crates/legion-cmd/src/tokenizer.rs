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

/// The deepest a substitution, inline shell, heredoc shell, `find -exec`, or
/// function body may nest before the region becomes [`Opaque::TooDeep`]
/// (FR-CMD-007).
pub const MAX_DEPTH: u8 = 2;

/// How a command position was reached (FR-CMD-007).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// A fixed strength ordering used only to pick one label when a single
    /// command position is reached through more than one route at once
    /// (e.g. a wrapped command inside a `find -exec`): the label names the
    /// most advanced feature in play, matching the order this type declares
    /// its variants in.
    fn rank(self) -> u8 {
        match self {
            Position::First => 0,
            Position::AfterOperator => 1,
            Position::AfterAssignment => 2,
            Position::Wrapper => 3,
            Position::InlineShell => 4,
            Position::HeredocShell => 5,
            Position::FindExec => 6,
            Position::FunctionBody => 7,
            Position::Substitution => 8,
        }
    }

    fn strongest(self, other: Position) -> Position {
        if self.rank() >= other.rank() {
            self
        } else {
            other
        }
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
    /// carry in [FR-CMD-007]'s wrapper table: recorded opaque rather than
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
    walk(command, 0, None, &mut out)?;
    Ok(out)
}

// ---------------------------------------------------------------------
// Word-level scanning: quotes, escapes, substitutions, heredocs.
// ---------------------------------------------------------------------

/// One shell word. `text` is the word's unquoted spelling: the scanner
/// expands nothing, it only strips quote and escape syntax.
#[derive(Debug, Clone, Default)]
struct Word {
    text: String,
    /// Any part of the word was quoted or backslash-escaped.
    quoted: bool,
    /// The word is exactly one command substitution or backtick
    /// substitution, and nothing else -- `$(which rg)` as a whole word.
    whole_subst: bool,
    /// Inner source text of each `$()`, backtick, or `<()`/`>()` inside the
    /// word, in order.
    substs: Vec<String>,
}

#[derive(Debug, Clone)]
enum Tok {
    Word(Word),
    /// `;` `&` `&&` `||` `|` `|&` `;;` newline `(` `)`. The operator's own
    /// spelling never matters downstream -- only that a boundary occurred.
    Sep,
    /// A redirect operator; the next word is its target, not an argument.
    Redirect,
    /// A heredoc body, attached to the command whose `<<` introduced it.
    Heredoc {
        body: String,
    },
    /// `function NAME { BODY }` or `NAME() { BODY }`: a definition, not a
    /// call (hardening: `NAME()` names a definition, not an invocation).
    FunctionDef {
        body: String,
    },
}

fn is_meta(ch: char) -> bool {
    matches!(ch, ';' | '&' | '|' | '(' | ')' | '<' | '>' | '\n')
}

struct Scanner {
    c: Vec<char>,
    i: usize,
}

impl Scanner {
    fn new(src: &str) -> Self {
        Scanner {
            c: src.chars().collect(),
            i: 0,
        }
    }

    fn peek(&self, off: usize) -> Option<char> {
        self.c.get(self.i + off).copied()
    }

    fn starts(&self, pat: &str) -> bool {
        pat.chars()
            .enumerate()
            .all(|(k, ch)| self.peek(k) == Some(ch))
    }

    /// The byte offset of char index `idx`, for error reporting. Computed
    /// rather than tracked incrementally so it is only paid for on the
    /// (rare) error path.
    fn byte_offset(&self, idx: usize) -> usize {
        self.c[..idx.min(self.c.len())]
            .iter()
            .map(|c| c.len_utf8())
            .sum()
    }

    fn err_at(&self, idx: usize) -> usize {
        self.byte_offset(idx)
    }

    fn run(&mut self) -> Result<Vec<Tok>, ScanError> {
        let mut toks = Vec::new();
        let mut pending: Vec<(String, bool, usize)> = Vec::new();
        let mut at_token_start = true;
        while let Some(ch) = self.peek(0) {
            match ch {
                ' ' | '\t' | '\r' => {
                    self.i += 1;
                    at_token_start = true;
                }
                '\\' if self.peek(1) == Some('\n') => self.i += 2,
                '\n' => {
                    self.i += 1;
                    toks.push(Tok::Sep);
                    self.read_heredoc_bodies(&mut pending, &mut toks)?;
                    at_token_start = true;
                }
                '#' if at_token_start => {
                    while let Some(c) = self.peek(0) {
                        if c == '\n' {
                            break;
                        }
                        self.i += 1;
                    }
                }
                _ if at_token_start && self.starts_function_def() => {
                    let body = self.read_function_def()?;
                    toks.push(Tok::FunctionDef { body });
                    at_token_start = false;
                }
                ';' | '&' | '|' => {
                    let two: String = [Some(ch), self.peek(1)].into_iter().flatten().collect();
                    let op = if matches!(two.as_str(), "&&" | "||" | ";;" | "|&") {
                        two
                    } else if ch == '&' && self.peek(1) == Some('>') {
                        self.i += if self.peek(2) == Some('>') { 3 } else { 2 };
                        toks.push(Tok::Redirect);
                        continue;
                    } else {
                        ch.to_string()
                    };
                    self.i += op.chars().count();
                    toks.push(Tok::Sep);
                    at_token_start = true;
                }
                '(' | ')' => {
                    self.i += 1;
                    toks.push(Tok::Sep);
                    at_token_start = true;
                }
                '<' | '>' => {
                    self.read_redirect(&mut pending, &mut toks)?;
                    at_token_start = true;
                }
                _ => {
                    if let Some(w) = self.read_word()? {
                        toks.push(Tok::Word(w));
                    }
                    at_token_start = false;
                }
            }
        }
        if !pending.is_empty() {
            let (delim, _, _) = &pending[0];
            return Err(ScanError::UnterminatedHeredoc {
                delimiter: delim.clone(),
                offset: self.err_at(self.i),
            });
        }
        Ok(toks)
    }

    fn read_redirect(
        &mut self,
        pending: &mut Vec<(String, bool, usize)>,
        toks: &mut Vec<Tok>,
    ) -> Result<(), ScanError> {
        if (self.starts("<(") || self.starts(">(")) && !self.starts("<<") {
            self.i += 2;
            let inner = self.read_balanced_parens()?;
            let w = Word {
                text: format!("<({inner})"),
                substs: vec![inner],
                ..Default::default()
            };
            toks.push(Tok::Word(w));
            return Ok(());
        }
        if self.starts("<<<") {
            self.i += 3;
            toks.push(Tok::Redirect);
            return Ok(());
        }
        if self.starts("<<") {
            let op_offset = self.i;
            self.i += 2;
            let strip = self.peek(0) == Some('-');
            if strip {
                self.i += 1;
            }
            while matches!(self.peek(0), Some(' ' | '\t')) {
                self.i += 1;
            }
            let delim = self.read_heredoc_delimiter();
            if delim.is_empty() {
                return Err(ScanError::MissingHeredocDelimiter {
                    offset: self.err_at(op_offset),
                });
            }
            let slot = toks.len();
            toks.push(Tok::Heredoc {
                body: String::new(),
            });
            pending.push((delim, strip, slot));
            return Ok(());
        }
        let mut op = String::new();
        while let Some(c) = self.peek(0) {
            if matches!(c, '<' | '>' | '&' | '|') && op.len() < 3 {
                op.push(c);
                self.i += 1;
            } else {
                break;
            }
        }
        toks.push(Tok::Redirect);
        Ok(())
    }

    /// A heredoc delimiter word: unquoted, single-, or double-quoted, with
    /// quoting stripped. Quoting only controls expansion inside the body,
    /// which this scanner never performs, so only the bare text matters.
    fn read_heredoc_delimiter(&mut self) -> String {
        let mut delim = String::new();
        while let Some(c) = self.peek(0) {
            if c.is_whitespace() || is_meta(c) {
                break;
            }
            match c {
                '\'' | '"' => {
                    self.i += 1;
                    while let Some(q) = self.peek(0) {
                        self.i += 1;
                        if q == c {
                            break;
                        }
                        delim.push(q);
                    }
                }
                '\\' => {
                    self.i += 1;
                    if let Some(n) = self.peek(0) {
                        delim.push(n);
                        self.i += 1;
                    }
                }
                _ => {
                    delim.push(c);
                    self.i += 1;
                }
            }
        }
        delim
    }

    fn read_heredoc_bodies(
        &mut self,
        pending: &mut Vec<(String, bool, usize)>,
        toks: &mut [Tok],
    ) -> Result<(), ScanError> {
        let taken = std::mem::take(pending);
        for (delim, strip, slot) in taken {
            let start_offset = self.err_at(self.i);
            let mut body = String::new();
            let mut closed = false;
            while self.i < self.c.len() {
                let start = self.i;
                while self.i < self.c.len() && self.c[self.i] != '\n' {
                    self.i += 1;
                }
                let line: String = self.c[start..self.i].iter().collect();
                if self.i < self.c.len() {
                    self.i += 1;
                }
                let cmp = if strip {
                    line.trim_start_matches('\t')
                } else {
                    line.as_str()
                };
                if cmp == delim {
                    closed = true;
                    break;
                }
                body.push_str(&line);
                body.push('\n');
            }
            if !closed {
                return Err(ScanError::UnterminatedHeredoc {
                    delimiter: delim,
                    offset: start_offset,
                });
            }
            if let Some(Tok::Heredoc { body: b }) = toks.get_mut(slot) {
                *b = body;
            }
        }
        Ok(())
    }

    fn read_word(&mut self) -> Result<Option<Word>, ScanError> {
        let mut w = Word::default();
        let start = self.i;
        let mut substs_at_start = 0usize;
        while let Some(ch) = self.peek(0) {
            if ch == ' ' || ch == '\t' || ch == '\r' || is_meta(ch) {
                if matches!(ch, '<' | '>')
                    && !w.quoted
                    && !w.text.is_empty()
                    && w.text.chars().all(|c| c.is_ascii_digit())
                    && self.i - start == w.text.len()
                {
                    // `2>` / `2>&1`: an all-digit word glued to a redirect
                    // is a file descriptor, not an argument -- drop it
                    // entirely and let the redirect reader run next.
                    return Ok(None);
                }
                break;
            }
            match ch {
                '\\' => {
                    if self.peek(1) == Some('\n') {
                        self.i += 2;
                        continue;
                    }
                    w.quoted = true;
                    if let Some(n) = self.peek(1) {
                        w.text.push(n);
                    }
                    self.i += 2;
                }
                '\'' => {
                    let quote_offset = self.i;
                    w.quoted = true;
                    self.i += 1;
                    let mut closed = false;
                    while let Some(c) = self.peek(0) {
                        self.i += 1;
                        if c == '\'' {
                            closed = true;
                            break;
                        }
                        w.text.push(c);
                    }
                    if !closed {
                        return Err(ScanError::UnterminatedSingleQuote {
                            offset: self.err_at(quote_offset),
                        });
                    }
                }
                '"' => {
                    let quote_offset = self.i;
                    w.quoted = true;
                    self.i += 1;
                    self.read_double(&mut w, quote_offset)?;
                }
                '`' => {
                    let tick_offset = self.i;
                    self.i += 1;
                    let mut inner = String::new();
                    let mut closed = false;
                    while let Some(c) = self.peek(0) {
                        self.i += 1;
                        if c == '\\' {
                            if let Some(n) = self.peek(0) {
                                inner.push(n);
                                self.i += 1;
                            }
                            continue;
                        }
                        if c == '`' {
                            closed = true;
                            break;
                        }
                        inner.push(c);
                    }
                    if !closed {
                        return Err(ScanError::UnterminatedBacktick {
                            offset: self.err_at(tick_offset),
                        });
                    }
                    if w.text.is_empty() {
                        substs_at_start += 1;
                    }
                    w.text.push_str("`...`");
                    w.substs.push(inner);
                }
                '$' => self.read_dollar(&mut w, &mut substs_at_start)?,
                _ => {
                    w.text.push(ch);
                    self.i += 1;
                }
            }
        }
        w.whole_subst = w.substs.len() == 1
            && substs_at_start == 1
            && (w.text == "$(...)" || w.text == "`...`");
        Ok(Some(w))
    }

    fn read_dollar(&mut self, w: &mut Word, substs_at_start: &mut usize) -> Result<(), ScanError> {
        if self.starts("$((") {
            self.i += 3;
            let inner = self.read_balanced_arith()?;
            w.text.push_str(&format!("$(({inner}))"));
        } else if self.starts("$(") {
            let offset = self.i;
            self.i += 2;
            let inner = self.read_balanced_parens_at(offset)?;
            if w.text.is_empty() {
                *substs_at_start += 1;
            }
            w.text.push_str("$(...)");
            w.substs.push(inner);
        } else if self.starts("${") {
            self.i += 2;
            let mut depth = 1;
            let mut inner = String::new();
            let mut closed = false;
            while let Some(c) = self.peek(0) {
                self.i += 1;
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            closed = true;
                            break;
                        }
                    }
                    _ => {}
                }
                inner.push(c);
            }
            if !closed {
                return Err(ScanError::UnterminatedSubstitution {
                    offset: self.err_at(self.i),
                });
            }
            w.text.push_str(&format!("${{{inner}}}"));
        } else if self.starts("$'") {
            w.quoted = true;
            self.i += 2;
            while let Some(c) = self.peek(0) {
                self.i += 1;
                if c == '\\' {
                    if let Some(n) = self.peek(0) {
                        w.text.push(n);
                        self.i += 1;
                    }
                    continue;
                }
                if c == '\'' {
                    break;
                }
                w.text.push(c);
            }
        } else {
            self.i += 1;
            w.text.push('$');
            let mut named = false;
            while let Some(c) = self.peek(0) {
                if c.is_ascii_alphanumeric()
                    || c == '_'
                    || (!named && matches!(c, '@' | '*' | '#' | '?' | '!' | '-'))
                {
                    w.text.push(c);
                    self.i += 1;
                    named = true;
                    if !(c.is_ascii_alphanumeric() || c == '_') {
                        break;
                    }
                } else {
                    break;
                }
            }
        }
        Ok(())
    }

    /// Inside double quotes: escapes, `$()`, backticks, `${}`; stops at the
    /// closing quote.
    fn read_double(&mut self, w: &mut Word, quote_offset: usize) -> Result<(), ScanError> {
        let mut dummy = 0usize;
        while let Some(c) = self.peek(0) {
            match c {
                '"' => {
                    self.i += 1;
                    return Ok(());
                }
                '\\' => {
                    if let Some(n) = self.peek(1) {
                        if !matches!(n, '"' | '\\' | '$' | '`' | '\n') {
                            w.text.push('\\');
                        }
                        if n != '\n' {
                            w.text.push(n);
                        }
                    }
                    self.i += 2;
                }
                '$' if matches!(self.peek(1), Some('(' | '{')) => {
                    self.read_dollar(w, &mut dummy)?;
                }
                '`' => {
                    let tick_offset = self.i;
                    self.i += 1;
                    let mut inner = String::new();
                    let mut closed = false;
                    while let Some(b) = self.peek(0) {
                        self.i += 1;
                        if b == '`' {
                            closed = true;
                            break;
                        }
                        inner.push(b);
                    }
                    if !closed {
                        return Err(ScanError::UnterminatedBacktick {
                            offset: self.err_at(tick_offset),
                        });
                    }
                    w.text.push_str("`...`");
                    w.substs.push(inner);
                }
                _ => {
                    w.text.push(c);
                    self.i += 1;
                }
            }
        }
        Err(ScanError::UnterminatedDoubleQuote {
            offset: self.err_at(quote_offset),
        })
    }

    fn read_balanced_parens(&mut self) -> Result<String, ScanError> {
        let offset = self.i;
        self.read_balanced_parens_at(offset)
    }

    fn read_balanced_parens_at(&mut self, offset: usize) -> Result<String, ScanError> {
        let start = self.i;
        let mut depth = 1usize;
        // `"$(cat <<'EOF' ... EOF\n)"`: a heredoc body inside a
        // substitution is literal text, so an apostrophe in it must not
        // open a quote (RESEARCH-CMD-bounded-tokenizer caveat).
        let mut heredocs: Vec<(String, bool)> = Vec::new();
        while let Some(c) = self.peek(0) {
            match c {
                '<' if self.peek(1) == Some('<') && self.peek(2) != Some('<') => {
                    self.i += 2;
                    let strip = self.peek(0) == Some('-');
                    if strip {
                        self.i += 1;
                    }
                    while matches!(self.peek(0), Some(' ' | '\t')) {
                        self.i += 1;
                    }
                    let delim = self.read_heredoc_delimiter();
                    if !delim.is_empty() {
                        heredocs.push((delim, strip));
                    }
                }
                '\n' if !heredocs.is_empty() => {
                    self.i += 1;
                    for (delim, strip) in std::mem::take(&mut heredocs) {
                        while self.i < self.c.len() {
                            let line_start = self.i;
                            while self.i < self.c.len() && self.c[self.i] != '\n' {
                                self.i += 1;
                            }
                            let line: String = self.c[line_start..self.i].iter().collect();
                            if self.i < self.c.len() {
                                self.i += 1;
                            }
                            let cmp = if strip {
                                line.trim_start_matches('\t')
                            } else {
                                line.as_str()
                            };
                            if cmp == delim {
                                break;
                            }
                        }
                    }
                }
                '\\' => self.i += 2,
                '\'' => {
                    self.i += 1;
                    while let Some(q) = self.peek(0) {
                        self.i += 1;
                        if q == '\'' {
                            break;
                        }
                    }
                }
                '"' => {
                    self.i += 1;
                    self.skip_double();
                }
                '`' => {
                    self.i += 1;
                    while let Some(q) = self.peek(0) {
                        self.i += 1;
                        if q == '`' {
                            break;
                        }
                    }
                }
                '(' => {
                    depth += 1;
                    self.i += 1;
                }
                ')' => {
                    depth -= 1;
                    self.i += 1;
                    if depth == 0 {
                        let end = self.i.saturating_sub(1).max(start);
                        return Ok(self.c[start..end.min(self.c.len())].iter().collect());
                    }
                }
                _ => self.i += 1,
            }
        }
        Err(ScanError::UnterminatedSubstitution {
            offset: self.err_at(offset),
        })
    }

    fn read_balanced_arith(&mut self) -> Result<String, ScanError> {
        let offset = self.i;
        let start = self.i;
        let mut depth = 2usize;
        while let Some(c) = self.peek(0) {
            match c {
                '(' => {
                    depth += 1;
                    self.i += 1;
                }
                ')' => {
                    depth -= 1;
                    self.i += 1;
                    if depth == 0 {
                        let end = self.i.saturating_sub(2).max(start);
                        return Ok(self.c[start..end.min(self.c.len())].iter().collect());
                    }
                }
                _ => self.i += 1,
            }
        }
        Err(ScanError::UnterminatedSubstitution {
            offset: self.err_at(offset),
        })
    }

    fn skip_double(&mut self) {
        while let Some(c) = self.peek(0) {
            match c {
                '\\' => self.i += 2,
                '"' => {
                    self.i += 1;
                    return;
                }
                '$' if self.peek(1) == Some('(') => {
                    self.i += 2;
                    let _ = self.read_balanced_parens();
                }
                _ => self.i += 1,
            }
        }
    }

    /// True if the unconsumed input at the current position begins a
    /// function definition: `function NAME [()] {` or `NAME() {`.
    /// Rollback-free: it only peeks.
    fn starts_function_def(&self) -> bool {
        self.peek_function_def().is_some()
    }

    /// Returns the byte length (in chars) consumed by a function-definition
    /// header (up to and including the opening `{`), if the input at the
    /// current position is one.
    fn peek_function_def(&self) -> Option<usize> {
        let mut j = self.i;
        let has_keyword = self.word_at(j) == Some("function".to_string());
        if has_keyword {
            j += "function".chars().count();
            j = skip_ws(&self.c, j);
        }
        let name = self.bareword_at(j)?;
        if name.is_empty() {
            return None;
        }
        j += name.chars().count();
        let mut saw_parens = false;
        let after_name_ws = skip_ws(&self.c, j);
        if self.c.get(after_name_ws) == Some(&'(') {
            let mut k = after_name_ws + 1;
            k = skip_ws(&self.c, k);
            if self.c.get(k) != Some(&')') {
                return None;
            }
            k += 1;
            j = k;
            saw_parens = true;
        }
        if !has_keyword && !saw_parens {
            return None;
        }
        j = skip_ws_and_newlines(&self.c, j);
        if self.c.get(j) == Some(&'{') {
            Some(j + 1 - self.i)
        } else {
            None
        }
    }

    fn word_at(&self, idx: usize) -> Option<String> {
        let bw = self.bareword_at(idx)?;
        let end = idx + bw.chars().count();
        let boundary = self
            .c
            .get(end)
            .is_none_or(|c| c.is_whitespace() || is_meta(*c));
        if boundary { Some(bw) } else { None }
    }

    fn bareword_at(&self, idx: usize) -> Option<String> {
        let mut end = idx;
        while let Some(c) = self.c.get(end) {
            if c.is_whitespace() || is_meta(*c) || matches!(c, '(' | ')' | '{' | '}') {
                break;
            }
            end += 1;
        }
        if end == idx {
            return None;
        }
        Some(self.c[idx..end].iter().collect())
    }

    fn read_function_def(&mut self) -> Result<String, ScanError> {
        let header_len = self
            .peek_function_def()
            .expect("caller checked starts_function_def");
        let header_start = self.i;
        self.i = header_start + header_len; // just past the opening `{`
        let brace_offset = self.err_at(self.i - 1);
        self.read_balanced_braces(brace_offset)
    }

    /// Reads to the `}` that closes the already-consumed opening `{`,
    /// respecting quotes, backticks, `$()`, and heredocs the same way
    /// [`Self::read_balanced_parens_at`] does for `()`.
    fn read_balanced_braces(&mut self, open_offset: usize) -> Result<String, ScanError> {
        let start = self.i;
        let mut depth = 1usize;
        let mut heredocs: Vec<(String, bool)> = Vec::new();
        while let Some(c) = self.peek(0) {
            match c {
                '<' if self.peek(1) == Some('<') && self.peek(2) != Some('<') => {
                    self.i += 2;
                    let strip = self.peek(0) == Some('-');
                    if strip {
                        self.i += 1;
                    }
                    while matches!(self.peek(0), Some(' ' | '\t')) {
                        self.i += 1;
                    }
                    let delim = self.read_heredoc_delimiter();
                    if !delim.is_empty() {
                        heredocs.push((delim, strip));
                    }
                }
                '\n' if !heredocs.is_empty() => {
                    self.i += 1;
                    for (delim, strip) in std::mem::take(&mut heredocs) {
                        while self.i < self.c.len() {
                            let line_start = self.i;
                            while self.i < self.c.len() && self.c[self.i] != '\n' {
                                self.i += 1;
                            }
                            let line: String = self.c[line_start..self.i].iter().collect();
                            if self.i < self.c.len() {
                                self.i += 1;
                            }
                            let cmp = if strip {
                                line.trim_start_matches('\t')
                            } else {
                                line.as_str()
                            };
                            if cmp == delim {
                                break;
                            }
                        }
                    }
                }
                '\\' => self.i += 2,
                '\'' => {
                    self.i += 1;
                    while let Some(q) = self.peek(0) {
                        self.i += 1;
                        if q == '\'' {
                            break;
                        }
                    }
                }
                '"' => {
                    self.i += 1;
                    self.skip_double();
                }
                '`' => {
                    self.i += 1;
                    while let Some(q) = self.peek(0) {
                        self.i += 1;
                        if q == '`' {
                            break;
                        }
                    }
                }
                '{' => {
                    depth += 1;
                    self.i += 1;
                }
                '}' => {
                    depth -= 1;
                    self.i += 1;
                    if depth == 0 {
                        let end = self.i.saturating_sub(1).max(start);
                        return Ok(self.c[start..end.min(self.c.len())].iter().collect());
                    }
                }
                _ => self.i += 1,
            }
        }
        Err(ScanError::UnterminatedFunctionBody {
            offset: open_offset,
        })
    }
}

fn skip_ws(c: &[char], mut idx: usize) -> usize {
    while matches!(c.get(idx), Some(' ' | '\t')) {
        idx += 1;
    }
    idx
}

fn skip_ws_and_newlines(c: &[char], mut idx: usize) -> usize {
    while matches!(c.get(idx), Some(' ' | '\t' | '\r' | '\n' | ';')) {
        idx += 1;
    }
    idx
}

fn scan_tokens(src: &str) -> Result<Vec<Tok>, ScanError> {
    Scanner::new(src).run()
}

// ---------------------------------------------------------------------
// Grouping tokens into simple commands, and resolving each one.
// ---------------------------------------------------------------------

#[derive(Default)]
struct Cmd {
    words: Vec<Word>,
    heredocs: Vec<String>,
    after_sep: bool,
}

fn group(toks: Vec<Tok>, out: &mut Vec<CmdOrDef>) {
    let mut cur = Cmd::default();
    let mut seen_sep = false;
    let mut redirect_pending = false;
    for t in toks {
        match t {
            Tok::Sep => {
                seen_sep = true;
                if !cur.words.is_empty() || !cur.heredocs.is_empty() {
                    out.push(CmdOrDef::Cmd(std::mem::take(&mut cur)));
                }
                cur.after_sep = true;
                redirect_pending = false;
            }
            Tok::Redirect => redirect_pending = true,
            Tok::Heredoc { body } => cur.heredocs.push(body),
            Tok::Word(w) => {
                if redirect_pending {
                    // Redirect targets are not command words.
                    redirect_pending = false;
                } else {
                    cur.after_sep = cur.after_sep || seen_sep;
                    cur.words.push(w);
                }
            }
            Tok::FunctionDef { body } => {
                if !cur.words.is_empty() || !cur.heredocs.is_empty() {
                    out.push(CmdOrDef::Cmd(std::mem::take(&mut cur)));
                }
                cur.after_sep = true;
                out.push(CmdOrDef::Def { body });
            }
        }
    }
    if !cur.words.is_empty() || !cur.heredocs.is_empty() {
        out.push(CmdOrDef::Cmd(cur));
    }
}

enum CmdOrDef {
    Cmd(Cmd),
    Def { body: String },
}

fn is_assignment(text: &str) -> bool {
    let Some(eq) = text.find('=') else {
        return false;
    };
    let name = text[..eq].trim_end_matches('+');
    let name = name.split('[').next().unwrap_or("");
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn basename(text: &str) -> &str {
    text.rsplit('/').next().unwrap_or(text)
}

const PREFIX_KEYWORDS: &[&str] = &[
    "if", "then", "else", "elif", "do", "while", "until", "!", "{", "coproc",
];
const END_KEYWORDS: &[&str] = &["fi", "done", "esac", "}"];
/// Words whose remaining siblings are data, never a command position.
const CLAUSE_KEYWORDS: &[&str] = &[
    "for", "case", "select", "in", "local", "declare", "export", "readonly", "typeset", "unset",
    "alias", "trap", "return", "exit", "break", "continue", "shift", "set", "read", "wait",
];

/// (wrapper name, options that take a separate value word, positional
/// arguments consumed before the wrapped command) -- FR-CMD-007's inline
/// wrapper table, hardened per RESEARCH-CMD-bounded-tokenizer's
/// `next_step.if_yes` with `watch`, `ssh`, `parallel`, and `setsid`.
/// `docker exec` is handled separately: it is a two-word wrapper.
const WRAPPERS: &[(&str, &[&str], usize)] = &[
    ("env", &["-u", "-C", "--unset", "--chdir"], 0),
    ("sudo", &["-u", "-g", "-p", "-C", "-D"], 0),
    ("timeout", &["-s", "-k", "--signal", "--kill-after"], 1),
    ("nice", &["-n", "--adjustment"], 0),
    ("nohup", &[], 0),
    ("stdbuf", &["-i", "-o", "-e"], 0),
    (
        "xargs",
        &[
            "-I",
            "-i",
            "-n",
            "-P",
            "-L",
            "-l",
            "-d",
            "-s",
            "-E",
            "-e",
            "-a",
            "--max-args",
            "--max-procs",
            "--delimiter",
            "--arg-file",
            "--replace",
        ],
        0,
    ),
    ("command", &[], 0),
    ("exec", &["-a"], 0),
    ("time", &["-f", "-o"], 0),
    ("watch", &["-n", "-d", "--interval"], 0),
    (
        "ssh",
        &[
            "-p", "-i", "-l", "-o", "-F", "-J", "-L", "-R", "-D", "-W", "-c", "-w", "-E", "-e",
            "-B",
        ],
        1,
    ),
    ("parallel", &[], 0),
    ("setsid", &[], 0),
];

/// Launchers this scanner recognizes as wrapper-shaped -- a bare command
/// followed by the command it runs -- but which FR-CMD-007's table does not
/// name. Kept small and explicit on purpose: only entries here become
/// [`Opaque::UnknownWrapper`]; every other unrecognized binary is an
/// ordinary [`Invocation`], not a wrapper at all.
const UNKNOWN_WRAPPERS: &[&str] = &["flock", "chpst", "unbuffer"];

const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "fish"];
const INTERPRETERS: &[&str] = &[
    "python",
    "python3",
    "node",
    "perl",
    "ruby",
    "bun",
    "deno",
    "php",
    "osascript",
    "lua",
];
const LOOKUPS: &[&str] = &["which", "type", "whereis", "man", "hash", "help", "tldr"];

fn walk(
    src: &str,
    depth: u8,
    inherited: Option<Position>,
    out: &mut Scan,
) -> Result<(), ScanError> {
    let toks = scan_tokens(src)?;
    let mut grouped = Vec::new();
    group(toks, &mut grouped);
    for item in grouped {
        match item {
            CmdOrDef::Cmd(cmd) => {
                let position = inherited.unwrap_or(if cmd.after_sep {
                    Position::AfterOperator
                } else {
                    Position::First
                });
                for w in &cmd.words {
                    for inner in &w.substs {
                        descend(
                            inner,
                            depth,
                            position.strongest(Position::Substitution),
                            out,
                        )?;
                    }
                }
                resolve(&cmd.words, &cmd.heredocs, position, depth, out)?;
            }
            CmdOrDef::Def { body } => {
                // A definition, not a call: the body is not executed here.
                // Its commands are still resolved -- a later call to the
                // function would run them -- so route sees them, marked as
                // reached through a function body.
                descend(&body, depth, Position::FunctionBody, out)?;
            }
        }
    }
    Ok(())
}

/// Recurse one level, or record the region as too deep to see.
fn descend(src: &str, depth: u8, position: Position, out: &mut Scan) -> Result<(), ScanError> {
    if depth + 1 > MAX_DEPTH {
        out.opaque.push(Opaque::TooDeep);
        Ok(())
    } else {
        walk(src, depth + 1, Some(position), out)
    }
}

fn record(out: &mut Scan, binary: &str, args: Vec<String>, position: Position, depth: u8) {
    out.invocations.push(Invocation {
        binary: binary.to_string(),
        args,
        position,
        depth,
    });
}

fn words_to_args(words: &[Word]) -> Vec<String> {
    words.iter().map(|w| w.text.clone()).collect()
}

/// Skips assignment prefixes (`FOO=1`) and prefix keywords (`if`, `while`,
/// `{`, ...) starting at `i`, folding each into the strongest [`Position`]
/// reached so far. Shared by [`resolve`] (the top of a simple command) and
/// every wrapper/find-exec/docker-exec dispatch that hands off to "the next
/// command in sequence" -- `time if grep -q foo; then ...; fi` needs the
/// same skip after `time` that the top level needs after `;`.
fn skip_prefix(words: &[Word], mut i: usize, mut position: Position) -> (usize, Position) {
    loop {
        let Some(w) = words.get(i) else {
            return (i, position);
        };
        let t = w.text.as_str();
        if is_assignment(t) && !w.quoted {
            position = position.strongest(Position::AfterAssignment);
            i += 1;
            continue;
        }
        if !w.quoted && PREFIX_KEYWORDS.contains(&t) {
            position = position.strongest(Position::AfterOperator);
            i += 1;
            continue;
        }
        break;
    }
    (i, position)
}

/// Resolves the command starting at `i`, after skipping any assignment or
/// keyword prefix. Returns without resolving anything if what follows the
/// prefix is a closing or clause keyword (`fi`, `for`, `in`, ...), since
/// its siblings are data, never a command position.
fn resolve_from(
    words: &[Word],
    heredocs: &[String],
    i: usize,
    position: Position,
    depth: u8,
    out: &mut Scan,
) -> Result<(), ScanError> {
    let (i, position) = skip_prefix(words, i, position);
    let Some(w) = words.get(i) else { return Ok(()) };
    if !w.quoted
        && (END_KEYWORDS.contains(&w.text.as_str()) || CLAUSE_KEYWORDS.contains(&w.text.as_str()))
    {
        return Ok(());
    }
    resolve_at(words, heredocs, i, position, depth, out)
}

fn resolve(
    words: &[Word],
    heredocs: &[String],
    position: Position,
    depth: u8,
    out: &mut Scan,
) -> Result<(), ScanError> {
    resolve_from(words, heredocs, 0, position, depth, out)
}

fn resolve_at(
    words: &[Word],
    heredocs: &[String],
    i: usize,
    position: Position,
    depth: u8,
    out: &mut Scan,
) -> Result<(), ScanError> {
    let Some(w) = words.get(i) else { return Ok(()) };
    if w.whole_subst {
        out.opaque.push(Opaque::DynamicCommand);
        return Ok(());
    }
    if w.text.starts_with('$') && w.text.len() > 1 {
        // A bare `$VAR`, `$@`, `$*`, or `${...}` as the command name: built
        // at runtime, not resolvable without executing it. This holds even
        // when the word was written `"$@"` (quoted for correct
        // word-splitting) -- quoting changes how the shell splits the
        // result, not whether the head is known ahead of execution.
        out.opaque.push(Opaque::DynamicCommand);
        return Ok(());
    }
    let raw = w.text.as_str();
    let bin = basename(raw);
    let rest = &words[i + 1..];
    if bin.is_empty() {
        return Ok(());
    }

    // A script-shaped extension marks a separate file, not resolvable
    // without reading it -- regardless of whether it is named by a path
    // (`./scripts/release.sh`) or a bare name. A path with no such
    // extension (`/usr/bin/grep`) is an ordinary executable: only the
    // extension implies "this is a script", not "this has a `/` in it".
    let is_script_path = [
        ".sh", ".py", ".js", ".mjs", ".ts", ".rb", ".pl", ".bash", ".zsh",
    ]
    .iter()
    .any(|e| bin.ends_with(e));
    if is_script_path {
        out.opaque.push(Opaque::ScriptFile);
        return Ok(());
    }

    if bin == "source" || bin == "." {
        record(out, bin, words_to_args(rest), position, depth);
        out.opaque.push(Opaque::Sourced);
        return Ok(());
    }
    if LOOKUPS.contains(&bin) {
        record(out, bin, words_to_args(rest), position, depth);
        return Ok(());
    }
    if bin == "eval" {
        record(out, bin, words_to_args(rest), position, depth);
        out.opaque.push(Opaque::Eval);
        return Ok(());
    }
    if bin == "git" {
        if let Some(alias_value) = git_c_alias_value(rest) {
            record(out, bin, words_to_args(rest), position, depth);
            if alias_value.trim_start().starts_with('!') {
                out.opaque.push(Opaque::Alias);
            }
            return Ok(());
        }
        record(out, bin, words_to_args(rest), position, depth);
        return Ok(());
    }
    if bin == "docker" && rest.first().map(|w| w.text.as_str()) == Some("exec") {
        record(out, bin, words_to_args(rest), position, depth);
        let mut j = i + 2;
        while let Some(word) = words.get(j) {
            let t = word.text.as_str();
            if t == "--" {
                j += 1;
                break;
            } else if t.starts_with('-') && t.len() > 1 {
                j += if matches!(t, "-u" | "-w" | "-e" | "--user" | "--workdir" | "--env") {
                    2
                } else {
                    1
                };
            } else {
                break;
            }
        }
        j += 1; // the container name
        if j < words.len() {
            return resolve_from(
                words,
                heredocs,
                j,
                position.strongest(Position::Wrapper),
                depth,
                out,
            );
        }
        return Ok(());
    }
    if UNKNOWN_WRAPPERS.contains(&bin) {
        record(out, bin, words_to_args(rest), position, depth);
        out.opaque.push(Opaque::UnknownWrapper);
        return Ok(());
    }
    if let Some((_, value_opts, positionals)) = WRAPPERS.iter().find(|(name, _, _)| *name == bin) {
        record(out, bin, words_to_args(rest), position, depth);
        if bin == "command"
            && rest
                .first()
                .is_some_and(|w| matches!(w.text.as_str(), "-v" | "-V"))
        {
            return Ok(()); // a lookup, not an invocation of the target
        }
        if bin == "env"
            && let Some(k) = rest.iter().position(|w| w.text == "-S")
            && let Some(payload) = rest.get(k + 1)
        {
            return resolve_env_dash_s(&payload.text, position, depth, out);
        }
        let mut j = i + 1;
        while let Some(word) = words.get(j) {
            let t = word.text.as_str();
            if bin == "env" && is_assignment(t) {
                j += 1;
            } else if t == "--" {
                j += 1;
                break;
            } else if t.starts_with('-') && t.len() > 1 {
                j += if value_opts.contains(&t) { 2 } else { 1 };
            } else {
                break;
            }
        }
        j += positionals;
        if j < words.len() {
            return resolve_from(
                words,
                heredocs,
                j,
                position.strongest(Position::Wrapper),
                depth,
                out,
            );
        }
        return Ok(());
    }
    if SHELLS.contains(&bin) {
        record(out, bin, words_to_args(rest), position, depth);
        return resolve_shell(words, heredocs, i, position, depth, out);
    }
    if bin == "awk" {
        record(out, bin, words_to_args(rest), position, depth);
        return resolve_awk(rest, out);
    }
    if INTERPRETERS.contains(&bin) || bin.starts_with("python3.") {
        record(out, bin, words_to_args(rest), position, depth);
        return resolve_interpreter(rest, heredocs, bin, out);
    }
    if bin == "find" {
        record(out, bin, words_to_args(rest), position, depth);
        let mut j = i + 1;
        while j < words.len() {
            let t = words[j].text.as_str();
            if !words[j].quoted && matches!(t, "-exec" | "-execdir" | "-ok" | "-okdir") {
                let start = j + 1;
                let end = (start..words.len())
                    .find(|&m| matches!(words[m].text.as_str(), ";" | "+"))
                    .unwrap_or(words.len());
                if start < end {
                    if depth + 1 > MAX_DEPTH {
                        out.opaque.push(Opaque::TooDeep);
                    } else {
                        resolve_from(
                            &words[..end],
                            &[],
                            start,
                            position.strongest(Position::FindExec),
                            depth + 1,
                            out,
                        )?;
                    }
                }
                j = end;
            }
            j += 1;
        }
        return Ok(());
    }
    record(out, bin, words_to_args(rest), position, depth);
    Ok(())
}

/// `git [-C dir] -c alias.<name>=<value> ...`: the value of the first `-c
/// alias.*` config override, if present.
fn git_c_alias_value(rest: &[Word]) -> Option<String> {
    let mut k = 0;
    while k < rest.len() {
        let t = rest[k].text.as_str();
        if t == "-c" {
            if let Some(v) = rest.get(k + 1)
                && let Some(rhs) = v
                    .text
                    .strip_prefix("alias.")
                    .and_then(|s| s.split_once('='))
            {
                return Some(rhs.1.to_string());
            }
            k += 2;
        } else if matches!(
            t,
            "-C" | "--git-dir" | "--work-tree" | "--namespace" | "--exec-path"
        ) {
            k += 2;
        } else if t.starts_with('-') {
            k += 1;
        } else {
            break;
        }
    }
    None
}

/// `env -S '<command line>' ...`: the payload is a single string built at
/// the shell layer and re-split by `env` at runtime. A bounded scanner
/// approximates that split with a plain whitespace split -- good enough to
/// find the head, not a claim of exact `env -S` quoting semantics.
fn resolve_env_dash_s(
    payload: &str,
    position: Position,
    depth: u8,
    out: &mut Scan,
) -> Result<(), ScanError> {
    let mut parts = payload.split_whitespace();
    let Some(head) = parts.next() else {
        return Ok(());
    };
    let args: Vec<String> = parts.map(String::from).collect();
    record(
        out,
        basename(head),
        args,
        position.strongest(Position::Wrapper),
        depth,
    );
    Ok(())
}

fn resolve_shell(
    words: &[Word],
    heredocs: &[String],
    i: usize,
    position: Position,
    depth: u8,
    out: &mut Scan,
) -> Result<(), ScanError> {
    let mut j = i + 1;
    let mut inline = None;
    while j < words.len() {
        let t = words[j].text.as_str();
        if matches!(t, "-o" | "+o" | "-O" | "+O") {
            j += 2;
        } else if t == "--" {
            j += 1;
            break;
        } else if t.starts_with("--") {
            j += 1;
        } else if (t.starts_with('-') || t.starts_with('+')) && t.len() > 1 {
            if t[1..].contains('c') {
                inline = Some(j + 1);
                break;
            }
            j += 1;
        } else {
            break;
        }
    }
    if let Some(k) = inline {
        if let Some(script) = words.get(k) {
            return descend(
                &script.text,
                depth,
                position.strongest(Position::InlineShell),
                out,
            );
        }
        return Ok(());
    }
    if words.get(j).is_some() {
        out.opaque.push(Opaque::ScriptFile);
    } else if let Some(body) = heredocs.first() {
        descend(body, depth, position.strongest(Position::HeredocShell), out)?;
    } else {
        out.opaque.push(Opaque::StdinScript);
    }
    Ok(())
}

fn resolve_interpreter(
    rest: &[Word],
    heredocs: &[String],
    bin: &str,
    out: &mut Scan,
) -> Result<(), ScanError> {
    let family = bin.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    let mut j = 0;
    while j < rest.len() {
        let t = rest[j].text.as_str();
        let code_flag = match family {
            "python" => t == "-c",
            "node" | "bun" => matches!(t, "-e" | "--eval" | "-p" | "--print"),
            "perl" | "ruby" => t.starts_with('-') && !t.starts_with("--") && t.contains('e'),
            "php" => t == "-r",
            "osascript" | "lua" => t == "-e",
            "deno" => t == "eval",
            _ => false,
        };
        if code_flag {
            let code = rest.get(j + 1).map(|w| w.text.clone()).unwrap_or_default();
            out.opaque.push(Opaque::Interpreter {
                interpreter: bin.to_string(),
                body: code,
            });
            return Ok(());
        }
        if t.starts_with('-') && t.len() > 1 {
            j += 1;
            continue;
        }
        if t == "-" {
            break;
        }
        out.opaque.push(Opaque::ScriptFile);
        return Ok(());
    }
    let body = heredocs.first().cloned().unwrap_or_default();
    out.opaque.push(Opaque::StdinScript);
    let _ = body; // kept opaque per FR-CMD-007: a stdin script carries no inline text to expose
    Ok(())
}

fn resolve_awk(rest: &[Word], out: &mut Scan) -> Result<(), ScanError> {
    let mut j = 0;
    while j < rest.len() {
        let t = rest[j].text.as_str();
        if t == "-f" {
            out.opaque.push(Opaque::ScriptFile);
            return Ok(());
        }
        if t.starts_with('-') && t.len() > 1 {
            j += 1;
            continue;
        }
        out.opaque.push(Opaque::Interpreter {
            interpreter: "awk".to_string(),
            body: t.to_string(),
        });
        return Ok(());
    }
    out.opaque.push(Opaque::StdinScript);
    Ok(())
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

impl Opaque {
    /// The kebab-case name used by the fixture battery to identify a
    /// variant, ignoring [`Opaque::Interpreter`]'s carried data.
    pub fn kind_name(&self) -> &'static str {
        match self {
            Opaque::Interpreter { .. } => "interpreter",
            Opaque::ScriptFile => "script-file",
            Opaque::Eval => "eval",
            Opaque::DynamicCommand => "dynamic-command",
            Opaque::StdinScript => "stdin-script",
            Opaque::Sourced => "sourced",
            Opaque::Alias => "alias",
            Opaque::UnknownWrapper => "unknown-wrapper",
            Opaque::TooDeep => "too-deep",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invocation<'a>(scan: &'a Scan, binary: &str) -> Option<&'a Invocation> {
        scan.invocations.iter().find(|inv| inv.binary == binary)
    }

    // -- Plain, first-position visibility --------------------------------

    #[test]
    fn plain_command_resolves_at_first_position() {
        let scan = scan("grep -rn foo src").expect("valid command");
        let inv = invocation(&scan, "grep").expect("grep resolved");
        assert_eq!(inv.position, Position::First);
        assert_eq!(inv.depth, 0);
        assert_eq!(inv.args, vec!["-rn", "foo", "src"]);
    }

    // -- NFR-CMD-002: each visibility path has a unit test ---------------

    #[test]
    fn managed_binary_after_pipe_is_routed_identically() {
        let first = scan("grep -rn foo src").expect("valid");
        let piped = scan("cat src/a.rs | grep -rn foo src").expect("valid");
        let a = invocation(&first, "grep").expect("first");
        let b = invocation(&piped, "grep").expect("after pipe");
        assert_eq!(a.binary, b.binary);
        assert_eq!(a.args, b.args);
        assert_eq!(a.position, Position::First);
        assert_eq!(b.position, Position::AfterOperator);
    }

    #[test]
    fn managed_binary_behind_environment_prefix_is_routed_identically() {
        let first = scan("grep -rn foo src").expect("valid");
        let prefixed = scan("FOO=1 grep -rn foo src").expect("valid");
        let a = invocation(&first, "grep").expect("first");
        let b = invocation(&prefixed, "grep").expect("behind assignment");
        assert_eq!(a.binary, b.binary);
        assert_eq!(a.args, b.args);
        assert_eq!(b.position, Position::AfterAssignment);
    }

    #[test]
    fn managed_binary_inside_inline_wrapper_is_routed_identically() {
        let first = scan("grep -rn foo src").expect("valid");
        let wrapped = scan("env grep -rn foo src").expect("valid");
        let a = invocation(&first, "grep").expect("first");
        let b = invocation(&wrapped, "grep").expect("inside wrapper");
        assert_eq!(a.binary, b.binary);
        assert_eq!(a.args, b.args);
        assert_eq!(b.position, Position::Wrapper);
    }

    // -- Wrapper table (FR-CMD-007) ---------------------------------------

    #[test]
    fn sudo_wrapper_reaches_the_wrapped_command() {
        let scan = scan("sudo -u root find / -name core").expect("valid");
        assert!(invocation(&scan, "sudo").is_some());
        let find = invocation(&scan, "find").expect("find resolved through sudo");
        assert_eq!(find.position, Position::Wrapper);
    }

    #[test]
    fn timeout_skips_its_duration_positional() {
        let scan = scan("timeout 30 gh run watch 123456").expect("valid");
        let gh = invocation(&scan, "gh").expect("gh resolved through timeout");
        assert_eq!(gh.position, Position::Wrapper);
        assert_eq!(gh.args, vec!["run", "watch", "123456"]);
    }

    #[test]
    fn command_v_is_a_lookup_not_an_invocation_of_its_target() {
        let scan = scan("command -v gh").expect("valid");
        assert!(invocation(&scan, "command").is_some());
        assert!(invocation(&scan, "gh").is_none());
    }

    #[test]
    fn docker_exec_reaches_the_wrapped_command() {
        let scan =
            scan("docker exec build-container grep -c ERROR /var/log/app.log").expect("valid");
        assert!(invocation(&scan, "docker").is_some());
        let grep = invocation(&scan, "grep").expect("grep resolved through docker exec");
        assert_eq!(grep.position, Position::Wrapper);
    }

    #[test]
    fn env_dash_s_splits_its_payload_and_resolves_the_head() {
        let scan = scan("env -S \"grep -rn foo\" src").expect("valid");
        let grep = invocation(&scan, "grep").expect("grep resolved through env -S");
        assert_eq!(grep.position, Position::Wrapper);
        assert_eq!(grep.args, vec!["-rn", "foo"]);
    }

    #[test]
    fn time_is_a_wrapper_not_a_skipped_keyword() {
        let scan = scan("time rg -n foo").expect("valid");
        assert!(invocation(&scan, "time").is_some());
        let rg = invocation(&scan, "rg").expect("rg resolved through time");
        assert_eq!(rg.position, Position::Wrapper);
    }

    #[test]
    fn time_before_a_keyword_still_reaches_the_wrapped_command() {
        let scan = scan("time if grep -q foo bar; then echo hit; fi").expect("valid");
        let grep = invocation(&scan, "grep").expect("grep resolved through time and if");
        assert_eq!(grep.position, Position::Wrapper);
    }

    // -- Shells: -c, heredoc, script file, stdin -------------------------

    #[test]
    fn sh_c_inline_script_resolves_at_depth_one() {
        let scan = scan("sh -c 'grep -rn foo src'").expect("valid");
        let grep = invocation(&scan, "grep").expect("grep resolved inside sh -c");
        assert_eq!(grep.position, Position::InlineShell);
        assert_eq!(grep.depth, 1);
    }

    #[test]
    fn bash_heredoc_resolves_the_body() {
        let scan = scan("bash <<'EOF'\ncd src\nrg -n 'fn main' .\nEOF").expect("valid");
        let rg = invocation(&scan, "rg").expect("rg resolved inside the heredoc body");
        assert_eq!(rg.position, Position::HeredocShell);
        assert_eq!(rg.depth, 1);
    }

    #[test]
    fn bash_script_file_is_opaque() {
        let scan = scan("bash run2.sh").expect("valid");
        assert!(scan.opaque.iter().any(|o| o.kind_name() == "script-file"));
    }

    #[test]
    fn dot_slash_script_is_opaque() {
        let scan = scan("./scripts/release.sh --dry-run").expect("valid");
        assert!(scan.opaque.iter().any(|o| o.kind_name() == "script-file"));
    }

    // -- Interpreters ------------------------------------------------------

    #[test]
    fn python_dash_c_body_is_opaque_but_carries_the_body_text() {
        let scan = scan("python3 -c \"import os; print(1)\"").expect("valid");
        let found = scan.opaque.iter().find_map(|o| match o {
            Opaque::Interpreter { interpreter, body } => Some((interpreter.clone(), body.clone())),
            _ => None,
        });
        let (interpreter, body) = found.expect("interpreter body recorded");
        assert_eq!(interpreter, "python3");
        assert!(body.contains("import os"));
    }

    #[test]
    fn awk_program_body_is_an_opaque_interpreter() {
        let scan = scan("awk 'BEGIN{system(\"grep -rn struct src\")}' access.log").expect("valid");
        let found = scan
            .opaque
            .iter()
            .any(|o| matches!(o, Opaque::Interpreter { interpreter, .. } if interpreter == "awk"));
        assert!(found);
    }

    // -- find -exec ----------------------------------------------------------

    #[test]
    fn find_exec_resolves_the_executed_command() {
        let scan = scan("find src -name '*.rs' -exec grep -l 'fn route' {} \\;").expect("valid");
        assert!(invocation(&scan, "find").is_some());
        let grep = invocation(&scan, "grep").expect("grep resolved through find -exec");
        assert_eq!(grep.position, Position::FindExec);
        assert_eq!(grep.depth, 1);
    }

    #[test]
    fn find_exec_sh_c_resolves_at_depth_two() {
        let scan =
            scan("find . -name '*.md' -exec sh -c 'grep -l TODO \"$1\"' _ {} \\;").expect("valid");
        let grep = invocation(&scan, "grep").expect("grep resolved through find -exec and sh -c");
        assert_eq!(grep.depth, 2);
    }

    // -- Substitutions -------------------------------------------------------

    #[test]
    fn command_substitution_resolves_at_depth_one() {
        let scan = scan("echo \"branch is $(git branch --show-current)\"").expect("valid");
        let git = invocation(&scan, "git").expect("git resolved inside the substitution");
        assert_eq!(git.position, Position::Substitution);
        assert_eq!(git.depth, 1);
    }

    #[test]
    fn heredoc_body_inside_command_substitution_is_literal_text() {
        // The commit-message idiom (RESEARCH-CMD-bounded-tokenizer caveat):
        // an apostrophe inside a heredoc body nested in `$()` must not be
        // read as opening a quote.
        let cmd = "git add src/a.rs && git commit -m \"$(cat <<'EOF'\nfix: don't trust the agent's \"quotes\" (it's a body)\nEOF\n)\" && git push origin HEAD";
        let scan = scan(cmd).expect("the heredoc-in-substitution idiom parses");
        let gits: Vec<&Invocation> = scan
            .invocations
            .iter()
            .filter(|i| i.binary == "git")
            .collect();
        assert!(
            gits.iter()
                .any(|g| g.args.first().map(String::as_str) == Some("commit"))
        );
        assert!(
            gits.iter()
                .any(|g| g.args.first().map(String::as_str) == Some("push"))
        );
    }

    #[test]
    fn too_deep_beyond_max_depth_is_recorded_opaque() {
        let scan = scan("echo $(echo $(echo $(grep -c x f)))").expect("valid");
        assert!(scan.opaque.iter().any(|o| o.kind_name() == "too-deep"));
        assert!(invocation(&scan, "grep").is_none());
    }

    // -- Function bodies -----------------------------------------------------

    #[test]
    fn function_keyword_body_is_resolved_as_function_body() {
        let scan = scan("function g { grep -rn \"$@\" src; }; g foo src").expect("valid");
        let grep = invocation(&scan, "grep").expect("grep resolved inside the function body");
        assert_eq!(grep.position, Position::FunctionBody);
    }

    #[test]
    fn name_parens_form_is_a_definition_not_a_call() {
        let scan = scan("search() { rg -n \"$@\" src; }").expect("valid");
        // The definition itself is not a call to a binary named "search".
        assert!(invocation(&scan, "search").is_none());
        let rg = invocation(&scan, "rg").expect("rg resolved inside the function body");
        assert_eq!(rg.position, Position::FunctionBody);
    }

    // -- Opaque classes named in FR-CMD-007 -----------------------------------

    #[test]
    fn eval_is_opaque() {
        let scan = scan("eval \"$(ssh-agent -s)\"").expect("valid");
        assert!(invocation(&scan, "eval").is_some());
        assert!(scan.opaque.iter().any(|o| o.kind_name() == "eval"));
    }

    #[test]
    fn dynamic_command_from_a_bare_variable_is_opaque() {
        let scan = scan("$SEARCH -rn foo src").expect("valid");
        assert!(
            scan.opaque
                .iter()
                .any(|o| o.kind_name() == "dynamic-command")
        );
    }

    #[test]
    fn dynamic_command_from_a_whole_word_substitution_is_opaque() {
        let scan = scan("$(which rg) -n foo").expect("valid");
        assert!(
            scan.opaque
                .iter()
                .any(|o| o.kind_name() == "dynamic-command")
        );
    }

    #[test]
    fn dynamic_command_from_positional_args_array_is_opaque() {
        let scan = scan("set -- grep -rn foo src; \"$@\"").expect("valid");
        assert!(
            scan.opaque
                .iter()
                .any(|o| o.kind_name() == "dynamic-command")
        );
    }

    #[test]
    fn sourced_substitution_is_opaque() {
        let scan = scan("source <(echo \"grep -rn foo src\")").expect("valid");
        assert!(scan.opaque.iter().any(|o| o.kind_name() == "sourced"));
    }

    #[test]
    fn git_shell_alias_is_opaque() {
        let scan = scan("git -c alias.s='!grep -rn foo src' s").expect("valid");
        assert!(scan.opaque.iter().any(|o| o.kind_name() == "alias"));
    }

    #[test]
    fn unknown_wrapper_is_recorded_opaque_not_silently_resolved() {
        let scan = scan("flock /tmp/build.lock grep -rn foo src").expect("valid");
        assert!(invocation(&scan, "flock").is_some());
        assert!(
            scan.opaque
                .iter()
                .any(|o| o.kind_name() == "unknown-wrapper")
        );
        assert!(invocation(&scan, "grep").is_none());
    }

    // -- Benign: things that look like a match but are not -------------------

    #[test]
    fn git_log_grep_reflog_flag_is_an_ordinary_argument() {
        let scan = scan("git log --grep-reflog=x").expect("valid");
        let git = invocation(&scan, "git").expect("git resolved");
        assert_eq!(git.args, vec!["log", "--grep-reflog=x"]);
    }

    #[test]
    fn dash_dash_marks_a_literal_pathspec_not_a_flag() {
        let scan = scan("git log -- -Sfoo").expect("valid");
        let git = invocation(&scan, "git").expect("git resolved");
        assert_eq!(git.args, vec!["log", "--", "-Sfoo"]);
    }

    #[test]
    fn pnpm_grep_is_an_ordinary_invocation_of_pnpm() {
        let scan = scan("pnpm grep").expect("valid");
        let pnpm = invocation(&scan, "pnpm").expect("pnpm resolved");
        assert_eq!(pnpm.args, vec!["grep"]);
    }

    #[test]
    fn gh_as_a_for_loop_word_is_never_an_invocation() {
        let scan = scan("for src in legion gh curl; do echo \"$src\"; done").expect("valid");
        assert!(invocation(&scan, "gh").is_none());
    }

    #[test]
    fn gh_inside_a_quoted_argument_is_never_an_invocation() {
        let scan =
            scan("legion reflect --repo legion --text \"use gh via legion issue\"").expect("valid");
        assert!(invocation(&scan, "gh").is_none());
    }

    // -- Errors never produce a partial Scan ----------------------------------

    #[test]
    fn unterminated_single_quote_is_a_scan_error() {
        let err = scan("grep -n 'oops src").expect_err("unterminated quote");
        assert!(matches!(err, ScanError::UnterminatedSingleQuote { .. }));
    }

    #[test]
    fn unterminated_heredoc_is_a_scan_error() {
        let err = scan("bash <<'EOF'\nrg foo\n").expect_err("unterminated heredoc");
        assert!(matches!(err, ScanError::UnterminatedHeredoc { .. }));
    }

    #[test]
    fn unterminated_substitution_is_a_scan_error() {
        let err = scan("echo $(grep foo").expect_err("unterminated substitution");
        assert!(matches!(err, ScanError::UnterminatedSubstitution { .. }));
    }

    #[test]
    fn scan_never_panics_on_a_deterministic_fuzz_corpus() {
        // No proptest/quickcheck dependency: a small deterministic
        // generator over an alphabet that includes every construct this
        // scanner special-cases (quotes, backticks, `$(`, `<<`, braces,
        // unbalanced parens, multibyte characters) stands in for a
        // property test (NFR requires "no panic on any input, including
        // invalid UTF-8 boundaries in slices").
        let alphabet: &[char] = &[
            'a',
            ' ',
            '\'',
            '"',
            '`',
            '\\',
            '$',
            '(',
            ')',
            '{',
            '}',
            '<',
            '>',
            '|',
            '&',
            ';',
            '\n',
            '\t',
            '-',
            '=',
            '.',
            '/',
            '*',
            '@',
            '#',
            '!',
            '~',
            'あ',
            '\u{1F600}',
        ];
        let mut state: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = || {
            // xorshift64*: deterministic, dependency-free.
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..2000 {
            let len = (next() % 40) as usize;
            let s: String = (0..len)
                .map(|_| alphabet[(next() as usize) % alphabet.len()])
                .collect();
            // The only contract under test is "never panics"; either
            // outcome of the Result is acceptable.
            let _ = scan(&s);
        }
    }

    #[test]
    fn scan_error_display_names_the_construct_and_offset() {
        let err = scan("grep -n 'oops src").expect_err("unterminated quote");
        assert!(err.to_string().contains("unterminated single quote"));
        assert!(err.to_string().contains("byte"));
    }
}
