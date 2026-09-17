//! Grouping the lexer's token stream into simple commands, and resolving
//! each command position to an [`Invocation`] or an [`Opaque`] region.

use super::lexer::{Scanner, SepKind, Tok, Word};
use super::tables::{
    CLAUSE_KEYWORDS, END_KEYWORDS, INTERPRETERS, LOOKUPS, PREFIX_KEYWORDS, SCRIPT_EXTENSIONS,
    SHELLS, TWO_WORD_WRAPPERS, TwoWordWrapper, UNKNOWN_WRAPPERS, WRAPPERS,
};
use super::{Invocation, MAX_DEPTH, Opaque, Position, Scan, ScanError};

#[derive(Default)]
struct Cmd {
    words: Vec<Word>,
    heredocs: Vec<String>,
    after_sep: bool,
    /// At least one redirect operator (`>`, `<`, `>>`, ...) appeared in
    /// this command. The redirect target itself is never kept as a word
    /// (see the `Tok::Word` arm below), so this is the only trace of it --
    /// needed by [`walk`] to decide FR-CMD-008's whole-command atomicity
    /// fact, since a redirect changes the command's effect the same way a
    /// pipe or `&&` does.
    has_redirect: bool,
}

enum CmdOrDef {
    Cmd(Cmd),
    Def { body: String },
}

/// Groups `toks` into simple commands and definitions, and reports
/// whether any [`SepKind::Other`] separator appeared anywhere in the
/// stream (FR-CMD-008): a background `&`, a pipe, `&&`/`||`, a `;;` case
/// terminator, or a `(...)` subshell boundary. This is a property of the
/// whole token stream, not of any single [`Cmd`] -- a *trailing* `&`
/// leaves no second command for a group count to catch (nothing follows
/// it), so [`walk`] cannot infer it from `out`'s length alone.
fn group(toks: Vec<Tok>, out: &mut Vec<CmdOrDef>) -> bool {
    let mut cur = Cmd::default();
    let mut redirect_pending = false;
    let mut saw_other_operator = false;
    for t in toks {
        match t {
            Tok::Sep(kind) => {
                if kind == SepKind::Other {
                    saw_other_operator = true;
                }
                if !cur.words.is_empty() || !cur.heredocs.is_empty() {
                    out.push(CmdOrDef::Cmd(std::mem::take(&mut cur)));
                }
                cur.after_sep = true;
                redirect_pending = false;
            }
            Tok::Redirect => {
                redirect_pending = true;
                // Marked here, where the redirect operator itself is
                // produced, not where its target word is consumed: a
                // target-less or glued-digit redirect (`>&2`, `2>/dev/null`)
                // may never reach the `Tok::Word` branch below, and this
                // trace must not depend on it doing so.
                cur.has_redirect = true;
            }
            Tok::Heredoc { body } => cur.heredocs.push(body),
            Tok::Word(w) => {
                if redirect_pending {
                    // Redirect targets are not command words.
                    redirect_pending = false;
                } else {
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
    saw_other_operator
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

pub(super) fn walk(
    src: &str,
    depth: u8,
    inherited: Option<Position>,
    out: &mut Scan,
) -> Result<(), ScanError> {
    let toks = Scanner::new(src).run()?;
    let mut grouped = Vec::new();
    let saw_other_operator = group(toks, &mut grouped);
    for item in &grouped {
        match item {
            CmdOrDef::Cmd(cmd) => {
                let position = inherited.unwrap_or(if cmd.after_sep {
                    Position::AfterOperator
                } else {
                    Position::First
                });
                for w in &cmd.words {
                    for inner in &w.substs {
                        descend(inner, depth, position.max(Position::Substitution), out)?;
                    }
                }
                resolve_from(&cmd.words, &cmd.heredocs, 0, position, depth, out)?;
            }
            CmdOrDef::Def { body } => {
                // A definition, not a call: the body is not executed here.
                // Its commands are still resolved -- a later call to the
                // function would run them -- so route sees them, marked as
                // reached through a function body.
                descend(body, depth, Position::FunctionBody, out)?;
            }
        }
    }
    // FR-CMD-008: a rewrite replaces the WHOLE command string, so it is
    // lossless only when the invocation it targets IS the whole command.
    // Computed once, at the true top level (never inside a substitution,
    // wrapper, or other recursion -- `inherited` is `None` only there):
    // no `SepKind::Other` operator anywhere (a background `&`, a pipe,
    // `&&`/`||`, `;;`, or a `(...)` subshell -- checked over the whole
    // token stream via `saw_other_operator`, not just the group count,
    // since a *trailing* one leaves no second group for that count to
    // catch), exactly one top-level group, with no redirect and no
    // heredoc on it (a redirect target is dropped rather than grouped, so
    // it needs its own check), and the whole scan -- including whatever
    // the group's words recursed into -- resolved to exactly one
    // invocation, at `Position::First` (not `AfterAssignment`: an
    // `env_var=1 rewritable` prefix would also be dropped by a whole-string
    // replacement, and is not visible to the invocation's own `args`),
    // with nothing left opaque.
    if depth == 0 && inherited.is_none() {
        out.is_single_simple_command = !saw_other_operator
            && matches!(
                grouped.as_slice(),
                [CmdOrDef::Cmd(cmd)] if !cmd.has_redirect && cmd.heredocs.is_empty()
            )
            && out.opaque.is_empty()
            && matches!(out.invocations.as_slice(), [inv] if inv.position == Position::First);
    }
    Ok(())
}

/// The next depth level, or `None` with a [`Opaque::TooDeep`] already
/// recorded when nesting one more level would exceed [`MAX_DEPTH`]. Shared
/// by every recursion point (substitutions, inline shells, heredoc shells,
/// function bodies, `find -exec`) so the gate is enforced identically
/// everywhere.
fn checked_depth(depth: u8, out: &mut Scan) -> Option<u8> {
    if depth + 1 > MAX_DEPTH {
        out.opaque.push(Opaque::TooDeep);
        None
    } else {
        Some(depth + 1)
    }
}

/// Recurse one level into `src`, or record the region as too deep to see.
fn descend(src: &str, depth: u8, position: Position, out: &mut Scan) -> Result<(), ScanError> {
    match checked_depth(depth, out) {
        Some(next_depth) => walk(src, next_depth, Some(position), out),
        None => Ok(()),
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
/// reached so far. Shared by [`resolve_from`] (the top of a simple command)
/// and every wrapper/find-exec/docker-exec dispatch that hands off to "the
/// next command in sequence" -- `time if grep -q foo; then ...; fi` needs
/// the same skip after `time` that the top level needs after `;`.
fn skip_prefix(words: &[Word], mut i: usize, mut position: Position) -> (usize, Position) {
    loop {
        let Some(w) = words.get(i) else {
            return (i, position);
        };
        let t = w.text.as_str();
        if is_assignment(t) && !w.quoted {
            position = position.max(Position::AfterAssignment);
            i += 1;
            continue;
        }
        if !w.quoted && PREFIX_KEYWORDS.contains(&t) {
            position = position.max(Position::AfterOperator);
            i += 1;
            continue;
        }
        break;
    }
    (i, position)
}

/// Skips `-flag`/`--flag` options starting at `words[from]`, consuming a
/// separate value word for each name in `value_opts`, and stopping at `--`
/// (consumed), a non-flag word, or the end of `words`. Returns the index of
/// the first word that is not part of the option run.
fn skip_options(words: &[Word], from: usize, value_opts: &[&str]) -> usize {
    let mut j = from;
    while let Some(word) = words.get(j) {
        let t = word.text.as_str();
        if t == "--" {
            j += 1;
            break;
        } else if t.starts_with('-') && t.len() > 1 {
            j += if value_opts.contains(&t) { 2 } else { 1 };
        } else {
            break;
        }
    }
    j
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

fn resolve_at(
    words: &[Word],
    heredocs: &[String],
    i: usize,
    position: Position,
    depth: u8,
    out: &mut Scan,
) -> Result<(), ScanError> {
    let Some(w) = words.get(i) else { return Ok(()) };
    if w.live_expansion {
        // The command-name word carries a live `$` expansion or backtick
        // substitution somewhere in it -- a bare `$VAR`/`$@`/`$*`,
        // `${...}`, `$(...)`, `$((...))`, or a backtick -- built or
        // reshaped at runtime, not resolvable without executing it. This
        // is precise about *where*: `gh${IFS}issue${IFS}list` (the
        // shell's own word-splitting after expansion turns one token into
        // the governed `gh issue list`) and `'lit'$(cmd)` (only part of
        // the word is quoted) both set it, while a fully single-quoted
        // `'$literal'` never does -- single quotes suppress expansion
        // entirely, so that `$` is inert text, not a reference. Double
        // quotes do not suppress expansion (only word-splitting), so
        // `"$@"` and `"${IFS}"` set it just as their unquoted spellings
        // do; see `Word::live_expansion`'s doc for how each case is
        // tracked at the point it is read.
        out.opaque.push(Opaque::DynamicCommand);
        return Ok(());
    }
    let raw = w.text.as_str();
    let bin = basename(raw);
    let rest = &words[i + 1..];
    if bin.is_empty() {
        // `""`, `dir/`, ...: no static, non-empty word names the command.
        out.opaque.push(Opaque::DynamicCommand);
        return Ok(());
    }
    if SCRIPT_EXTENSIONS.iter().any(|e| {
        // Byte-slice, not str-slice: `bin` may contain multibyte
        // characters before the extension, and a byte offset that does
        // not land on a UTF-8 char boundary would panic a `&str` slice.
        // Raw bytes have no such requirement.
        bin.len() >= e.len()
            && bin.as_bytes()[bin.len() - e.len()..].eq_ignore_ascii_case(e.as_bytes())
    }) {
        out.opaque.push(Opaque::ScriptFile);
        return Ok(());
    }

    record(out, bin, words_to_args(rest), position, depth);

    if bin == "source" || bin == "." {
        out.opaque.push(Opaque::Sourced);
        return Ok(());
    }
    if LOOKUPS.contains(&bin) {
        return Ok(());
    }
    if bin == "eval" {
        out.opaque.push(Opaque::Eval);
        return Ok(());
    }
    if bin == "git" {
        if let Some(alias_value) = git_c_alias_value(rest)
            && alias_value.trim_start().starts_with('!')
        {
            out.opaque.push(Opaque::Alias);
        }
        return Ok(());
    }
    let two_word_rows: Vec<TwoWordWrapper> = TWO_WORD_WRAPPERS
        .iter()
        .copied()
        .filter(|(first, ..)| *first == bin)
        .collect();
    if !two_word_rows.is_empty() {
        for (_, second, global_opts, sub_opts, positionals) in two_word_rows.iter().copied() {
            let after_globals = skip_options(words, i + 1, global_opts);
            if words.get(after_globals).map(|w| w.text.as_str()) == Some(second) {
                let mut j = skip_options(words, after_globals + 1, sub_opts);
                j += positionals;
                if j < words.len() {
                    return resolve_from(
                        words,
                        heredocs,
                        j,
                        position.max(Position::Wrapper),
                        depth,
                        out,
                    );
                }
                return Ok(());
            }
        }
        // No row's second word matched at its expected spot. A bare
        // `pnpm grep` (no `exec`/`dlx` anywhere) stays an ordinary
        // invocation, but if the runner's own second word still appears
        // later in the command -- an option this table does not yet model
        // consumed the wrong number of words -- fail closed rather than
        // silently treating it as a plain invocation: this is a wrapper we
        // recognize but could not correctly unwrap.
        let second_word_appears_later = rest.iter().any(|w| {
            two_word_rows
                .iter()
                .any(|(_, second, ..)| w.text == *second)
        });
        if second_word_appears_later {
            out.opaque.push(Opaque::UnknownWrapper);
        }
        return Ok(());
    }
    if UNKNOWN_WRAPPERS.contains(&bin) {
        out.opaque.push(Opaque::UnknownWrapper);
        return Ok(());
    }
    if let Some((_, value_opts, positionals)) = WRAPPERS.iter().find(|(name, _, _)| *name == bin) {
        if bin == "command"
            && rest
                .first()
                .is_some_and(|w| matches!(w.text.as_str(), "-v" | "-V"))
        {
            return Ok(()); // a lookup, not an invocation of the target
        }
        if bin == "env" {
            return resolve_env(words, heredocs, i, value_opts, position, depth, out);
        }
        let mut j = skip_options(words, i + 1, value_opts);
        j += positionals;
        if j < words.len() {
            return resolve_from(
                words,
                heredocs,
                j,
                position.max(Position::Wrapper),
                depth,
                out,
            );
        }
        return Ok(());
    }
    if SHELLS.contains(&bin) {
        return resolve_shell(words, heredocs, i, position, depth, out);
    }
    if bin == "awk" {
        resolve_awk(rest, out);
        return Ok(());
    }
    if INTERPRETERS.contains(&bin) || bin.starts_with("python3.") {
        resolve_interpreter(rest, bin, out);
        return Ok(());
    }
    if bin == "find" {
        let mut j = i + 1;
        while j < words.len() {
            let t = words[j].text.as_str();
            if !words[j].quoted && matches!(t, "-exec" | "-execdir" | "-ok" | "-okdir") {
                let start = j + 1;
                let end = (start..words.len())
                    .find(|&m| matches!(words[m].text.as_str(), ";" | "+"))
                    .unwrap_or(words.len());
                if start < end
                    && let Some(next_depth) = checked_depth(depth, out)
                {
                    resolve_from(
                        &words[..end],
                        &[],
                        start,
                        position.max(Position::FindExec),
                        next_depth,
                        out,
                    )?;
                }
                j = end;
            }
            j += 1;
        }
    }
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

/// `env`'s own leading options: `NAME=value` assignments and flags are
/// walked one at a time from `words[i + 1]`, so a `-S` that belongs to the
/// wrapped command (`env sort -S 1G file`) is never mistaken for env's own
/// `-S` -- unlike scanning `rest` for the first `-S` anywhere in it, which
/// would. Stops at the first non-option word (the wrapped command) or, on
/// finding env's own `-S`, dispatches to [`resolve_env_dash_s`].
fn resolve_env(
    words: &[Word],
    heredocs: &[String],
    i: usize,
    value_opts: &[&str],
    position: Position,
    depth: u8,
    out: &mut Scan,
) -> Result<(), ScanError> {
    let mut j = i + 1;
    while let Some(word) = words.get(j) {
        let t = word.text.as_str();
        if is_assignment(t) {
            j += 1;
        } else if t == "-S" {
            if let Some(payload) = words.get(j + 1) {
                resolve_env_dash_s(&payload.text, position, depth, out);
            }
            return Ok(());
        } else if t == "--" {
            j += 1;
            break;
        } else if t.starts_with('-') && t.len() > 1 {
            j += if value_opts.contains(&t) { 2 } else { 1 };
        } else {
            break;
        }
    }
    if j < words.len() {
        return resolve_from(
            words,
            heredocs,
            j,
            position.max(Position::Wrapper),
            depth,
            out,
        );
    }
    Ok(())
}

/// `env -S '<command line>' ...`: the payload is a single string built at
/// the shell layer and re-split by `env` at runtime. A bounded scanner
/// approximates that split with a plain whitespace split -- good enough to
/// find the head, not a claim of exact `env -S` quoting semantics.
fn resolve_env_dash_s(payload: &str, position: Position, depth: u8, out: &mut Scan) {
    let mut parts = payload.split_whitespace();
    let Some(head) = parts.next() else { return };
    let args: Vec<String> = parts.map(String::from).collect();
    record(
        out,
        basename(head),
        args,
        position.max(Position::Wrapper),
        depth,
    );
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
                position.max(Position::InlineShell),
                out,
            );
        }
        return Ok(());
    }
    if words.get(j).is_some() {
        out.opaque.push(Opaque::ScriptFile);
    } else if let Some(body) = heredocs.first() {
        descend(body, depth, position.max(Position::HeredocShell), out)?;
    } else {
        out.opaque.push(Opaque::StdinScript);
    }
    Ok(())
}

fn resolve_interpreter(rest: &[Word], bin: &str, out: &mut Scan) {
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
            return;
        }
        if t.starts_with('-') && t.len() > 1 {
            j += 1;
            continue;
        }
        if t == "-" {
            break;
        }
        out.opaque.push(Opaque::ScriptFile);
        return;
    }
    out.opaque.push(Opaque::StdinScript);
}

fn resolve_awk(rest: &[Word], out: &mut Scan) {
    let mut j = 0;
    while j < rest.len() {
        let t = rest[j].text.as_str();
        if t == "-f" {
            out.opaque.push(Opaque::ScriptFile);
            return;
        }
        if t.starts_with('-') && t.len() > 1 {
            j += 1;
            continue;
        }
        out.opaque.push(Opaque::Interpreter {
            interpreter: "awk".to_string(),
            body: t.to_string(),
        });
        return;
    }
    out.opaque.push(Opaque::StdinScript);
}
