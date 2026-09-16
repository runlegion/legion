//! Grouping the lexer's token stream into simple commands, and resolving
//! each command position to an [`Invocation`] or an [`Opaque`] region.

use super::lexer::{Scanner, Tok, Word};
use super::tables::{
    CLAUSE_KEYWORDS, DOCKER_EXEC_VALUE_OPTS, END_KEYWORDS, INTERPRETERS, LOOKUPS, PREFIX_KEYWORDS,
    SCRIPT_EXTENSIONS, SHELLS, UNKNOWN_WRAPPERS, WRAPPERS,
};
use super::{Invocation, MAX_DEPTH, Opaque, Position, Scan, ScanError};

#[derive(Default)]
struct Cmd {
    words: Vec<Word>,
    heredocs: Vec<String>,
    after_sep: bool,
}

enum CmdOrDef {
    Cmd(Cmd),
    Def { body: String },
}

fn group(toks: Vec<Tok>, out: &mut Vec<CmdOrDef>) {
    let mut cur = Cmd::default();
    let mut redirect_pending = false;
    for t in toks {
        match t {
            Tok::Sep => {
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
                descend(&body, depth, Position::FunctionBody, out)?;
            }
        }
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
/// (consumed), a non-flag word, or the end of `words`. When
/// `skip_assignment`, an `env`-style `NAME=value` word is also skipped
/// (only `env` itself takes bare assignments ahead of its wrapped command).
/// Returns the index of the first word that is not part of the option run.
fn skip_options(words: &[Word], from: usize, value_opts: &[&str], skip_assignment: bool) -> usize {
    let mut j = from;
    while let Some(word) = words.get(j) {
        let t = word.text.as_str();
        if skip_assignment && is_assignment(t) {
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
    if w.whole_subst || (w.text.starts_with('$') && w.text.len() > 1) {
        // A bare `$VAR`, `$@`, `$*`, `${...}`, or a whole-word substitution
        // as the command name: built at runtime, not resolvable without
        // executing it. This holds even when the word was written `"$@"`
        // (quoted for correct word-splitting) -- quoting changes how the
        // shell splits the result, not whether the head is known ahead of
        // execution.
        out.opaque.push(Opaque::DynamicCommand);
        return Ok(());
    }
    let raw = w.text.as_str();
    let bin = basename(raw);
    let rest = &words[i + 1..];
    if bin.is_empty() {
        return Ok(());
    }
    if SCRIPT_EXTENSIONS.iter().any(|e| bin.ends_with(e)) {
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
    if bin == "docker" && rest.first().map(|w| w.text.as_str()) == Some("exec") {
        let mut j = skip_options(words, i + 2, DOCKER_EXEC_VALUE_OPTS, false);
        j += 1; // the container name
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
        if bin == "env"
            && let Some(k) = rest.iter().position(|w| w.text == "-S")
            && let Some(payload) = rest.get(k + 1)
        {
            resolve_env_dash_s(&payload.text, position, depth, out);
            return Ok(());
        }
        let mut j = skip_options(words, i + 1, value_opts, bin == "env");
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
