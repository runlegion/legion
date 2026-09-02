//! The Bash tokenizer and the opaque-interpreter route (FR-CMD-003, FR-CMD-004).
//!
//! A hand-written, byte-span-and-quoting-kind scanner. No `shell-words`
//! dependency: NFR-CMD-002 requires unparseable input to never silently
//! `Allow`, and a general-purpose shell-words crate drops quote kind, has no
//! operators, splits `$(...)` mid-token, and tokenizes heredoc bodies as
//! words -- none of which this crate can afford to inherit.
//!
//! # What this module runs on
//!
//! [`analyze`] returns `None` for every [`ToolCall`] variant except `Bash`.
//! An `Edit`/`Write`/`Grep`/... call with shell metacharacters in its content
//! is routed on its path and content by the caller, never handed to this
//! scanner -- there is no code path from a non-Bash call into [`scan`].
//!
//! # The covered see-through set
//!
//! - Pipes (`|`), sequencing (`&&`, `||`, `;`, newline) -- stage boundaries.
//! - Env prefixes: `env X=1 cmd` and bare `VAR=v cmd`, chained.
//! - Wrappers whose `options_before_command` is set in the route table
//!   (`timeout 5`, `nice -n 5`, `sudo -E`, `xargs -0`, `command`, `exec`,
//!   `time`, `env -S`), chained with each other and with env prefixes.
//! - Absolute-path-to-basename normalization on the resolved command word.
//! - Quoting sufficient to tokenize with quote kind kept (`Quoting`).
//! - Heredocs, attached to their stage as an opaque payload.
//! - `$(...)` and backticks to exactly one level, with a single-quoted `$(`
//!   staying inert text.
//!
//! # Known gaps (documented, not silent)
//!
//! - `Operator::Amp` (a bare `&`, backgrounding) is NOT a stage boundary --
//!   the issue's named boundary set is `Pipe`/`And`/`Or`/`Semi`/`Newline`
//!   only, so `gh pr list & rm -rf /` is one stage and `gh` sits in argument
//!   position rather than command position of its own stage. A future slice
//!   should decide whether `&` needs its own boundary.
//! - The wrapper skip is a heuristic, not a grammar: a wrapper's own leading
//!   flags (`-*`) and any all-digit positional (`timeout 5`, `nice -n 5`) are
//!   skipped, because `Wrapper` in the route table carries only
//!   `options_before_command: bool` and not a flag-arity table. A wrapper
//!   whose own option takes a non-numeric value ahead of the real command
//!   (uncommon) is not covered.
//! - The interpreter `-c` flag is recognized by name; `-e` (node) is not,
//!   to stay literal to FR-CMD-004's named trigger. A bare inline-script
//!   argument with no flag at all (`awk 'program' file`) is not recognized
//!   as opaque -- it is a documented gap, not a silent one.
//! - Redirects are assumed to trail the command's words within a stage for
//!   splice purposes (`gh pr view 5 2>/dev/null`). A redirect written before
//!   or between argument words is tokenized correctly but is not carried
//!   separately from the words that follow it.
//! - Multiple heredocs on one command line are consumed in the order their
//!   `<<` markers appear, each up to its own terminator line, which matches
//!   real shell behavior for the common case but has not been exercised
//!   against every POSIX edge case.

use std::ops::Range;

use thiserror::Error;

use crate::call::ToolCall;
use crate::decision::{Decision, Matched};
use crate::table::Wrapper;

/// One lexical unit of a Bash command line (FR-CMD-003).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub span: Range<usize>,
    pub kind: TokenKind,
}

/// What a [`Token`] is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    /// A word: a command name, a flag, an argument, or literal text.
    ///
    /// `live` is `true` when the word carries an unquoted or double-quoted
    /// `$(...)`/backtick substitution somewhere in its bytes -- the word is
    /// then ineligible for a lossless byte-for-byte rewrite, because part of
    /// it resolves at run time.
    Word {
        quoting: Quoting,
        live: bool,
    },
    Op(Operator),
    /// A redirect operator plus its target, kept as one opaque token so the
    /// target is never mistaken for a positional argument.
    Redirect,
    /// A bare `$(...)`/backtick construct that IS the whole word, with no
    /// surrounding quoting and nothing concatenated before or after it.
    ///
    /// `depth` is always `1` for a token that exists at all: anything
    /// nesting a second substitution inside this one is a scan error
    /// ([`ScanError::NestingExceeded`]) and never reaches token creation.
    /// The field is here for the shape's own future use, not because this
    /// slice ever produces another value.
    Subst {
        depth: u8,
    },
    /// A heredoc introducer (`<<DELIM`/`<<-DELIM`). `body_span` is the body
    /// text located later in the source, attached here rather than
    /// tokenized as words -- a heredoc body is never shell syntax.
    Heredoc {
        delim: String,
        quoted: bool,
        body_span: Range<usize>,
    },
    /// A `#`-to-end-of-line comment, recognized only at a word boundary.
    Comment,
}

/// Which kind of quoting a [`TokenKind::Word`] used, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quoting {
    None,
    Single,
    Double,
    Mixed,
}

/// A shell operator token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    Pipe,
    And,
    Or,
    Semi,
    Amp,
    Newline,
}

/// A construct the scanner could not tokenize.
///
/// Every variant maps to `Deny` or `Ask`, never `Allow` (NFR-CMD-002).
/// `NestingExceeded` is a construct the scanner FULLY resolved and refused;
/// the other three are constructs the scanner could not resolve the
/// boundary of at all.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ScanError {
    #[error("unbalanced quote starting at byte {at}")]
    UnbalancedQuote { at: usize },
    #[error("unterminated command substitution starting at byte {at}")]
    UnterminatedSubstitution { at: usize },
    #[error("nesting beyond one level of command substitution at byte {at}")]
    NestingExceeded { at: usize },
    #[error("unterminated heredoc, delimiter `{delim}` never found")]
    UnterminatedHeredoc { delim: String },
}

impl ScanError {
    /// The decision an unparseable command resolves to (NFR-CMD-002).
    ///
    /// `NestingExceeded` is a fully-resolved refusal, so it names the
    /// nesting and denies. The other three are boundaries the scanner
    /// could not locate at all, so they escalate rather than guess.
    fn into_decision(self) -> Decision {
        match &self {
            ScanError::NestingExceeded { .. } => Decision::Deny(self.to_string()),
            ScanError::UnbalancedQuote { .. }
            | ScanError::UnterminatedSubstitution { .. }
            | ScanError::UnterminatedHeredoc { .. } => Decision::Ask(self.to_string()),
        }
    }
}

fn quoting_from(saw_single: bool, saw_double: bool) -> Quoting {
    match (saw_single, saw_double) {
        (false, false) => Quoting::None,
        (true, false) => Quoting::Single,
        (false, true) => Quoting::Double,
        (true, true) => Quoting::Mixed,
    }
}

fn char_len(s: &str, i: usize) -> usize {
    s[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1)
}

/// The result of scanning one word's bytes, before classification.
struct WordScan {
    end: usize,
    saw_single: bool,
    saw_double: bool,
    live: bool,
    subst_range: Option<Range<usize>>,
}

fn classify_word(scan: &WordScan, start: usize) -> TokenKind {
    if !scan.saw_single
        && !scan.saw_double
        && let Some(r) = &scan.subst_range
        && r.start == start
        && r.end == scan.end
    {
        return TokenKind::Subst { depth: 1 };
    }
    TokenKind::Word {
        quoting: quoting_from(scan.saw_single, scan.saw_double),
        live: scan.live,
    }
}

/// Scan one word starting at `start`, honoring quoting and one level of
/// command substitution.
///
/// Substitution content gets its OWN local quote tracking (`inner_quote`),
/// independent of the outer word's quote state -- a quote inside `$(...)`
/// starts and ends its own local run, the way it does inside real Bash.
fn scan_word(src: &str, start: usize) -> Result<WordScan, ScanError> {
    let bytes = src.as_bytes();
    let mut i = start;
    let mut saw_single = false;
    let mut saw_double = false;
    let mut live = false;
    let mut subst_stack: Vec<u8> = Vec::new();
    let mut inner_quote: Option<u8> = None;
    let mut subst_range: Option<Range<usize>> = None;
    let mut cur_subst_start: Option<usize> = None;
    let mut quote: Option<u8> = None;

    loop {
        if i >= bytes.len() {
            if quote.is_some() {
                return Err(ScanError::UnbalancedQuote { at: start });
            }
            if !subst_stack.is_empty() {
                return Err(ScanError::UnterminatedSubstitution { at: start });
            }
            break;
        }
        let b = bytes[i];

        if !subst_stack.is_empty() {
            if inner_quote == Some(b'\'') {
                if b == b'\'' {
                    inner_quote = None;
                }
                i += char_len(src, i);
                continue;
            }
            if b == b'\\' && inner_quote != Some(b'\'') {
                i += 1;
                if i < bytes.len() {
                    i += char_len(src, i);
                }
                continue;
            }
            if inner_quote == Some(b'"') {
                if b == b'"' {
                    inner_quote = None;
                    i += char_len(src, i);
                    continue;
                }
                // Double quotes do not suppress command substitution, even
                // nested one level inside a substitution's own content.
                if b == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'(' {
                    subst_stack.push(b'(');
                    if subst_stack.len() > 1 {
                        return Err(ScanError::NestingExceeded { at: i });
                    }
                    i += 2;
                    continue;
                }
                if b == b'`' {
                    subst_stack.push(b'`');
                    if subst_stack.len() > 1 {
                        return Err(ScanError::NestingExceeded { at: i });
                    }
                    i += 1;
                    continue;
                }
                i += char_len(src, i);
                continue;
            }
            if b == b'\'' {
                inner_quote = Some(b'\'');
                i += 1;
                continue;
            }
            if b == b'"' {
                inner_quote = Some(b'"');
                i += 1;
                continue;
            }
            let top = *subst_stack.last().expect("non-empty");
            if (top == b'(' && b == b')') || (top == b'`' && b == b'`') {
                subst_stack.pop();
                i += 1;
                if subst_stack.is_empty()
                    && let Some(s0) = cur_subst_start
                    && subst_range.is_none()
                {
                    subst_range = Some(s0..i);
                }
                continue;
            }
            if b == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'(' {
                subst_stack.push(b'(');
                if subst_stack.len() > 1 {
                    return Err(ScanError::NestingExceeded { at: i });
                }
                i += 2;
                continue;
            }
            if b == b'`' {
                subst_stack.push(b'`');
                if subst_stack.len() > 1 {
                    return Err(ScanError::NestingExceeded { at: i });
                }
                i += 1;
                continue;
            }
            i += char_len(src, i);
            continue;
        }

        if quote == Some(b'\'') {
            if b == b'\'' {
                quote = None;
            }
            i += char_len(src, i);
            continue;
        }
        if b == b'\\' && quote != Some(b'\'') {
            i += 1;
            if i < bytes.len() {
                i += char_len(src, i);
            }
            continue;
        }
        if quote == Some(b'"') {
            if b == b'"' {
                quote = None;
                i += 1;
                continue;
            }
            if b == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'(' {
                cur_subst_start = Some(i);
                subst_stack.push(b'(');
                live = true;
                i += 2;
                continue;
            }
            if b == b'`' {
                cur_subst_start = Some(i);
                subst_stack.push(b'`');
                live = true;
                i += 1;
                continue;
            }
            i += char_len(src, i);
            continue;
        }

        // Bare, unquoted position: this is where a word can end.
        if b == b' ' || b == b'\t' || b == b'\n' {
            break;
        }
        if matches!(b, b'|' | b'&' | b';' | b'<' | b'>') {
            break;
        }
        if b == b'\'' {
            quote = Some(b'\'');
            saw_single = true;
            i += 1;
            continue;
        }
        if b == b'"' {
            quote = Some(b'"');
            saw_double = true;
            i += 1;
            continue;
        }
        if b == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'(' {
            cur_subst_start = Some(i);
            subst_stack.push(b'(');
            live = true;
            i += 2;
            continue;
        }
        if b == b'`' {
            cur_subst_start = Some(i);
            subst_stack.push(b'`');
            live = true;
            i += 1;
            continue;
        }
        i += char_len(src, i);
    }

    Ok(WordScan {
        end: i,
        saw_single,
        saw_double,
        live,
        subst_range,
    })
}

enum RedirectScan {
    Plain,
    Heredoc {
        dash: bool,
        delim: String,
        quoted: bool,
    },
}

fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    i
}

/// Scan one redirect operator (with any leading fd digits) plus its target.
fn scan_redirect(src: &str, start: usize) -> Result<(RedirectScan, usize), ScanError> {
    let bytes = src.as_bytes();
    let mut i = start;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b'&' && i + 1 < bytes.len() && bytes[i + 1] == b'>' {
        i += 2;
        i = skip_ws(bytes, i);
        let w = scan_word(src, i)?;
        return Ok((RedirectScan::Plain, w.end));
    }
    if i < bytes.len() && bytes[i] == b'<' {
        i += 1;
        if i < bytes.len() && bytes[i] == b'<' {
            i += 1;
            if i < bytes.len() && bytes[i] == b'<' {
                // Here-string `<<<target` -- treat as a plain redirect.
                i += 1;
                i = skip_ws(bytes, i);
                let w = scan_word(src, i)?;
                return Ok((RedirectScan::Plain, w.end));
            }
            let dash = i < bytes.len() && bytes[i] == b'-';
            if dash {
                i += 1;
            }
            i = skip_ws(bytes, i);
            let (delim, quoted, end) = scan_heredoc_delim(src, i)?;
            return Ok((
                RedirectScan::Heredoc {
                    dash,
                    delim,
                    quoted,
                },
                end,
            ));
        }
        i = skip_ws(bytes, i);
        let w = scan_word(src, i)?;
        return Ok((RedirectScan::Plain, w.end));
    }
    if i < bytes.len() && bytes[i] == b'>' {
        i += 1;
        if i < bytes.len() && bytes[i] == b'>' {
            i += 1;
        } else if i < bytes.len() && bytes[i] == b'&' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            return Ok((RedirectScan::Plain, i));
        }
        i = skip_ws(bytes, i);
        let w = scan_word(src, i)?;
        return Ok((RedirectScan::Plain, w.end));
    }
    // The caller only reaches here once it has confirmed a redirect starter.
    Err(ScanError::UnbalancedQuote { at: start })
}

fn scan_heredoc_delim(src: &str, start: usize) -> Result<(String, bool, usize), ScanError> {
    let bytes = src.as_bytes();
    if start >= bytes.len() {
        return Err(ScanError::UnterminatedHeredoc {
            delim: String::new(),
        });
    }
    let b = bytes[start];
    if b == b'\'' || b == b'"' {
        let mut i = start + 1;
        while i < bytes.len() && bytes[i] != b {
            i += char_len(src, i);
        }
        if i >= bytes.len() {
            return Err(ScanError::UnbalancedQuote { at: start });
        }
        let delim = src[start + 1..i].to_owned();
        return Ok((delim, true, i + 1));
    }
    let mut i = start;
    while i < bytes.len() && !matches!(bytes[i], b' ' | b'\t' | b'\n' | b';' | b'|' | b'&') {
        i += char_len(src, i);
    }
    if i == start {
        return Err(ScanError::UnterminatedHeredoc {
            delim: String::new(),
        });
    }
    Ok((src[start..i].to_owned(), false, i))
}

/// Locate a heredoc's body: from `start` (right after the introducer
/// line's newline) to the first line that, after stripping leading tabs
/// when `dash` is set, equals `delim` exactly.
fn consume_heredoc_body(
    src: &str,
    start: usize,
    delim: &str,
    dash: bool,
) -> Result<(Range<usize>, usize), ScanError> {
    let mut pos = start;
    loop {
        let line_start = pos;
        let nl = src[pos..].find('\n').map(|o| pos + o);
        let line_end = nl.unwrap_or(src.len());
        let line = &src[line_start..line_end];
        let candidate = if dash {
            line.trim_start_matches('\t')
        } else {
            line
        };
        if candidate == delim {
            let body_span = start..line_start;
            let after = match nl {
                Some(n) => n + 1,
                None => line_end,
            };
            return Ok((body_span, after));
        }
        match nl {
            Some(n) => pos = n + 1,
            None => {
                return Err(ScanError::UnterminatedHeredoc {
                    delim: delim.to_owned(),
                });
            }
        }
    }
}

fn push_redirect(
    tokens: &mut Vec<Token>,
    pending: &mut Vec<(usize, String, bool)>,
    kind: RedirectScan,
    span: Range<usize>,
) {
    match kind {
        RedirectScan::Plain => tokens.push(Token {
            span,
            kind: TokenKind::Redirect,
        }),
        RedirectScan::Heredoc {
            dash,
            delim,
            quoted,
        } => {
            let idx = tokens.len();
            tokens.push(Token {
                span,
                kind: TokenKind::Heredoc {
                    delim: delim.clone(),
                    quoted,
                    body_span: 0..0,
                },
            });
            pending.push((idx, delim, dash));
        }
    }
}

/// Tokenize a Bash command line.
///
/// No per-call pattern compilation: this is a hand-written byte scan, and
/// nothing in the crate compiles a regex (NFR-CMD-001).
pub fn scan(command: &str) -> Result<Vec<Token>, ScanError> {
    let bytes = command.as_bytes();
    let mut i = 0usize;
    let mut tokens: Vec<Token> = Vec::new();
    let mut pending_heredocs: Vec<(usize, String, bool)> = Vec::new();

    while i < bytes.len() {
        let b = bytes[i];
        if b == b' ' || b == b'\t' {
            i += 1;
            continue;
        }
        if b == b'\n' {
            tokens.push(Token {
                span: i..i + 1,
                kind: TokenKind::Op(Operator::Newline),
            });
            i += 1;
            for (tok_idx, delim, dash) in pending_heredocs.drain(..) {
                let (body_span, after) = consume_heredoc_body(command, i, &delim, dash)?;
                if let TokenKind::Heredoc { body_span: bs, .. } = &mut tokens[tok_idx].kind {
                    *bs = body_span;
                }
                i = after;
            }
            continue;
        }
        if b == b'#' {
            let start = i;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            tokens.push(Token {
                span: start..i,
                kind: TokenKind::Comment,
            });
            continue;
        }
        if b == b'|' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'|' {
                tokens.push(Token {
                    span: i..i + 2,
                    kind: TokenKind::Op(Operator::Or),
                });
                i += 2;
            } else {
                tokens.push(Token {
                    span: i..i + 1,
                    kind: TokenKind::Op(Operator::Pipe),
                });
                i += 1;
            }
            continue;
        }
        if b == b'&' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'&' {
                tokens.push(Token {
                    span: i..i + 2,
                    kind: TokenKind::Op(Operator::And),
                });
                i += 2;
                continue;
            }
            if i + 1 < bytes.len() && bytes[i + 1] == b'>' {
                let (kind, end) = scan_redirect(command, i)?;
                push_redirect(&mut tokens, &mut pending_heredocs, kind, i..end);
                i = end;
                continue;
            }
            tokens.push(Token {
                span: i..i + 1,
                kind: TokenKind::Op(Operator::Amp),
            });
            i += 1;
            continue;
        }
        if b == b';' {
            tokens.push(Token {
                span: i..i + 1,
                kind: TokenKind::Op(Operator::Semi),
            });
            i += 1;
            continue;
        }
        if b == b'<' || b == b'>' {
            let (kind, end) = scan_redirect(command, i)?;
            push_redirect(&mut tokens, &mut pending_heredocs, kind, i..end);
            i = end;
            continue;
        }
        if b.is_ascii_digit() {
            let mut j = i;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j < bytes.len() && (bytes[j] == b'<' || bytes[j] == b'>') {
                let (kind, end) = scan_redirect(command, i)?;
                push_redirect(&mut tokens, &mut pending_heredocs, kind, i..end);
                i = end;
                continue;
            }
        }
        let scanned = scan_word(command, i)?;
        let kind = classify_word(&scanned, i);
        tokens.push(Token {
            span: i..scanned.end,
            kind,
        });
        i = scanned.end;
    }

    if let Some((_, delim, _)) = pending_heredocs.into_iter().next() {
        return Err(ScanError::UnterminatedHeredoc { delim });
    }

    Ok(tokens)
}

/// One pipeline stage: the words between `Op` boundaries, plus the
/// redirects and heredocs attached to it.
///
/// `command_span` covers only the `Word`/`Subst` tokens (command + args);
/// `span` covers the whole stage including its redirects, so a splice can
/// replace `command_span` while carrying whatever `span` holds outside it
/// (redirects) forward untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stage {
    pub span: Range<usize>,
    pub command_span: Range<usize>,
    pub tokens: Vec<Token>,
    pub redirects: Vec<Token>,
    pub heredocs: Vec<Token>,
}

/// Group a token stream into pipeline stages.
///
/// Boundaries are exactly `Pipe`/`And`/`Or`/`Semi`/`Newline` -- `Amp` is
/// deliberately NOT a boundary (see the module doc's "known gaps").
pub fn build_stages(tokens: &[Token]) -> Vec<Stage> {
    let mut stages = Vec::new();
    let mut cur_tokens: Vec<Token> = Vec::new();
    let mut cur_redirects: Vec<Token> = Vec::new();
    let mut cur_heredocs: Vec<Token> = Vec::new();
    let mut stage_start: Option<usize> = None;
    let mut command_end: usize = 0;
    let mut last_end: usize = 0;

    let flush = |stages: &mut Vec<Stage>,
                 stage_start: &mut Option<usize>,
                 command_end: &mut usize,
                 last_end: &mut usize,
                 cur_tokens: &mut Vec<Token>,
                 cur_redirects: &mut Vec<Token>,
                 cur_heredocs: &mut Vec<Token>| {
        if let Some(start) = stage_start.take()
            && !(cur_tokens.is_empty() && cur_redirects.is_empty() && cur_heredocs.is_empty())
        {
            stages.push(Stage {
                span: start..*last_end,
                command_span: start..(*command_end).max(start),
                tokens: std::mem::take(cur_tokens),
                redirects: std::mem::take(cur_redirects),
                heredocs: std::mem::take(cur_heredocs),
            });
        }
        *command_end = 0;
        *last_end = 0;
    };

    for tok in tokens {
        match &tok.kind {
            TokenKind::Op(op) => {
                if matches!(
                    op,
                    Operator::Pipe
                        | Operator::And
                        | Operator::Or
                        | Operator::Semi
                        | Operator::Newline
                ) {
                    flush(
                        &mut stages,
                        &mut stage_start,
                        &mut command_end,
                        &mut last_end,
                        &mut cur_tokens,
                        &mut cur_redirects,
                        &mut cur_heredocs,
                    );
                } else if stage_start.is_some() {
                    last_end = last_end.max(tok.span.end);
                }
            }
            TokenKind::Word { .. } | TokenKind::Subst { .. } => {
                if stage_start.is_none() {
                    stage_start = Some(tok.span.start);
                }
                cur_tokens.push(tok.clone());
                command_end = tok.span.end;
                last_end = tok.span.end;
            }
            TokenKind::Redirect => {
                if stage_start.is_none() {
                    stage_start = Some(tok.span.start);
                }
                cur_redirects.push(tok.clone());
                last_end = tok.span.end;
            }
            TokenKind::Heredoc { .. } => {
                if stage_start.is_none() {
                    stage_start = Some(tok.span.start);
                }
                cur_heredocs.push(tok.clone());
                last_end = tok.span.end;
            }
            TokenKind::Comment => {
                if stage_start.is_some() {
                    last_end = last_end.max(tok.span.end);
                }
            }
        }
    }
    flush(
        &mut stages,
        &mut stage_start,
        &mut command_end,
        &mut last_end,
        &mut cur_tokens,
        &mut cur_redirects,
        &mut cur_heredocs,
    );
    stages
}

fn token_text<'a>(src: &'a str, tok: &Token) -> &'a str {
    &src[tok.span.clone()]
}

/// Strip a uniform surrounding quote pair from a word's raw source text, for
/// name matching only. An approximation: it does not unescape interior
/// backslash sequences, it only recognizes a single wrap spanning the whole
/// token.
fn literal_command_text(raw: &str) -> String {
    let bytes = raw.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'\'' || first == b'"') && first == last {
            let inner = &raw[1..raw.len() - 1];
            if !inner.contains(first as char) {
                return inner.to_owned();
            }
        }
    }
    raw.to_owned()
}

fn basename(word: &str) -> &str {
    match word.rfind('/') {
        Some(idx) if idx + 1 < word.len() => &word[idx + 1..],
        _ => word,
    }
}

fn is_assignment(s: &str) -> bool {
    let mut chars = s.char_indices();
    let Some((_, c0)) = chars.next() else {
        return false;
    };
    if !(c0.is_ascii_alphabetic() || c0 == '_') {
        return false;
    }
    for (i, c) in chars {
        if c == '=' {
            return i > 0;
        }
        if !(c.is_ascii_alphanumeric() || c == '_') {
            return false;
        }
    }
    false
}

fn is_skippable(src: &str, tok: &Token) -> bool {
    if !matches!(tok.kind, TokenKind::Word { .. }) {
        return false;
    }
    let literal = literal_command_text(token_text(src, tok));
    if literal.starts_with('-') {
        return true;
    }
    if !literal.is_empty() && literal.bytes().all(|b| b.is_ascii_digit()) {
        return true;
    }
    is_assignment(&literal)
}

/// Resolve the token that actually names the command run in this stage,
/// seeing through bare/`env` assignments and options-before-command
/// wrappers, chained.
///
/// Returns `(index into stage tokens, basename, is a bare substitution)`.
/// `basename` is empty when the resolved token is a substitution -- the
/// caller resolves that case separately.
fn resolve_binary_position(
    src: &str,
    stage_tokens: &[Token],
    wrappers: &[Wrapper],
) -> Option<(usize, String, bool)> {
    let mut idx = 0;
    loop {
        while idx < stage_tokens.len() && is_skippable(src, &stage_tokens[idx]) {
            idx += 1;
        }
        if idx >= stage_tokens.len() {
            return None;
        }
        let tok = &stage_tokens[idx];
        if matches!(tok.kind, TokenKind::Subst { .. }) {
            return Some((idx, String::new(), true));
        }
        let literal = literal_command_text(token_text(src, tok));
        let base = basename(&literal).to_owned();
        if wrappers
            .iter()
            .any(|w| w.name == base && w.options_before_command)
        {
            idx += 1;
            continue;
        }
        return Some((idx, base, false));
    }
}

fn is_dollar_var(raw: &str) -> bool {
    raw.starts_with('$') && !raw.starts_with("$(")
}

fn substitution_inner_text(src: &str, tok: &Token) -> Option<String> {
    let s = token_text(src, tok);
    if let Some(inner) = s.strip_prefix("$(").and_then(|s| s.strip_suffix(')')) {
        return Some(inner.to_owned());
    }
    if let Some(inner) = s.strip_prefix('`').and_then(|s| s.strip_suffix('`')) {
        return Some(inner.to_owned());
    }
    None
}

fn stage_is_opaque_interpreter(src: &str, stage: &Stage) -> bool {
    if !stage.heredocs.is_empty() {
        return true;
    }
    stage.tokens.iter().any(|t| {
        matches!(t.kind, TokenKind::Word { .. }) && literal_command_text(token_text(src, t)) == "-c"
    })
}

/// Why a call resolved to `Proxy`, kept distinguishable per the everything-
/// is-audited rule: a positional refusal and a genuine coverage gap must
/// never collapse into the same reason text (NFR-CMD-004 rev 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyReason {
    /// A managed equivalent is known by name, but the match sits in a
    /// pipeline stage that is neither the sole nor the last one, so
    /// splicing it in would change what the pipeline feeds downstream.
    Positional {
        replacement: String,
        position: usize,
    },
    /// No managed equivalent exists to evaluate this call against at all.
    CoverageUnknown { note: String },
}

impl ProxyReason {
    pub fn render(&self) -> String {
        match self {
            ProxyReason::Positional {
                replacement,
                position,
            } => format!(
                "known replacement `{replacement}` exists but stage {position} is not the sole \
                 or last pipeline stage; splicing here would change what the pipeline feeds \
                 downstream"
            ),
            ProxyReason::CoverageUnknown { note } => {
                format!("no managed equivalent to evaluate: {note}")
            }
        }
    }
}

/// What the tokenizer alone could determine about a Bash call.
///
/// `decision` is a TOKENIZER-LEVEL verdict, not the router's final answer:
/// it is `Deny`/`Ask` on an unparseable command, `Proxy` on a `$VAR` command
/// position or an opaque interpreter payload, and `Allow` otherwise -- even
/// when `matched` names a managed binary. A plain match with no tokenizer-
/// level trigger defers to the matched route's own patterns, which is
/// slice 3's job (the populated route table) and slice 3/9's wiring into
/// `Router::route`, both out of this issue's scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Analysis {
    pub stages: Vec<Stage>,
    pub matched: Option<Matched>,
    pub opaque: bool,
    pub decision: Decision,
}

/// Analyze a tool call against the shell-layer see-through set (FR-CMD-003)
/// and the opaque-interpreter route (FR-CMD-004).
///
/// `managed`, `interpreters`, and `wrappers` are data taken as parameters
/// (NFR-CMD-005) -- never a hard-coded list in this function.
pub fn analyze(
    call: &ToolCall,
    managed: &[String],
    interpreters: &[String],
    wrappers: &[Wrapper],
) -> Option<Analysis> {
    let ToolCall::Bash { command } = call else {
        return None;
    };

    let tokens = match scan(command) {
        Ok(t) => t,
        Err(e) => {
            return Some(Analysis {
                stages: Vec::new(),
                matched: None,
                opaque: false,
                decision: e.into_decision(),
            });
        }
    };
    let stages = build_stages(&tokens);

    let mut matched: Option<Matched> = None;
    let mut opaque = false;
    let mut decision = Decision::Allow;

    'stages: for (i, stage) in stages.iter().enumerate() {
        // A managed binary named INSIDE any substitution in this stage is
        // still Deny/Proxy-eligible, regardless of whether the substitution
        // sits in command position or argument position -- `echo $(gh pr
        // view 1)` names `gh` just as much as a bare `$(gh pr view 1)`
        // would.
        for tok in &stage.tokens {
            if !matches!(tok.kind, TokenKind::Subst { .. }) {
                continue;
            }
            let Some(inner) = substitution_inner_text(command, tok) else {
                continue;
            };
            let Ok(inner_tokens) = scan(&inner) else {
                continue;
            };
            let inner_stages = build_stages(&inner_tokens);
            let Some(first) = inner_stages.first() else {
                continue;
            };
            let Some((_, ibase, _)) = resolve_binary_position(&inner, &first.tokens, wrappers)
            else {
                continue;
            };
            if managed.iter().any(|m| m == &ibase) {
                matched = Some(Matched {
                    binary: ibase,
                    stage_span: i..i + 1,
                });
                decision = Decision::Proxy(
                    ProxyReason::CoverageUnknown {
                        note: "managed binary found inside a command substitution; not \
                               spliceable"
                            .into(),
                    }
                    .render(),
                );
                break 'stages;
            }
        }

        let Some((idx, base, is_subst)) = resolve_binary_position(command, &stage.tokens, wrappers)
        else {
            continue;
        };
        if is_subst {
            // Handled generically above; a bare substitution in command
            // position is just one case of "a Subst token in this stage".
            continue;
        }

        let raw = token_text(command, &stage.tokens[idx]);
        if is_dollar_var(raw) {
            decision = Decision::Proxy(
                ProxyReason::CoverageUnknown {
                    note: "command position is a variable expansion resolved at runtime".into(),
                }
                .render(),
            );
            break;
        }

        if interpreters.iter().any(|it| it == &base) && stage_is_opaque_interpreter(command, stage)
        {
            opaque = true;
            decision = Decision::Proxy(
                ProxyReason::CoverageUnknown {
                    note: format!("{base}: inline interpreter program is opaque, not parsed"),
                }
                .render(),
            );
            break;
        }

        if managed.iter().any(|m| m == &base) && matched.is_none() {
            matched = Some(Matched {
                binary: base,
                stage_span: i..i + 1,
            });
        }
    }

    Some(Analysis {
        stages,
        matched,
        opaque,
        decision,
    })
}

/// Splice a replacement into the command's sole-or-last stage, or explain
/// why it cannot be spliced.
///
/// A splice re-emits the original bytes outside `stage.command_span`
/// byte-for-byte and the replacement inside it -- a redirect attached to
/// the stage lives outside `command_span` and is carried across untouched.
/// A stage that is neither the sole nor the last one resolves to
/// `Proxy(ProxyReason::Positional)` instead: the splice would change what
/// the pipeline feeds downstream.
pub fn splice(
    command: &str,
    stages: &[Stage],
    stage_index: usize,
    replacement: &str,
    reason: &str,
) -> Decision {
    let stage = &stages[stage_index];
    let is_sole_or_last = stages.len() == 1 || stage_index == stages.len() - 1;
    if !is_sole_or_last {
        return Decision::Proxy(
            ProxyReason::Positional {
                replacement: replacement.to_owned(),
                position: stage_index,
            }
            .render(),
        );
    }
    let mut out = String::new();
    out.push_str(&command[..stage.command_span.start]);
    out.push_str(replacement);
    out.push_str(&command[stage.command_span.end..]);
    Decision::Rewrite {
        command: out,
        reason: reason.to_owned(),
        carry: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bash(cmd: &str) -> ToolCall {
        ToolCall::Bash {
            command: cmd.into(),
        }
    }

    fn wrapper(name: &str) -> Wrapper {
        Wrapper {
            name: name.into(),
            options_before_command: true,
        }
    }

    fn all_wrappers() -> Vec<Wrapper> {
        [
            "timeout", "nice", "sudo", "xargs", "command", "exec", "time", "env",
        ]
        .into_iter()
        .map(wrapper)
        .collect()
    }

    fn managed_gh() -> Vec<String> {
        vec!["gh".into()]
    }

    fn word_text(src: &str, tok: &Token) -> String {
        token_text(src, tok).to_owned()
    }

    // -- The evasion-class table: >= 27 constructs, each proving a managed
    // binary hidden behind it is still detected. --

    #[test]
    fn evasion_class_table_detects_gh_behind_every_named_construct() {
        let cases: &[(&str, &str)] = &[
            ("plain", "gh pr view 1"),
            ("pipe first stage", "gh pr list | wc -l"),
            ("pipe second stage", "wc -l | gh pr list"),
            ("sequencing and", "cd /tmp && gh pr view 1"),
            ("sequencing semi", "echo hi; gh pr view 1"),
            ("sequencing or", "false || gh pr view 1"),
            ("sequencing newline", "echo hi\ngh pr view 1"),
            ("env prefix explicit", "env X=1 gh pr view 1"),
            ("env prefix bare", "X=1 gh pr view 1"),
            ("env prefix chained", "A=1 B=2 gh pr view 1"),
            ("wrapper timeout", "timeout 5 gh pr view 1"),
            ("wrapper nice", "nice -n 5 gh pr view 1"),
            ("wrapper sudo", "sudo -E gh pr view 1"),
            ("wrapper xargs", "xargs -0 gh pr view 1"),
            ("wrapper command", "command gh pr view 1"),
            ("wrapper exec", "exec gh pr view 1"),
            ("wrapper time", "time gh pr view 1"),
            ("wrapper env -S", "env -S gh pr view 1"),
            ("absolute path", "/usr/bin/gh pr view 1"),
            ("nested absolute path", "/usr/local/bin/gh pr view 1"),
            ("single-quoted arg", "gh pr view '1'"),
            ("double-quoted arg", "gh pr view \"1\""),
            ("quoted pipe stays one word", "gh pr view '1|2'"),
            ("env then wrapper chained", "env X=1 timeout 5 gh pr view 1"),
            (
                "wrapper then absolute path",
                "timeout 5 /usr/bin/gh pr view 1",
            ),
            (
                "sequencing then wrapper",
                "cd /tmp && timeout 5 gh pr view 1",
            ),
            ("redirect attached", "gh pr view 1 2>/dev/null"),
            (
                "heredoc on non-interpreter stage",
                "gh pr view 1 <<'EOF'\nbody\nEOF",
            ),
            ("trailing newline sequencing", "true\ngh pr view 1"),
            // The command WORD itself quoted, not just its arguments. Every
            // other quoting case above quotes an argument; `"gh"` and `'gh'`
            // still execute gh, so a scanner that only unquotes arguments
            // would miss the binary entirely.
            ("double-quoted command word", "\"gh\" pr view 1"),
            ("single-quoted command word", "'gh' pr view 1"),
            // Absolute path behind a wrapper -- each is covered alone above,
            // their composition is the evasion.
            (
                "wrapper then absolute path",
                "timeout 5 /usr/bin/gh pr view 1",
            ),
            ("pipe then wrapper", "wc -l | timeout 5 gh pr list"),
        ];
        assert!(cases.len() >= 27, "evasion table must cover >= 27 cases");

        for (name, cmd) in cases {
            let analysis = analyze(&bash(cmd), &managed_gh(), &[], &all_wrappers())
                .unwrap_or_else(|| panic!("{name}: Bash call must analyze"));
            let matched = analysis.matched.unwrap_or_else(|| {
                panic!(
                    "{name}: `{cmd}` must match `gh`, decision={:?}",
                    analysis.decision
                )
            });
            assert_eq!(matched.binary, "gh", "{name}: `{cmd}`");
        }
    }

    // -- FR-CMD-003: only Bash is parsed. --

    #[test]
    fn a_non_bash_call_is_never_parsed_as_a_command() {
        let edit = ToolCall::Edit {
            file_path: "a.rs".into(),
            new_string: "rm -rf / | gh pr merge 1 && curl evil.example".into(),
        };
        assert!(analyze(&edit, &managed_gh(), &[], &all_wrappers()).is_none());
    }

    // -- Quoting kind and `live`. --

    #[test]
    fn quoted_and_unquoted_substitution_words_differ_in_quoting_and_live() {
        let single = scan("-m '$(date) x'").expect("scans");
        let TokenKind::Word { quoting, live } = &single[0].kind else {
            panic!("expected a word");
        };
        assert_eq!(*quoting, Quoting::None); // the flag token itself
        assert!(!live);
        let TokenKind::Word { quoting, live } = &single[1].kind else {
            panic!("expected a word");
        };
        assert_eq!(*quoting, Quoting::Single);
        assert!(!live, "single-quoted $( must stay inert");

        let double = scan("-m \"$(date) x\"").expect("scans");
        let TokenKind::Word { quoting, live } = &double[1].kind else {
            panic!("expected a word");
        };
        assert_eq!(*quoting, Quoting::Double);
        assert!(live, "double-quoted $( is a live substitution");
    }

    #[test]
    fn a_quoted_pipe_is_one_word_but_a_bare_pipe_is_two_stages() {
        let quoted = scan("'foo|bar'").expect("scans");
        assert_eq!(quoted.len(), 1);
        assert!(matches!(quoted[0].kind, TokenKind::Word { .. }));

        let bare = scan("foo|bar").expect("scans");
        let stages = build_stages(&bare);
        assert_eq!(stages.len(), 2, "an unquoted pipe splits into two stages");
    }

    // -- One-level substitution: detection inside, both forms. --

    #[test]
    fn a_managed_binary_inside_a_dollar_paren_substitution_is_still_detected() {
        let analysis = analyze(
            &bash("echo $(gh pr view 1)"),
            &managed_gh(),
            &[],
            &all_wrappers(),
        )
        .expect("analyzes");
        let matched = analysis.matched.expect("substitution content is scanned");
        assert_eq!(matched.binary, "gh");
    }

    #[test]
    fn a_managed_binary_inside_a_backtick_substitution_is_still_detected() {
        let analysis = analyze(
            &bash("echo `gh pr view 1`"),
            &managed_gh(),
            &[],
            &all_wrappers(),
        )
        .expect("analyzes");
        let matched = analysis.matched.expect("backtick content is scanned");
        assert_eq!(matched.binary, "gh");
    }

    #[test]
    fn nesting_beyond_one_level_denies_and_names_the_nesting() {
        for cmd in ["echo $(gh $(echo x))", "echo `gh $(echo x)`"] {
            let err = scan(cmd).expect_err("must refuse deeper nesting");
            assert!(matches!(err, ScanError::NestingExceeded { .. }));
            let decision = err.into_decision();
            match decision {
                Decision::Deny(reason) => assert!(
                    reason.contains("nesting"),
                    "reason must name the nesting: {reason}"
                ),
                other => panic!("expected Deny, got {other:?}: {cmd}"),
            }
        }
    }

    #[test]
    fn a_single_quoted_dollar_paren_is_inert_text_not_a_substitution_boundary() {
        let tokens = scan("'$(rm -rf /)'").expect("must scan without error");
        assert_eq!(tokens.len(), 1);
        let TokenKind::Word { quoting, live } = &tokens[0].kind else {
            panic!("expected a word, got {:?}", tokens[0].kind);
        };
        assert_eq!(*quoting, Quoting::Single);
        assert!(!live);

        // And detection must not fire on it: even with a managed binary
        // that would match `rm`, an inert single-quoted word is not a
        // command-position match at all.
        let analysis = analyze(&bash("'$(rm -rf /)'"), &["rm".into()], &[], &all_wrappers())
            .expect("analyzes");
        assert_eq!(
            analysis.matched, None,
            "a fully single-quoted literal must not be treated as a command"
        );
    }

    // -- $VAR in command position: Proxy, never Allow. --

    #[test]
    fn a_dollar_var_in_command_position_resolves_to_proxy_never_allow() {
        let analysis = analyze(&bash("$CMD pr view 1"), &managed_gh(), &[], &all_wrappers())
            .expect("analyzes");
        match analysis.decision {
            Decision::Proxy(reason) => assert!(
                reason.contains("variable expansion"),
                "reason should name the cause: {reason}"
            ),
            other => panic!("expected Proxy, got {other:?}"),
        }
    }

    // -- Splice: rewrite when sole/last, positional Proxy otherwise. --

    #[test]
    fn a_matched_binary_in_the_last_stage_is_spliced_as_a_rewrite() {
        let cmd = "cd X && gh pr view 5";
        let tokens = scan(cmd).expect("scans");
        let stages = build_stages(&tokens);
        assert_eq!(stages.len(), 2);
        let analysis = analyze(&bash(cmd), &managed_gh(), &[], &all_wrappers()).expect("analyzes");
        let matched = analysis.matched.expect("gh is matched");
        let stage_index = matched.stage_span.start;
        assert_eq!(stage_index, 1, "gh sits in the last stage");

        let decision = splice(
            cmd,
            &stages,
            stage_index,
            "legion pr view 5",
            "work-source actions go through legion",
        );
        match decision {
            Decision::Rewrite {
                command,
                reason,
                carry,
            } => {
                assert_eq!(command, "cd X && legion pr view 5");
                assert_eq!(reason, "work-source actions go through legion");
                assert!(carry.is_empty());
            }
            other => panic!("expected Rewrite, got {other:?}"),
        }
    }

    #[test]
    fn a_matched_binary_in_a_non_last_stage_is_proxy_for_a_positional_reason() {
        let cmd = "gh pr list | wc -l";
        let tokens = scan(cmd).expect("scans");
        let stages = build_stages(&tokens);
        assert_eq!(stages.len(), 2);
        let analysis = analyze(&bash(cmd), &managed_gh(), &[], &all_wrappers()).expect("analyzes");
        let matched = analysis.matched.expect("gh is matched");
        let stage_index = matched.stage_span.start;
        assert_eq!(stage_index, 0, "gh sits in the first, non-last stage");

        let decision = splice(cmd, &stages, stage_index, "legion pr list", "because");
        match decision {
            Decision::Proxy(reason) => {
                assert!(
                    reason.contains("legion pr list"),
                    "names the replacement: {reason}"
                );
                assert!(reason.contains('0'), "names the position: {reason}");
            }
            other => panic!("expected Proxy, got {other:?}"),
        }
    }

    #[test]
    fn the_positional_proxy_and_the_coverage_unknown_proxy_carry_different_reasons() {
        // gh pr list | wc -l -- a known replacement, refused for a
        // structural (positional) reason.
        let known_cmd = "gh pr list | wc -l";
        let known_tokens = scan(known_cmd).expect("scans");
        let known_stages = build_stages(&known_tokens);
        let positional = splice(known_cmd, &known_stages, 0, "legion pr list", "because");

        // python3 -c "..." -- no managed equivalent exists at all.
        let coverage_cmd = "python3 -c \"import os\"";
        let coverage = analyze(
            &bash(coverage_cmd),
            &managed_gh(),
            &["python3".into()],
            &all_wrappers(),
        )
        .expect("analyzes");

        let (Decision::Proxy(positional_reason), Decision::Proxy(coverage_reason)) =
            (positional, coverage.decision)
        else {
            panic!("both cases must be Proxy");
        };
        assert_ne!(
            positional_reason, coverage_reason,
            "a positional refusal must not read like a coverage gap"
        );
        assert!(positional_reason.contains("legion pr list"));
        assert!(coverage_reason.contains("opaque") || coverage_reason.contains("no managed"));
    }

    #[test]
    fn redirects_attached_to_a_spliced_stage_are_carried_across_the_splice() {
        let cmd = "gh pr view 5 2>/dev/null";
        let tokens = scan(cmd).expect("scans");
        let stages = build_stages(&tokens);
        assert_eq!(stages.len(), 1);
        assert_eq!(stages[0].redirects.len(), 1);

        let decision = splice(cmd, &stages, 0, "legion pr view 5", "because");
        match decision {
            Decision::Rewrite { command, .. } => {
                assert_eq!(command, "legion pr view 5 2>/dev/null");
            }
            other => panic!("expected Rewrite, got {other:?}"),
        }
    }

    // -- Opaque interpreter route (FR-CMD-004). --

    #[test]
    fn an_interpreter_c_payload_is_proxy_and_opaque_never_deny_never_allow() {
        let cmd = "python3 -c \"import os; os.system('gh pr merge 1')\"";
        let analysis = analyze(
            &bash(cmd),
            &managed_gh(),
            &["python3".into()],
            &all_wrappers(),
        )
        .expect("analyzes");
        assert!(analysis.opaque, "an interpreter -c payload is opaque");
        assert!(
            matches!(analysis.decision, Decision::Proxy(_)),
            "expected Proxy, got {:?}",
            analysis.decision
        );
        assert_eq!(
            analysis.matched, None,
            "the tokenizer never resolves a match inside the -c payload"
        );
        // And the payload word itself is kept as one token -- the scanner
        // never split it looking for shell syntax inside.
        let stage = &analysis.stages[0];
        assert_eq!(stage.tokens.len(), 3, "python3, -c, and the payload word");
        assert!(word_text(cmd, &stage.tokens[2]).contains("gh pr merge 1"));
    }

    #[test]
    fn an_interpreter_heredoc_payload_is_proxy_and_opaque() {
        let cmd = "sh <<'EOF'\nrm -rf /\nEOF";
        let analysis = analyze(&bash(cmd), &[], &["sh".into()], &all_wrappers()).expect("analyzes");
        assert!(analysis.opaque);
        assert!(matches!(analysis.decision, Decision::Proxy(_)));
        assert_eq!(analysis.matched, None);
        // The heredoc body is never tokenized as words: the only stage
        // holds just the interpreter's own name.
        assert_eq!(analysis.stages.len(), 1);
        assert_eq!(analysis.stages[0].tokens.len(), 1);
    }

    // -- Managed/wrapper lists are parameters, not match arms. --

    #[test]
    fn managed_and_wrapper_lists_are_read_from_the_parameters_not_hardcoded() {
        let cmd = "curl https://example.com";
        let none = analyze(&bash(cmd), &managed_gh(), &[], &[]).expect("analyzes");
        assert_eq!(
            none.matched, None,
            "curl is not in this call's managed list"
        );

        let some = analyze(&bash(cmd), &["curl".into()], &[], &[]).expect("analyzes");
        assert_eq!(
            some.matched.map(|m| m.binary),
            Some("curl".into()),
            "the same scanner call detects curl once it is in the list"
        );
    }

    // -- Error handling: unresolved boundaries. --

    #[test]
    fn an_unbalanced_quote_asks_rather_than_denies_or_allows() {
        let err = scan("gh pr view \"unterminated").expect_err("must refuse");
        assert!(matches!(err, ScanError::UnbalancedQuote { .. }));
        assert!(matches!(err.into_decision(), Decision::Ask(_)));
    }

    #[test]
    fn an_unterminated_substitution_asks() {
        let err = scan("echo $(gh pr view 1").expect_err("must refuse");
        assert!(matches!(err, ScanError::UnterminatedSubstitution { .. }));
        assert!(matches!(err.into_decision(), Decision::Ask(_)));
    }

    #[test]
    fn an_unterminated_heredoc_asks() {
        let err = scan("cat <<EOF\nbody without a terminator").expect_err("must refuse");
        assert!(matches!(err, ScanError::UnterminatedHeredoc { .. }));
        assert!(matches!(err.into_decision(), Decision::Ask(_)));
    }

    #[test]
    fn a_scan_error_denies_or_asks_even_with_no_managed_binary_listed() {
        // Order matters: the scan failure must be checked before any
        // binary-matching logic, so this must refuse even when nothing is
        // in the managed list at all -- a vacuous managed list must not
        // make garbage input Allow.
        let analysis = analyze(&bash("echo \"unterminated"), &[], &[], &[]).expect("analyzes");
        assert_ne!(analysis.decision, Decision::Allow);
    }

    // -- Fuzz/adversarial suite: malformed input never Allows. --

    #[test]
    fn malformed_and_evasive_input_never_resolves_to_allow() {
        let adversarial = [
            "echo \"unterminated",
            "echo 'unterminated",
            "echo $(unterminated",
            "echo `unterminated",
            "cat <<EOF\nno terminator here",
            "echo $(a $(b))",
            "echo `a $(b)`",
            "echo $(a $(b $(c)))",
            "echo \"$(a \"$(b)\")\"",
            "echo \"$(",
            "echo '$(",
            "gh pr view \"$(nested $(deep))\"",
        ];
        for cmd in adversarial {
            let analysis =
                analyze(&bash(cmd), &[], &[], &[]).unwrap_or_else(|| panic!("{cmd}: must analyze"));
            assert_ne!(
                analysis.decision,
                Decision::Allow,
                "adversarial input must never silently Allow: {cmd}"
            );
        }
    }
}
