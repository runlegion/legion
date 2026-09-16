//! Word-level scanning: quotes, escapes, substitutions, and heredocs. No
//! grammar lives here -- `Scanner::run` produces a flat token stream that
//! `resolve` groups into simple commands and dispatches on.

use super::ScanError;

/// One shell word. `text` is the word's unquoted spelling: the scanner
/// expands nothing, it only strips quote and escape syntax.
#[derive(Debug, Clone, Default)]
pub(super) struct Word {
    pub(super) text: String,
    /// Any part of the word was quoted or backslash-escaped.
    pub(super) quoted: bool,
    /// The word is exactly one command substitution or backtick
    /// substitution, and nothing else -- `$(which rg)` as a whole word.
    pub(super) whole_subst: bool,
    /// Inner source text of each `$()`, backtick, or `<()`/`>()` inside the
    /// word, in order.
    pub(super) substs: Vec<String>,
}

#[derive(Debug, Clone)]
pub(super) enum Tok {
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

/// A `<<`/`<<-` heredoc waiting for its body, once the current line ends.
struct PendingHeredoc {
    delimiter: String,
    strip_tabs: bool,
    /// Index into the token stream of the `Tok::Heredoc` placeholder this
    /// pending heredoc's body will fill in.
    slot: usize,
}

/// A `<<`/`<<-` heredoc nested inside a substitution or function body,
/// whose text is skipped as literal rather than attached to any token
/// (RESEARCH-CMD-bounded-tokenizer caveat: a heredoc body inside `$()` must
/// be skipped as literal text, or the commit-message idiom mis-parses).
struct InlineHeredoc {
    delimiter: String,
    strip_tabs: bool,
}

pub(super) fn is_meta(ch: char) -> bool {
    matches!(ch, ';' | '&' | '|' | '(' | ')' | '<' | '>' | '\n')
}

/// What a `$...` construct parsed to: its literal replacement text, the
/// inner source of a command substitution when it is one (so the caller can
/// recurse into it and, for a bare `$(...)` word, count it toward
/// [`Word::whole_subst`]), and whether it was quoted (`$'...'`).
struct DollarPart {
    text: String,
    subst: Option<String>,
    quoted: bool,
}

pub(super) struct Scanner {
    c: Vec<char>,
    i: usize,
}

impl Scanner {
    pub(super) fn new(src: &str) -> Self {
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

    pub(super) fn run(&mut self) -> Result<Vec<Tok>, ScanError> {
        let mut toks = Vec::new();
        let mut pending: Vec<PendingHeredoc> = Vec::new();
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
                ';' | '&' | '|' => {
                    if ch == '&' && self.peek(1) == Some('>') {
                        self.i += if self.peek(2) == Some('>') { 3 } else { 2 };
                        toks.push(Tok::Redirect);
                    } else {
                        let two_char = matches!(
                            (ch, self.peek(1)),
                            (';', Some(';'))
                                | ('&', Some('&'))
                                | ('|', Some('|'))
                                | ('|', Some('&'))
                        );
                        self.i += if two_char { 2 } else { 1 };
                        toks.push(Tok::Sep);
                    }
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
                    if at_token_start && let Some(header_len) = self.peek_function_def() {
                        let body = self.read_function_def(header_len)?;
                        toks.push(Tok::FunctionDef { body });
                    } else if let Some(w) = self.read_word()? {
                        toks.push(Tok::Word(w));
                    }
                    at_token_start = false;
                }
            }
        }
        if let Some(first) = pending.first() {
            return Err(ScanError::UnterminatedHeredoc {
                delimiter: first.delimiter.clone(),
                offset: self.byte_offset(self.i),
            });
        }
        Ok(toks)
    }

    fn read_redirect(
        &mut self,
        pending: &mut Vec<PendingHeredoc>,
        toks: &mut Vec<Tok>,
    ) -> Result<(), ScanError> {
        if self.starts("<(") || self.starts(">(") {
            self.i += 2;
            let offset = self.i;
            let inner = self.read_balanced('(', ')', 1, true)?.ok_or(
                ScanError::UnterminatedSubstitution {
                    offset: self.byte_offset(offset),
                },
            )?;
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
            let (delim, strip) = self.read_heredoc_op()?;
            if delim.is_empty() {
                return Err(ScanError::MissingHeredocDelimiter {
                    offset: self.byte_offset(op_offset),
                });
            }
            let slot = toks.len();
            toks.push(Tok::Heredoc {
                body: String::new(),
            });
            pending.push(PendingHeredoc {
                delimiter: delim,
                strip_tabs: strip,
                slot,
            });
            return Ok(());
        }
        let mut op_len = 0usize;
        while let Some(c) = self.peek(0) {
            if matches!(c, '<' | '>' | '&' | '|') && op_len < 3 {
                op_len += 1;
                self.i += 1;
            } else {
                break;
            }
        }
        toks.push(Tok::Redirect);
        Ok(())
    }

    /// Reads a `<<`/`<<-` operator already confirmed present at the current
    /// position: the optional `-` strip flag, the whitespace before the
    /// delimiter, and the delimiter word itself. Returns an empty delimiter
    /// if none was found.
    fn read_heredoc_op(&mut self) -> Result<(String, bool), ScanError> {
        self.i += 2; // "<<"
        let strip = self.peek(0) == Some('-');
        if strip {
            self.i += 1;
        }
        while matches!(self.peek(0), Some(' ' | '\t')) {
            self.i += 1;
        }
        Ok((self.read_heredoc_delimiter()?, strip))
    }

    /// A heredoc delimiter word: unquoted, single-, or double-quoted, with
    /// quoting stripped. Quoting only controls expansion inside the body,
    /// which this scanner never performs, so only the bare text matters.
    /// An unterminated quote stops at the newline rather than consuming the
    /// rest of the input looking for a closing quote that may not exist.
    fn read_heredoc_delimiter(&mut self) -> Result<String, ScanError> {
        let mut delim = String::new();
        while let Some(c) = self.peek(0) {
            if c.is_whitespace() || is_meta(c) {
                break;
            }
            match c {
                '\'' | '"' => {
                    let quote_offset = self.i;
                    self.i += 1;
                    let mut closed = false;
                    while let Some(q) = self.peek(0) {
                        if q == '\n' {
                            break;
                        }
                        self.i += 1;
                        if q == c {
                            closed = true;
                            break;
                        }
                        delim.push(q);
                    }
                    if !closed {
                        return Err(if c == '\'' {
                            ScanError::UnterminatedSingleQuote {
                                offset: self.byte_offset(quote_offset),
                            }
                        } else {
                            ScanError::UnterminatedDoubleQuote {
                                offset: self.byte_offset(quote_offset),
                            }
                        });
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
        Ok(delim)
    }

    /// Reads lines until one equals `delim` (after stripping leading tabs,
    /// when `strip_tabs`), returning the body with that delimiter line
    /// excluded. `None` if EOF is reached first.
    fn read_heredoc_body(&mut self, delim: &str, strip_tabs: bool) -> Option<String> {
        let mut body = String::new();
        while self.i < self.c.len() {
            let start = self.i;
            while self.i < self.c.len() && self.c[self.i] != '\n' {
                self.i += 1;
            }
            let line: String = self.c[start..self.i].iter().collect();
            if self.i < self.c.len() {
                self.i += 1;
            }
            let cmp = if strip_tabs {
                line.trim_start_matches('\t')
            } else {
                line.as_str()
            };
            if cmp == delim {
                return Some(body);
            }
            body.push_str(&line);
            body.push('\n');
        }
        None
    }

    fn read_heredoc_bodies(
        &mut self,
        pending: &mut Vec<PendingHeredoc>,
        toks: &mut [Tok],
    ) -> Result<(), ScanError> {
        for p in std::mem::take(pending) {
            let start_offset = self.byte_offset(self.i);
            let body = self.read_heredoc_body(&p.delimiter, p.strip_tabs).ok_or(
                ScanError::UnterminatedHeredoc {
                    delimiter: p.delimiter.clone(),
                    offset: start_offset,
                },
            )?;
            if let Some(Tok::Heredoc { body: b }) = toks.get_mut(p.slot) {
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
                    match self.peek(1) {
                        Some(n) => {
                            w.text.push(n);
                            self.i += 2;
                        }
                        None => {
                            // A lone trailing backslash at the end of the
                            // input escapes nothing: keep it as a literal
                            // `\` rather than silently dropping it.
                            w.text.push('\\');
                            self.i += 1;
                        }
                    }
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
                            offset: self.byte_offset(quote_offset),
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
                    let inner = self.read_backtick()?;
                    if w.text.is_empty() {
                        substs_at_start += 1;
                    }
                    w.text.push_str("`...`");
                    w.substs.push(inner);
                }
                '$' => {
                    let part = self.read_dollar()?;
                    if part.quoted {
                        w.quoted = true;
                    }
                    if let Some(inner) = part.subst {
                        if w.text.is_empty() {
                            substs_at_start += 1;
                        }
                        w.substs.push(inner);
                    }
                    w.text.push_str(&part.text);
                }
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

    /// A backtick substitution's inner source, with backslash escapes
    /// resolved. Shared by [`Self::read_word`] and [`Self::read_double`],
    /// which must agree on escape handling.
    fn read_backtick(&mut self) -> Result<String, ScanError> {
        let tick_offset = self.i;
        self.i += 1; // opening `
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
                offset: self.byte_offset(tick_offset),
            });
        }
        Ok(inner)
    }

    fn read_dollar(&mut self) -> Result<DollarPart, ScanError> {
        if self.starts("$((") {
            self.i += 3;
            let offset = self.i;
            let inner = self.read_balanced('(', ')', 2, false)?.ok_or(
                ScanError::UnterminatedSubstitution {
                    offset: self.byte_offset(offset),
                },
            )?;
            Ok(DollarPart {
                text: format!("$(({inner}))"),
                subst: None,
                quoted: false,
            })
        } else if self.starts("$(") {
            self.i += 2;
            let offset = self.i;
            let inner = self.read_balanced('(', ')', 1, true)?.ok_or(
                ScanError::UnterminatedSubstitution {
                    offset: self.byte_offset(offset),
                },
            )?;
            Ok(DollarPart {
                text: "$(...)".to_string(),
                subst: Some(inner),
                quoted: false,
            })
        } else if self.starts("${") {
            let offset = self.i;
            self.i += 2;
            let inner = self.read_balanced('{', '}', 1, false)?.ok_or(
                ScanError::UnterminatedSubstitution {
                    offset: self.byte_offset(offset),
                },
            )?;
            Ok(DollarPart {
                text: format!("${{{inner}}}"),
                subst: None,
                quoted: false,
            })
        } else if self.starts("$'") {
            let quote_offset = self.i;
            self.i += 2;
            let mut text = String::new();
            let mut closed = false;
            while let Some(c) = self.peek(0) {
                self.i += 1;
                if c == '\\' {
                    if let Some(n) = self.peek(0) {
                        text.push(n);
                        self.i += 1;
                    }
                    continue;
                }
                if c == '\'' {
                    closed = true;
                    break;
                }
                text.push(c);
            }
            if !closed {
                return Err(ScanError::UnterminatedSingleQuote {
                    offset: self.byte_offset(quote_offset),
                });
            }
            Ok(DollarPart {
                text,
                subst: None,
                quoted: true,
            })
        } else {
            self.i += 1;
            let mut text = String::from('$');
            let mut named = false;
            while let Some(c) = self.peek(0) {
                if c.is_ascii_alphanumeric()
                    || c == '_'
                    || (!named && matches!(c, '@' | '*' | '#' | '?' | '!' | '-'))
                {
                    text.push(c);
                    self.i += 1;
                    named = true;
                    if !(c.is_ascii_alphanumeric() || c == '_') {
                        break;
                    }
                } else {
                    break;
                }
            }
            Ok(DollarPart {
                text,
                subst: None,
                quoted: false,
            })
        }
    }

    /// Inside double quotes: escapes, `$()`, backticks, `${}`; stops at the
    /// closing quote.
    fn read_double(&mut self, w: &mut Word, quote_offset: usize) -> Result<(), ScanError> {
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
                    let part = self.read_dollar()?;
                    if let Some(inner) = part.subst {
                        w.substs.push(inner);
                    }
                    w.text.push_str(&part.text);
                }
                '`' => {
                    let inner = self.read_backtick()?;
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
            offset: self.byte_offset(quote_offset),
        })
    }

    /// Reads to the char that closes an already-consumed `open`/`close`
    /// pair, given how many are already open (`initial_depth`: 1 for a
    /// single `$(`/`{`, 2 for `$((` which needs two `)` to close). Returns
    /// `None` on EOF before closing.
    ///
    /// When `quote_aware`, quotes, backticks, and heredocs inside are
    /// skipped as literal text rather than scanned for `open`/`close`
    /// (RESEARCH-CMD-bounded-tokenizer's heredoc-in-`$()` caveat): this is
    /// used for `$()`, `<()`/`>()`, and function bodies. Arithmetic
    /// (`$(( ))`) is not quote-aware, matching real shell grammar where
    /// `<<` there is the left-shift operator, not a heredoc.
    fn read_balanced(
        &mut self,
        open: char,
        close: char,
        initial_depth: usize,
        quote_aware: bool,
    ) -> Result<Option<String>, ScanError> {
        let start = self.i;
        let mut depth = initial_depth;
        let mut heredocs: Vec<InlineHeredoc> = Vec::new();
        while let Some(c) = self.peek(0) {
            if quote_aware {
                match c {
                    '<' if self.peek(1) == Some('<') && self.peek(2) != Some('<') => {
                        let (delimiter, strip_tabs) = self.read_heredoc_op()?;
                        if !delimiter.is_empty() {
                            heredocs.push(InlineHeredoc {
                                delimiter,
                                strip_tabs,
                            });
                        }
                        continue;
                    }
                    '\n' if !heredocs.is_empty() => {
                        self.i += 1;
                        for h in std::mem::take(&mut heredocs) {
                            let _ = self.read_heredoc_body(&h.delimiter, h.strip_tabs);
                        }
                        continue;
                    }
                    '\\' => {
                        self.i += 2;
                        continue;
                    }
                    '\'' => {
                        self.i += 1;
                        while let Some(q) = self.peek(0) {
                            self.i += 1;
                            if q == '\'' {
                                break;
                            }
                        }
                        continue;
                    }
                    '"' => {
                        self.i += 1;
                        self.skip_double()?;
                        continue;
                    }
                    '`' => {
                        self.i += 1;
                        while let Some(q) = self.peek(0) {
                            self.i += 1;
                            if q == '`' {
                                break;
                            }
                        }
                        continue;
                    }
                    _ => {}
                }
            }
            if c == open {
                depth += 1;
                self.i += 1;
            } else if c == close {
                depth -= 1;
                self.i += 1;
                if depth == 0 {
                    let end = self.i.saturating_sub(initial_depth).max(start);
                    return Ok(Some(self.c[start..end.min(self.c.len())].iter().collect()));
                }
            } else {
                self.i += 1;
            }
        }
        Ok(None)
    }

    fn skip_double(&mut self) -> Result<(), ScanError> {
        while let Some(c) = self.peek(0) {
            match c {
                '\\' => self.i += 2,
                '"' => {
                    self.i += 1;
                    return Ok(());
                }
                '$' if self.peek(1) == Some('(') => {
                    self.i += 2;
                    let _ = self.read_balanced('(', ')', 1, true)?;
                }
                _ => self.i += 1,
            }
        }
        Ok(())
    }

    /// Returns the byte length (in chars) consumed by a function-definition
    /// header (up to and including the opening `{`), if the input at the
    /// current position begins one: `function NAME [()] {` or `NAME() {`.
    /// Rollback-free: it only peeks.
    fn peek_function_def(&self) -> Option<usize> {
        let mut j = self.i;
        let has_keyword = self.word_at(j).as_deref() == Some("function");
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

    /// Reads a function body, given the header length [`Self::peek_function_def`]
    /// already found at the current position. Taking that length as a
    /// parameter (rather than recomputing it here) means there is no panic
    /// path: the caller only calls this after a successful peek.
    fn read_function_def(&mut self, header_len: usize) -> Result<String, ScanError> {
        let header_start = self.i;
        self.i = header_start + header_len; // just past the opening `{`
        let brace_offset = self.byte_offset(self.i - 1);
        self.read_balanced('{', '}', 1, true)?
            .ok_or(ScanError::UnterminatedFunctionBody {
                offset: brace_offset,
            })
    }
}

pub(super) fn skip_ws(c: &[char], mut idx: usize) -> usize {
    while matches!(c.get(idx), Some(' ' | '\t')) {
        idx += 1;
    }
    idx
}

pub(super) fn skip_ws_and_newlines(c: &[char], mut idx: usize) -> usize {
    while matches!(c.get(idx), Some(' ' | '\t' | '\r' | '\n' | ';')) {
        idx += 1;
    }
    idx
}
