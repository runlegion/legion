//! legion-cmd's command splitter (#1226, FR-CMD-007).
//!
//! `scan` parses a Bash command string with `brush-parser` and walks the
//! resulting tree for every command position the grammar carries: first
//! position, after a pipe or list operator, behind an environment prefix,
//! inside a function body, and inside a command or process substitution. A
//! region the walk cannot reduce to a command is reported as [`Unreduced`]
//! with its raw text and one [`UnreducedReason`].
//!
//! The splitter holds no binary, wrapper, interpreter or shell name, and no
//! table of them. A payload reachable only by knowing what a name does --
//! `sh -c '...'`, `xargs grep`, `find -exec grep`, `env -S '...'` -- is left
//! as an ordinary argument string of the outer command; the splitter never
//! decides that a name it does not recognize wraps, aliases or interprets
//! another command. Resolving those payloads is the router's job once the
//! policy names the wrapper (#1227): it re-enters the payload text through
//! [`scan_at`], carrying depth across the re-entry.
//!
//! # Position, resolved
//!
//! [`Position`] is a closed, five-member set, so a command sitting inside an
//! `if`/`while`/`until`/`case` condition or body, a subshell, or a brace
//! group -- none of which the set names -- is resolved by one mechanical
//! rule: every such body is walked the same way the top-level program is
//! walked. The first command found while walking it is [`Position::First`]
//! relative to that body (the same relativity FR-CMD-007 states explicitly
//! for a wrapper payload the router re-enters), and every command that
//! follows an operator (`|`, `&&`, `||`, `;`, `&`, or a newline) within it is
//! [`Position::AfterOperator`]. [`Position::Substitution`] and
//! [`Position::FunctionBody`] are not sequence positions but location tags:
//! once the walk is inside a command/process substitution or a function
//! body, every command found there carries that tag regardless of its
//! sequence position, and a substitution encountered while already inside a
//! function body is tagged `Substitution` (the more immediate context wins).
//! Symmetrically, a function body encountered while already inside a
//! substitution is tagged `FunctionBody`: whichever of the two encloses a
//! command more tightly is the tag that command carries.
//! [`Position::AfterAssignment`] outranks every other tag: a command whose
//! `SimpleCommand` prefix carries an environment assignment reports
//! `AfterAssignment` whatever else is true of where it sits (FR-CMD-007).
//!
//! `time` is bash grammar, not a wrapper name: `brush-parser` models it as
//! [`brush_parser::ast::Pipeline::timed`], so `time rg -n foo` resolves `rg`
//! at `First` without the splitter ever seeing the word `time`.
//!
//! A glob-like or otherwise unrecognized command word (`gr?p`, `=grep`) is
//! resolved exactly as written: the splitter has no table to consult, so it
//! reports the literal text as `binary` and leaves matching it to the
//! policy.

use std::io::Cursor;

use brush_parser::ast::{
    self, Command, CommandPrefixOrSuffixItem, CompoundCommand, CompoundList, IoFileRedirectTarget,
    IoRedirect, Pipeline, RedirectList, SimpleCommand, SourceLocation as _, SubshellCommand,
};
use brush_parser::word::{self, WordPiece};
use brush_parser::{Parser, ParserOptions};

/// The depth bound (FR-CMD-007). A region past this bound is reported as
/// [`Unreduced`] with reason [`UnreducedReason::TooDeep`] rather than walked
/// further.
pub const MAX_DEPTH: u8 = 25;

/// Where in the command's grammar a command position sits (FR-CMD-007).
///
/// Every variant is a grammar fact. No variant names a binary, a wrapper, an
/// interpreter or a shell: those live in the policy data (FR-CMD-011). A
/// command found inside a wrapper payload the router re-entered is `First`
/// relative to that payload; the router composes the full path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Position {
    /// The first command of the text being split (or of the body being
    /// walked -- see the module docs for how this resolves inside
    /// `if`/`while`/`case`/subshell bodies, which the grammar hands the
    /// splitter but which FR-CMD-007's five-member set does not itself
    /// name).
    First,
    /// After `|`, `&&`, `||`, `;`, `&`, or a newline.
    AfterOperator,
    /// Behind an environment prefix, e.g. `FOO=1 grep`.
    AfterAssignment,
    /// Inside a command or process substitution: `$(...)`, backticks,
    /// `<(...)`, `>(...)`.
    Substitution,
    /// Inside a function body.
    FunctionBody,
}

/// A resolved command invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// Basename of the command word, quotes and a leading backslash removed.
    pub binary: String,
    /// The remaining words, as the grammar produced them.
    pub args: Vec<String>,
    pub position: Position,
    /// 0 at the top of the text being split; never above [`MAX_DEPTH`].
    pub depth: u8,
}

/// Why a region was not reduced to a command (FR-CMD-007). Closed set.
///
/// The splitter produces only the reasons it can see from grammar alone:
/// [`Self::InterpreterBody`], [`Self::DynamicName`], [`Self::TooDeep`],
/// [`Self::Unparsed`]. [`Self::WrapperPayload`] and [`Self::ScriptFile`] need
/// a name lookup, so the router records those (#1227); they live in this
/// enum because the enum is the shared vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnreducedReason {
    /// A payload carried by a name the policy does not say wraps a command.
    WrapperPayload,
    /// A script file named rather than inlined.
    ScriptFile,
    /// A heredoc body or an interpreter body.
    InterpreterBody,
    /// A command name the shell builds at runtime.
    DynamicName,
    /// A region past the depth bound.
    TooDeep,
    /// A region the parser could not parse.
    Unparsed,
}

/// A region the splitter could not reduce to a command (FR-CMD-007).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unreduced {
    /// The region's raw text, copied from the command string by byte offset.
    pub text: String,
    /// Exactly one reason.
    pub reason: UnreducedReason,
    pub depth: u8,
}

/// The result of splitting a command string.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Scan {
    pub invocations: Vec<Invocation>,
    pub unreduced: Vec<Unreduced>,
    /// True only when the parse is structurally exactly one simple command
    /// with a command word and no substitution anywhere in it. See
    /// [`is_one_simple_command`].
    pub single_simple: bool,
}

/// Errors raised when `scan`'s own top-level parse of the given command
/// fails. A nested region that fails to parse while the outer parse
/// succeeds is reported as an [`Unreduced`] with reason
/// [`UnreducedReason::Unparsed`] instead of an error (FR-CMD-007); this type
/// is only ever raised for the text `scan`/`scan_at` was directly given.
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    /// The tokenizer or parser found a syntax error at this byte offset into
    /// the given command string.
    #[error("syntax error at byte {offset}")]
    Syntax {
        /// Byte offset into the given command string.
        offset: usize,
    },
    /// The parser reached the end of the input with no more specific
    /// construct identified.
    #[error("unexpected end of input at byte {offset}")]
    UnexpectedEnd {
        /// Byte offset into the given command string (its length).
        offset: usize,
    },
    /// A single, double, or ANSI-C quote opened at this byte offset was
    /// never closed.
    #[error("unterminated quote opened at byte {offset}")]
    UnterminatedQuote {
        /// Byte offset of the quote character that opened the construct.
        offset: usize,
    },
    /// A backquoted command substitution opened at this byte offset was
    /// never closed.
    #[error("unterminated backquote opened at byte {offset}")]
    UnterminatedBackquote {
        /// Byte offset of the backquote that opened the construct.
        offset: usize,
    },
    /// An extended glob pattern opened at this byte offset was never
    /// closed.
    #[error("unterminated extended glob opened at byte {offset}")]
    UnterminatedExtendedGlob {
        /// Byte offset of the construct that opened the pattern.
        offset: usize,
    },
    /// A command, arithmetic, or parameter expansion was never closed. The
    /// tokenizer does not tag this construct with its own opening position,
    /// so `offset` is the end of the given command string.
    #[error("unterminated expansion at byte {offset}")]
    UnterminatedExpansion {
        /// Byte offset into the given command string, when known.
        offset: usize,
    },
    /// A here document was opened but never terminated. The tokenizer does
    /// not tag this construct with its own opening position, so `offset` is
    /// the end of the given command string.
    #[error("unterminated here document at byte {offset}")]
    UnterminatedHereDocument {
        /// Byte offset into the given command string, when known.
        offset: usize,
    },
    /// The tokenizer failed on the given command string for a reason not
    /// named by another variant.
    #[error("failed to tokenize at byte {offset}: {detail}")]
    Tokenize {
        /// Byte offset into the given command string, when known.
        offset: usize,
        /// The tokenizer's own error text.
        detail: String,
    },
}

/// Splits `command` at depth 0.
pub fn scan(command: &str) -> Result<Scan, ScanError> {
    scan_at(command, 0)
}

/// Splits `command` starting at `depth`, for a caller re-entering a payload
/// (the router, across a wrapper it recognizes; the splitter itself, across
/// a command or process substitution).
pub fn scan_at(command: &str, depth: u8) -> Result<Scan, ScanError> {
    scan_tagged(command, depth, None)
}

/// The shared parse-and-walk entry `scan_at` and `rescan_substitution` both
/// call, so the tag-precedence rule (see the module docs) is written once.
/// `tag` is the location forced by an enclosing command/process substitution
/// or function body; `scan_at` enters with no tag, and `rescan_substitution`
/// enters with `Some(Position::Substitution)` -- from there `walk_command`'s
/// ordinary precedence (an assignment prefix outranks the tag, a function
/// body overrides it) applies exactly as it does for an already-parsed
/// process substitution walked via [`walk_process_substitution`].
fn scan_tagged(command: &str, depth: u8, tag: Tag) -> Result<Scan, ScanError> {
    if depth >= MAX_DEPTH
        || exceeds_depth_guard(command, depth)
        || exceeds_keyword_depth_guard(command, depth)
    {
        return Ok(too_deep(command, depth));
    }

    let options = ParserOptions::default();
    let mut parser = Parser::new(Cursor::new(command.as_bytes()), &options);
    let program = parser
        .parse_program()
        .map_err(|err| to_scan_error(&err, command))?;

    let mut scan = Scan::default();
    // `brush-parser` hands back a `Program` as several `complete_commands`
    // rather than one list, but the newline between two top-level
    // statements is exactly the kind of separator `AfterOperator` names --
    // so only the very first item of the very first `complete_command` is
    // `First`; every item after it, whether it continues that
    // `complete_command` or starts the next one, is `AfterOperator`.
    let mut seen_first = false;
    for complete_command in &program.complete_commands {
        for item in &complete_command.0 {
            walk_and_or_list(
                command,
                &item.0,
                depth,
                tag,
                !seen_first,
                Position::First,
                &mut scan,
            );
            seen_first = true;
        }
    }
    scan.single_simple = is_one_simple_command(&program, depth);
    Ok(scan)
}

/// True when `program` is exactly one simple command that names a command
/// word: one complete command holding one list item, run synchronously, with
/// no `&&`/`||`, one pipeline stage, no `!` negation, and no `time`, and whose
/// every word and redirect is plain (see [`is_plain_item`]). A pipe, a list
/// operator, `&`, a subshell, a brace group, `[[ ]]`, `(( ))`, a function
/// definition, a line with no command word (`X=1`, `> out.txt`), and any
/// command or process substitution anywhere in the command all fail it. This
/// is the shape a rewrite can replace without dropping anything else the line
/// runs (FR-CMD-008). It is decided from the parse tree alone, never from
/// what the walk happened to record.
fn is_one_simple_command(program: &ast::Program, depth: u8) -> bool {
    let [list] = program.complete_commands.as_slice() else {
        return false;
    };
    let [ast::CompoundListItem(and_or, separator)] = list.0.as_slice() else {
        return false;
    };
    if !matches!(separator, ast::SeparatorOperator::Sequence) || !and_or.additional.is_empty() {
        return false;
    }
    let pipeline = &and_or.first;
    if pipeline.bang || pipeline.timed.is_some() {
        return false;
    }
    let [Command::Simple(simple)] = pipeline.seq.as_slice() else {
        return false;
    };
    let Some(command_word) = &simple.word_or_name else {
        return false;
    };
    let items = simple
        .prefix
        .iter()
        .flat_map(|prefix| &prefix.0)
        .chain(simple.suffix.iter().flat_map(|suffix| &suffix.0));
    is_plain_word(command_word, depth) && items.into_iter().all(|item| is_plain_item(item, depth))
}

/// An allow-list over one prefix or suffix item: a word or assignment whose
/// word is plain, or a file, fd-duplicate, here-string or `&>` redirect whose
/// target word is plain. A process substitution, whether an argument or a
/// redirect target, and a here-document (whose body may expand) are not.
fn is_plain_item(item: &CommandPrefixOrSuffixItem, depth: u8) -> bool {
    match item {
        CommandPrefixOrSuffixItem::Word(word)
        | CommandPrefixOrSuffixItem::AssignmentWord(_, word) => is_plain_word(word, depth),
        CommandPrefixOrSuffixItem::ProcessSubstitution(..) => false,
        CommandPrefixOrSuffixItem::IoRedirect(redirect) => match redirect {
            IoRedirect::File(_, _, target) => match target {
                IoFileRedirectTarget::Filename(word) | IoFileRedirectTarget::Duplicate(word) => {
                    is_plain_word(word, depth)
                }
                IoFileRedirectTarget::Fd(_) => true,
                IoFileRedirectTarget::ProcessSubstitution(..) => false,
            },
            IoRedirect::HereString(_, word) | IoRedirect::OutputAndError(word, _) => {
                is_plain_word(word, depth)
            }
            IoRedirect::HereDocument(..) => false,
        },
    }
}

/// An allow-list over a word's pieces: literal text, quoting, escapes, a tilde,
/// and a bare parameter reference (`$X`, `${X}`, `$1`, `$@`). A command
/// substitution (`$(...)` or backquotes), an arithmetic expansion, and every
/// parameter expansion that carries embedded shell text (a default value, a
/// pattern, an array subscript) or is indirect (`${!x}`) are not, since each
/// can run a command.
fn is_plain_word(word: &ast::Word, depth: u8) -> bool {
    parse_word_pieces(&word.value, depth)
        .is_ok_and(|pieces| pieces.iter().all(|piece| is_plain_piece(&piece.piece)))
}

fn is_plain_piece(piece: &WordPiece) -> bool {
    match piece {
        WordPiece::Text(_)
        | WordPiece::SingleQuotedText(_)
        | WordPiece::AnsiCQuotedText(_)
        | WordPiece::EscapeSequence(_)
        | WordPiece::TildeExpansion(_) => true,
        WordPiece::DoubleQuotedSequence(pieces)
        | WordPiece::GettextDoubleQuotedSequence(pieces) => {
            pieces.iter().all(|p| is_plain_piece(&p.piece))
        }
        // An indirect reference (`${!x}`) dereferences the runtime value as
        // parameter syntax, whose subscript can run a command substitution, so
        // it is never plain -- the same as a literal subscript.
        WordPiece::ParameterExpansion(word::ParameterExpr::Parameter {
            parameter,
            indirect,
        }) => !indirect && !matches!(parameter, word::Parameter::NamedWithIndex { .. }),
        WordPiece::ParameterExpansion(_)
        | WordPiece::CommandSubstitution(_)
        | WordPiece::BackquotedCommandSubstitution(_)
        | WordPiece::ArithmeticExpression(_) => false,
    }
}

fn too_deep(text: &str, depth: u8) -> Scan {
    Scan {
        invocations: Vec::new(),
        unreduced: vec![too_deep_unreduced(text, depth)],
        single_simple: false,
    }
}

fn too_deep_unreduced(text: &str, depth: u8) -> Unreduced {
    Unreduced {
        text: text.to_string(),
        reason: UnreducedReason::TooDeep,
        depth,
    }
}

/// Counts nesting over `text`'s raw bytes ahead of any parse call
/// (FR-CMD-007): the crate's own PEG recursion can abort the process on
/// deeply nested input even inside a single `parse_program` call, which a
/// guard that runs after parsing would never reach. A combined opener set --
/// parens, braces, brackets, and paired backticks -- covers subshells,
/// command and arithmetic substitution, process substitution, and brace
/// groups alike. Over-counting is safe (an over-cautious `TooDeep`);
/// under-counting is not.
///
/// A byte inside a single-quoted span, or immediately after an unquoted
/// backslash, contributes no nesting: bash gives single-quoted text no
/// expansion at all, so the crate's tokenizer scans it as a flat run with no
/// recursive descent per bracket, and a backslash-escaped bracket is a
/// literal character, not an opener the grammar recurses through either.
/// Skipping both keeps an ordinary command with many independent quoted or
/// escaped brackets -- `grep '[[[[' f`, `rg \( f0 && rg \( f1 && ...` -- from
/// climbing an unmatched-opener count that never has a matching closer to
/// bring it back down, which is a false `TooDeep` rather than the crate's
/// own recursion depth. Every other byte is still counted without regard to
/// matching bracket type or double-quote nesting, which stays deliberately
/// conservative: over-counting there is safe.
fn exceeds_depth_guard(text: &str, depth: u8) -> bool {
    let budget = MAX_DEPTH.saturating_sub(depth);
    let mut level: u32 = 0;
    let mut max_level: u32 = 0;
    let mut in_backtick = false;
    let mut in_single_quote = false;
    let mut escaped = false;
    for byte in text.bytes() {
        if escaped {
            escaped = false;
            continue;
        }
        if in_single_quote {
            if byte == b'\'' {
                in_single_quote = false;
            }
            continue;
        }
        match byte {
            b'\\' => escaped = true,
            b'\'' => in_single_quote = true,
            b'(' | b'{' | b'[' => {
                level += 1;
                max_level = max_level.max(level);
            }
            b')' | b'}' | b']' => {
                level = level.saturating_sub(1);
            }
            b'`' => {
                if in_backtick {
                    level = level.saturating_sub(1);
                } else {
                    level += 1;
                    max_level = max_level.max(level);
                }
                in_backtick = !in_backtick;
            }
            _ => {}
        }
    }
    u32::from(budget) < max_level
}

/// Reserved words whose grammar nests one compound command inside another:
/// `if`/`fi`, `for`/`done`, `while`/`done`, `until`/`done`, `case`/`esac`.
/// `brush-parser`'s PEG grammar recurses one level deeper per opener here
/// exactly as it does per bracket, so deeply nested reserved words can abort
/// the process the same way deeply nested brackets can, even at bracket
/// nesting depth 0 -- `exceeds_depth_guard` cannot see this class because
/// none of these words is a bracket. FR-CMD-007's Behavior section names
/// bracket nesting as *a* pre-parse guard, not the only one a stack-overflow
/// risk may need; this closes the gap the bracket guard leaves on reserved
/// words without adding any binary, wrapper, interpreter, or shell name --
/// these five words are bash grammar, the same status `time` already has in
/// this module's docs.
///
/// A reserved word is recognized only as a standalone token bounded by
/// whitespace or a shell metacharacter, so an identifier like `for_each` or
/// a basename like `ifconfig` is never split into a false match. A token
/// inside a single- or double-quoted span is skipped, the same as in
/// `exceeds_depth_guard`'s single-quote handling, so a quoted argument that
/// merely contains the letters cannot trip it -- a shell never treats a word
/// inside either quote kind as its own token, so `git commit -m "fix for
/// f"` carries no more reserved-word nesting than the same message
/// single-quoted would. A token elsewhere that happens to match is counted
/// regardless of context, which stays conservative in the same
/// over-counting-is-safe direction as the bracket guard.
fn exceeds_keyword_depth_guard(text: &str, depth: u8) -> bool {
    let budget = MAX_DEPTH.saturating_sub(depth);
    let mut level: u32 = 0;
    let mut max_level: u32 = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut word_start: Option<usize> = None;
    let bytes = text.as_bytes();

    for (index, &byte) in bytes.iter().enumerate() {
        if in_single_quote {
            if byte == b'\'' {
                in_single_quote = false;
            }
            continue;
        }
        if in_double_quote {
            if byte == b'"' {
                in_double_quote = false;
            }
            continue;
        }
        if is_shell_word_boundary(byte) {
            if let Some(start) = word_start.take() {
                apply_keyword_nesting(&text[start..index], &mut level, &mut max_level);
            }
            if byte == b'\'' {
                in_single_quote = true;
            } else if byte == b'"' {
                in_double_quote = true;
            }
        } else if word_start.is_none() {
            word_start = Some(index);
        }
    }
    if let Some(start) = word_start {
        apply_keyword_nesting(&text[start..], &mut level, &mut max_level);
    }
    u32::from(budget) < max_level
}

/// Whitespace and the shell metacharacters that end a word without being
/// part of one, used to tokenize reserved words for
/// `exceeds_keyword_depth_guard`.
fn is_shell_word_boundary(byte: u8) -> bool {
    matches!(
        byte,
        b' ' | b'\t'
            | b'\n'
            | b'\r'
            | b';'
            | b'&'
            | b'|'
            | b'('
            | b')'
            | b'<'
            | b'>'
            | b'`'
            | b'\''
            | b'"'
    )
}

fn apply_keyword_nesting(word: &str, level: &mut u32, max_level: &mut u32) {
    match word {
        "if" | "for" | "while" | "until" | "case" => {
            *level += 1;
            *max_level = (*max_level).max(*level);
        }
        "fi" | "done" | "esac" => {
            *level = level.saturating_sub(1);
        }
        _ => {}
    }
}

fn to_scan_error(err: &brush_parser::ParseError, command: &str) -> ScanError {
    match err {
        brush_parser::ParseError::ParsingNear(pos) => ScanError::Syntax { offset: pos.offset },
        brush_parser::ParseError::ParsingAtEndOfInput => ScanError::UnexpectedEnd {
            offset: command.len(),
        },
        brush_parser::ParseError::Tokenizing { inner, position } => {
            to_tokenize_scan_error(inner, position, command)
        }
    }
}

/// Names the construct a tokenizer failure points to (Error Handling),
/// rather than only the kind of failure. Several `TokenizerError` variants
/// carry the `SourcePosition` where the construct itself opened --
/// `UnterminatedSingleQuote`, `UnterminatedDoubleQuote`, and
/// `UnterminatedAnsiCQuote` all record the position of the opening quote,
/// `UnterminatedBackquote` the opening backquote, `UnterminatedExtendedGlob`
/// the opening pattern -- and this uses that position rather than the
/// `ParseError::Tokenizing` position, which is the tokenizer's cursor at the
/// point it gave up (typically the end of input). The remaining variants
/// carry no position of their own in the crate, so those fall back to the
/// outer position: fixing that would mean tracking a construct's opening
/// offset ourselves, which is exactly the offset work the dependency pin
/// exists to avoid taking on locally.
fn to_tokenize_scan_error(
    inner: &brush_parser::TokenizerError,
    position: &Option<brush_parser::SourcePosition>,
    command: &str,
) -> ScanError {
    let fallback = position.as_ref().map_or(command.len(), |p| p.offset);
    match inner {
        brush_parser::TokenizerError::UnterminatedSingleQuote(pos)
        | brush_parser::TokenizerError::UnterminatedDoubleQuote(pos)
        | brush_parser::TokenizerError::UnterminatedAnsiCQuote(pos) => {
            ScanError::UnterminatedQuote { offset: pos.offset }
        }
        brush_parser::TokenizerError::UnterminatedBackquote(pos) => {
            ScanError::UnterminatedBackquote { offset: pos.offset }
        }
        brush_parser::TokenizerError::UnterminatedExtendedGlob(pos) => {
            ScanError::UnterminatedExtendedGlob { offset: pos.offset }
        }
        brush_parser::TokenizerError::UnterminatedCommandSubstitution
        | brush_parser::TokenizerError::UnterminatedExpansion
        | brush_parser::TokenizerError::UnterminatedVariable => {
            ScanError::UnterminatedExpansion { offset: fallback }
        }
        brush_parser::TokenizerError::UnterminatedHereDocuments(_, _)
        | brush_parser::TokenizerError::MissingHereTagForDocumentBody
        | brush_parser::TokenizerError::MissingHereTag(_) => {
            ScanError::UnterminatedHereDocument { offset: fallback }
        }
        _ => ScanError::Tokenize {
            offset: fallback,
            detail: inner.to_string(),
        },
    }
}

/// Copies `text[start..end]`, falling back to the nearest char boundary
/// rather than panicking on a boundary a byte offset does not land on.
fn slice_bytes(text: &str, start: usize, end: usize) -> String {
    let start = start.min(text.len());
    let end = end.min(text.len()).max(start);
    match text.get(start..end) {
        Some(slice) => slice.to_string(),
        None => {
            let mut lo = start;
            while lo > 0 && !text.is_char_boundary(lo) {
                lo -= 1;
            }
            let mut hi = end;
            while hi < text.len() && !text.is_char_boundary(hi) {
                hi += 1;
            }
            text.get(lo..hi).unwrap_or_default().to_string()
        }
    }
}

/// The context a walk carries: the location tag forced by an enclosing
/// substitution or function body, which outranks ordinary sequence
/// position (see the module docs).
type Tag = Option<Position>;

/// `entry` is the `Position` the list's own very first command carries when
/// nothing overrides it: ordinarily `Position::First` (a fresh body reset,
/// per the module's documented relative-First rule), but a `BraceGroup` or
/// `Subshell` walked from `walk_command`'s `Compound` arm passes that arm's
/// own `seq_position` through instead, so a subshell or brace group sitting
/// after a pipe or list operator does not silently report its first command
/// as `First`.
fn walk_list(
    text: &str,
    list: &CompoundList,
    depth: u8,
    tag: Tag,
    entry: Position,
    scan: &mut Scan,
) {
    for (index, item) in list.0.iter().enumerate() {
        let item_is_first = index == 0;
        walk_and_or_list(text, &item.0, depth, tag, item_is_first, entry, scan);
    }
}

fn walk_and_or_list(
    text: &str,
    and_or: &ast::AndOrList,
    depth: u8,
    tag: Tag,
    item_is_first: bool,
    entry: Position,
    scan: &mut Scan,
) {
    for (index, pipeline) in and_or.iter().enumerate() {
        let pipeline_is_first = item_is_first && index == 0;
        walk_pipeline(text, pipeline.1, depth, tag, pipeline_is_first, entry, scan);
    }
}

fn walk_pipeline(
    text: &str,
    pipeline: &Pipeline,
    depth: u8,
    tag: Tag,
    pipeline_is_first: bool,
    entry: Position,
    scan: &mut Scan,
) {
    for (index, command) in pipeline.seq.iter().enumerate() {
        let stage_is_first = pipeline_is_first && index == 0;
        let seq_position = if stage_is_first {
            entry
        } else {
            Position::AfterOperator
        };
        walk_command(text, command, depth, tag, seq_position, scan);
    }
}

fn walk_command(
    text: &str,
    command: &Command,
    depth: u8,
    tag: Tag,
    seq_position: Position,
    scan: &mut Scan,
) {
    match command {
        Command::Simple(simple) => {
            walk_simple_command(text, simple, depth, tag, seq_position, scan)
        }
        Command::Compound(compound, redirects) => {
            walk_compound_command_tagged(text, compound, depth, tag, seq_position, scan);
            if let Some(redirects) = redirects {
                walk_redirect_list(text, redirects, depth, scan);
            }
        }
        Command::Function(function_def) => {
            walk_function_body(text, &function_def.body, depth, scan);
        }
        Command::ExtendedTest(extended_test, redirects) => {
            walk_extended_test_expr(&extended_test.expr, depth, scan);
            if let Some(redirects) = redirects {
                walk_redirect_list(text, redirects, depth, scan);
            }
        }
    }
}

/// Walks a `[[ ... ]]` extended test expression for its own words: the
/// expression carries `Word`s (in its unary and binary tests) like any other
/// command, and the walk word-parses every word so a substitution inside one
/// is not lost.
fn walk_extended_test_expr(expr: &ast::ExtendedTestExpr, depth: u8, scan: &mut Scan) {
    match expr {
        ast::ExtendedTestExpr::And(left, right) | ast::ExtendedTestExpr::Or(left, right) => {
            walk_extended_test_expr(left, depth, scan);
            walk_extended_test_expr(right, depth, scan);
        }
        ast::ExtendedTestExpr::Not(inner) | ast::ExtendedTestExpr::Parenthesized(inner) => {
            walk_extended_test_expr(inner, depth, scan);
        }
        ast::ExtendedTestExpr::UnaryTest(_, word) => walk_word(word, depth, scan),
        ast::ExtendedTestExpr::BinaryTest(_, left, right) => {
            walk_word(left, depth, scan);
            walk_word(right, depth, scan);
        }
    }
}

fn walk_simple_command(
    text: &str,
    simple: &SimpleCommand,
    depth: u8,
    tag: Tag,
    seq_position: Position,
    scan: &mut Scan,
) {
    let mut has_assignment = false;
    if let Some(prefix) = &simple.prefix {
        for item in &prefix.0 {
            if matches!(item, CommandPrefixOrSuffixItem::AssignmentWord(_, _)) {
                has_assignment = true;
            }
            walk_prefix_or_suffix_item(text, item, depth, scan);
        }
    }

    let position = if has_assignment {
        Position::AfterAssignment
    } else {
        tag.unwrap_or(seq_position)
    };

    if let Some(word) = &simple.word_or_name {
        match classify_command_word(word, depth) {
            CommandWord::Unreduced(reason) => {
                scan.unreduced.push(Unreduced {
                    text: word.value.clone(),
                    reason,
                    depth,
                });
            }
            // A dynamic command word (`$(...)`, a backquoted substitution,
            // an expansion used as the command name itself) still carries a
            // nested command position the grammar shows: walk the pieces
            // `classify_command_word` already parsed the same way an
            // ordinary word's substitution is walked, so the region is
            // reported opaque *and* its inner commands are not silently
            // dropped. The pieces are reused rather than re-parsing
            // `word.value` a second time through `walk_word`.
            CommandWord::Dynamic(pieces) => {
                scan.unreduced.push(Unreduced {
                    text: word.value.clone(),
                    reason: UnreducedReason::DynamicName,
                    depth,
                });
                for piece in &pieces {
                    walk_word_piece(&piece.piece, depth, scan);
                }
            }
            CommandWord::Literal(literal) => {
                let binary = to_binary(&literal);
                let args = simple
                    .suffix
                    .iter()
                    .flat_map(|suffix| &suffix.0)
                    .filter_map(|item| suffix_item_arg(text, item))
                    .collect();
                scan.invocations.push(Invocation {
                    binary,
                    args,
                    position,
                    depth,
                });
            }
        }
    }

    if let Some(suffix) = &simple.suffix {
        for item in &suffix.0 {
            walk_prefix_or_suffix_item(text, item, depth, scan);
        }
    }
}

/// The text a suffix item contributes to `Invocation::args`, or `None` for
/// a redirect (never an argument word).
///
/// Only a *leading* assignment, in the command's prefix, is an environment
/// assignment in bash; the grammar still tags a `NAME=VALUE`-shaped suffix
/// item as `AssignmentWord` regardless of position, but a suffix one is an
/// ordinary word the command receives as an argument (`grep a=b file`'s
/// `a=b` is `grep`'s search pattern, not an assignment), so its raw text is
/// kept in `args` the same as any other suffix word.
///
/// A suffix `ProcessSubstitution` (`<(...)`/`>(...)`) is likewise an
/// ordinary argument word from the command's point of view. It carries no
/// `Word` of its own, so its text is rebuilt from the already-parsed
/// subshell's source span plus its kind's `<`/`>` marker; the inner command
/// is still walked and reported through `walk_prefix_or_suffix_item`; this
/// only supplies the text `args` would otherwise be missing entirely.
fn suffix_item_arg(text: &str, item: &CommandPrefixOrSuffixItem) -> Option<String> {
    match item {
        CommandPrefixOrSuffixItem::Word(word)
        | CommandPrefixOrSuffixItem::AssignmentWord(_, word) => Some(word.value.clone()),
        CommandPrefixOrSuffixItem::ProcessSubstitution(kind, subshell) => {
            let inner = subshell
                .location()
                .map(|loc| slice_bytes(text, loc.start.offset, loc.end.offset))
                .unwrap_or_default();
            Some(format!("{kind}{inner}"))
        }
        CommandPrefixOrSuffixItem::IoRedirect(_) => None,
    }
}

enum CommandWord {
    /// The command word's literal text, built piece-by-piece
    /// (`word_literal_text`) rather than re-unquoted from the word's raw
    /// source.
    Literal(String),
    /// The word is dynamic (`UnreducedReason::DynamicName`), carrying its
    /// already-parsed pieces so the caller can walk them for embedded
    /// command positions without a second call to `parse_word_pieces` on
    /// the same text.
    Dynamic(Vec<word::WordPieceWithSource>),
    Unreduced(UnreducedReason),
}

/// A command word is dynamic (FR-CMD-007's `DynamicName`) when any piece of
/// it is something the shell resolves at runtime rather than a literal
/// string: a parameter expansion (`$SEARCH`, `"$@"`), a command or
/// backquoted substitution used as the command name itself
/// (`"$(echo grep)"`), an arithmetic expression, or a tilde expansion. A
/// glob-like literal (`gr?p`) has no such piece, so it is left as an
/// ordinary, if unmatched, binary -- the splitter does not get to decide
/// what a glob means. A word past the bracket-nesting guard, or one
/// `word::parse` itself cannot parse, is also `Unreduced`, carrying the
/// matching reason: the caller records it the same as `walk_word` does for
/// any other word, rather than resolving a binary from text the guard
/// refused to look at.
fn classify_command_word(word: &ast::Word, depth: u8) -> CommandWord {
    match parse_word_pieces(&word.value, depth) {
        Ok(pieces) => {
            if pieces.iter().any(|piece| is_dynamic_piece(&piece.piece)) {
                CommandWord::Dynamic(pieces)
            } else {
                CommandWord::Literal(word_literal_text(&pieces))
            }
        }
        Err(reason) => CommandWord::Unreduced(reason),
    }
}

fn is_dynamic_piece(piece: &WordPiece) -> bool {
    match piece {
        WordPiece::ParameterExpansion(_)
        | WordPiece::CommandSubstitution(_)
        | WordPiece::BackquotedCommandSubstitution(_)
        | WordPiece::ArithmeticExpression(_)
        | WordPiece::TildeExpansion(_) => true,
        // The crate hands back an ANSI-C quoted piece's inner text raw,
        // without decoding its escapes (`$'\x67rep'` carries the literal
        // four characters `\x67rep`, not a decoded `g`): a piece with no
        // backslash is exact as literal text, but one that does carry a
        // backslash names a value the shell computes at runtime the same
        // way a parameter expansion does, and treating it as literal would
        // hand `to_binary` un-decoded escape syntax as if it were a real
        // basename. This is a grammar fact about what the crate does and
        // does not decode, not a table of tool names.
        WordPiece::AnsiCQuotedText(text) => text.contains('\\'),
        WordPiece::DoubleQuotedSequence(pieces)
        | WordPiece::GettextDoubleQuotedSequence(pieces) => {
            pieces.iter().any(|p| is_dynamic_piece(&p.piece))
        }
        WordPiece::Text(_) | WordPiece::SingleQuotedText(_) | WordPiece::EscapeSequence(_) => false,
    }
}

/// Assembles a word's literal text directly from its already-parsed pieces,
/// rather than re-unquoting the word's raw source: `classify_command_word`
/// only reaches this once every piece has already been confirmed
/// non-dynamic by `is_dynamic_piece`, so every piece here is one of the
/// literal-text kinds `piece_literal_text` handles.
fn word_literal_text(pieces: &[word::WordPieceWithSource]) -> String {
    pieces
        .iter()
        .map(|p| piece_literal_text(&p.piece))
        .collect()
}

/// A piece's own literal text. Only reached for a piece `is_dynamic_piece`
/// judged non-dynamic, so the five dynamic-only variants never arrive here;
/// they are listed rather than covered by a wildcard so a new `WordPiece`
/// variant added upstream fails to compile here instead of silently
/// resolving to an empty string. Each panics rather than returning an empty
/// string if it is ever reached anyway: a future narrowing of
/// `is_dynamic_piece` that stops excluding one of these variants must fail
/// loudly here, not hand `to_binary` a silently truncated binary.
fn piece_literal_text(piece: &WordPiece) -> String {
    match piece {
        WordPiece::Text(text)
        | WordPiece::SingleQuotedText(text)
        | WordPiece::AnsiCQuotedText(text) => text.clone(),
        WordPiece::DoubleQuotedSequence(pieces)
        | WordPiece::GettextDoubleQuotedSequence(pieces) => pieces
            .iter()
            .map(|p| piece_literal_text(&p.piece))
            .collect(),
        // `\g` parses as an EscapeSequence carrying the raw two-byte source
        // `\g`; its literal value is the escaped character alone.
        WordPiece::EscapeSequence(text) => text.strip_prefix('\\').unwrap_or(text).to_string(),
        WordPiece::TildeExpansion(_)
        | WordPiece::ParameterExpansion(_)
        | WordPiece::CommandSubstitution(_)
        | WordPiece::BackquotedCommandSubstitution(_)
        | WordPiece::ArithmeticExpression(_) => {
            unreachable!("is_dynamic_piece excludes this variant from reaching piece_literal_text")
        }
    }
}

/// Basename of the command word: the text after the last `/`
/// (FR-CMD-007's `Invocation::binary`). `raw` is already a word's literal
/// text (`word_literal_text`), assembled piece-by-piece with quotes and
/// escapes already resolved -- a leading backslash escape is stripped there
/// (`piece_literal_text`'s `EscapeSequence` arm), so there is no unresolved
/// backslash left for this function to strip.
fn to_binary(raw: &str) -> String {
    match raw.rfind('/') {
        Some(index) => raw[index + 1..].to_string(),
        None => raw.to_string(),
    }
}

fn walk_prefix_or_suffix_item(
    text: &str,
    item: &CommandPrefixOrSuffixItem,
    depth: u8,
    scan: &mut Scan,
) {
    match item {
        CommandPrefixOrSuffixItem::IoRedirect(redirect) => {
            walk_redirect(text, redirect, depth, scan)
        }
        CommandPrefixOrSuffixItem::Word(word) => walk_word(word, depth, scan),
        CommandPrefixOrSuffixItem::AssignmentWord(_, word) => walk_word(word, depth, scan),
        CommandPrefixOrSuffixItem::ProcessSubstitution(_kind, subshell) => {
            walk_process_substitution(text, subshell, depth, scan);
        }
    }
}

fn walk_redirect_list(text: &str, redirects: &RedirectList, depth: u8, scan: &mut Scan) {
    for redirect in &redirects.0 {
        walk_redirect(text, redirect, depth, scan);
    }
}

fn walk_redirect(text: &str, redirect: &IoRedirect, depth: u8, scan: &mut Scan) {
    match redirect {
        IoRedirect::File(_, _, target) => walk_redirect_target(text, target, depth, scan),
        IoRedirect::HereDocument(_, heredoc) => scan.unreduced.push(Unreduced {
            text: heredoc.doc.value.clone(),
            reason: UnreducedReason::InterpreterBody,
            depth,
        }),
        IoRedirect::HereString(_, word) => walk_word(word, depth, scan),
        IoRedirect::OutputAndError(word, _) => walk_word(word, depth, scan),
    }
}

fn walk_redirect_target(text: &str, target: &IoFileRedirectTarget, depth: u8, scan: &mut Scan) {
    match target {
        IoFileRedirectTarget::Filename(word) | IoFileRedirectTarget::Duplicate(word) => {
            walk_word(word, depth, scan);
        }
        IoFileRedirectTarget::Fd(_) => {}
        IoFileRedirectTarget::ProcessSubstitution(_kind, subshell) => {
            walk_process_substitution(text, subshell, depth, scan);
        }
    }
}

/// Parses a word's text into pieces, the shared entry `classify_command_word`
/// and `walk_word` both call. The bracket-nesting guard runs first (the
/// third guard site FR-CMD-007's Behavior names, alongside `scan_at`'s guard
/// over the top-level command and every re-entered payload): a word whose
/// raw text is nested past the bound is refused before `word::parse` ever
/// sees it, the same way `scan_at` refuses a command string. A word the
/// guard passes but `word::parse` still cannot parse comes back `Unparsed`.
fn parse_word_pieces(
    value: &str,
    depth: u8,
) -> Result<Vec<word::WordPieceWithSource>, UnreducedReason> {
    if exceeds_depth_guard(value, depth) || exceeds_keyword_depth_guard(value, depth) {
        return Err(UnreducedReason::TooDeep);
    }
    let options = ParserOptions::default();
    word::parse(value, &options).map_err(|_| UnreducedReason::Unparsed)
}

/// Scans a word for embedded command or backquoted substitutions
/// (FR-CMD-007). Every other piece kind (plain text, parameter expansions,
/// arithmetic) carries no command position and is left alone. A word that
/// fails to parse -- past the bracket-nesting guard, or unparseable outright
/// -- is reported as `Unreduced` with the matching reason rather than
/// silently skipped, so a substitution buried in it is not lost from the
/// scan without a trace.
fn walk_word(word: &ast::Word, depth: u8, scan: &mut Scan) {
    let pieces = match parse_word_pieces(&word.value, depth) {
        Ok(pieces) => pieces,
        Err(reason) => {
            scan.unreduced.push(Unreduced {
                text: word.value.clone(),
                reason,
                depth,
            });
            return;
        }
    };
    for piece in &pieces {
        walk_word_piece(&piece.piece, depth, scan);
    }
}

fn walk_word_piece(piece: &WordPiece, depth: u8, scan: &mut Scan) {
    match piece {
        WordPiece::CommandSubstitution(inner) | WordPiece::BackquotedCommandSubstitution(inner) => {
            rescan_substitution(inner, depth, scan);
        }
        WordPiece::DoubleQuotedSequence(pieces)
        | WordPiece::GettextDoubleQuotedSequence(pieces) => {
            for nested in pieces {
                walk_word_piece(&nested.piece, depth, scan);
            }
        }
        WordPiece::ParameterExpansion(expr) => walk_parameter_expr(expr, depth, scan),
        WordPiece::ArithmeticExpression(expr) => walk_embedded_text(&expr.value, depth, scan),
        // No embedded shell text: nothing to walk.
        WordPiece::Text(_)
        | WordPiece::SingleQuotedText(_)
        | WordPiece::AnsiCQuotedText(_)
        | WordPiece::EscapeSequence(_)
        | WordPiece::TildeExpansion(_) => {}
    }
}

/// Walks a `ParameterExpr`'s own embedded shell text (FR-CMD-007's "the walk
/// word-parses every word"): several of its fields -- a default, alternative
/// or error value, a pattern, a replacement, an offset or length -- carry raw
/// shell text the grammar has not yet parsed into pieces (unlike
/// `DoubleQuotedSequence`, which the parser already broke down). A command
/// substitution buried in one of these must not be lost, the same as one
/// buried in an `ArithmeticExpression` piece.
fn walk_parameter_expr(expr: &word::ParameterExpr, depth: u8, scan: &mut Scan) {
    use word::ParameterExpr as PE;
    match expr {
        PE::UseDefaultValues { default_value, .. }
        | PE::AssignDefaultValues { default_value, .. } => {
            walk_embedded_text_opt(default_value, depth, scan);
        }
        PE::IndicateErrorIfNullOrUnset { error_message, .. } => {
            walk_embedded_text_opt(error_message, depth, scan);
        }
        PE::UseAlternativeValue {
            alternative_value, ..
        } => {
            walk_embedded_text_opt(alternative_value, depth, scan);
        }
        PE::RemoveSmallestSuffixPattern { pattern, .. }
        | PE::RemoveLargestSuffixPattern { pattern, .. }
        | PE::RemoveSmallestPrefixPattern { pattern, .. }
        | PE::RemoveLargestPrefixPattern { pattern, .. }
        | PE::UppercaseFirstChar { pattern, .. }
        | PE::UppercasePattern { pattern, .. }
        | PE::LowercaseFirstChar { pattern, .. }
        | PE::LowercasePattern { pattern, .. } => {
            walk_embedded_text_opt(pattern, depth, scan);
        }
        PE::Substring { offset, length, .. } => {
            walk_embedded_text(&offset.value, depth, scan);
            if let Some(length) = length {
                walk_embedded_text(&length.value, depth, scan);
            }
        }
        PE::ReplaceSubstring {
            pattern,
            replacement,
            ..
        } => {
            walk_embedded_text(pattern, depth, scan);
            walk_embedded_text_opt(replacement, depth, scan);
        }
        // A bare parameter reference or its length carries no expansion
        // text of its own beyond a possible array subscript, which the
        // parser leaves as raw text on the `Parameter` itself
        // (`Parameter::NamedWithIndex`'s `index` field) the same way an
        // arithmetic expression's text is raw: `${x[$(grep a)]}` and
        // `${#x[$(grep a)]}` must not lose `grep` any more than
        // `${x[1]:-$(grep a)}` loses it from `default_value`.
        PE::Parameter { parameter, .. } | PE::ParameterLength { parameter, .. } => {
            walk_parameter_subscript(parameter, depth, scan);
        }
        // No embedded shell text: a transform (none of whose operations
        // carry raw text), or a literal variable-name prefix.
        PE::Transform { .. } | PE::VariableNames { .. } | PE::MemberKeys { .. } => {}
    }
}

/// Walks a `Parameter`'s own array subscript, when it has one
/// (`Parameter::NamedWithIndex`'s `index` field): raw shell text the parser
/// has not broken down, so a substitution inside it is not lost. Every
/// other `Parameter` variant (a positional, special, or bare named
/// parameter, or an all-indices reference) carries no such text.
fn walk_parameter_subscript(parameter: &word::Parameter, depth: u8, scan: &mut Scan) {
    if let word::Parameter::NamedWithIndex { index, .. } = parameter {
        walk_embedded_text(index, depth, scan);
    }
}

fn walk_embedded_text_opt(text: &Option<String>, depth: u8, scan: &mut Scan) {
    if let Some(text) = text {
        walk_embedded_text(text, depth, scan);
    }
}

/// Word-parses a raw string extracted from a piece's own field (a parameter
/// expansion's default value, an arithmetic expression's raw text) rather
/// than a grammar-carried `Word`, so a synthetic `Word` wraps it for
/// `walk_word`'s existing guard-then-parse-then-walk path. `depth` is passed
/// through unchanged, not incremented: this is further parsing of text that
/// already sits inside the enclosing word, not a re-entry into a new command
/// scope the way a command substitution is -- the extracted text is always a
/// proper substring of the word `parse_word_pieces` already guarded at this
/// same depth, so recursion here is bounded by that substring's shrinking
/// length. A real depth bump still happens the moment a nested command
/// substitution surfaces and `rescan_substitution` re-parses it as a program.
fn walk_embedded_text(text: &str, depth: u8, scan: &mut Scan) {
    let word = ast::Word {
        value: text.to_string(),
        loc: None,
    };
    walk_word(&word, depth, scan);
}

/// Re-enters a command or process substitution's text (FR-CMD-007): the
/// splitter re-enters these itself, because they are grammar, unlike a
/// wrapper payload which needs a name the policy supplies. The re-parsed
/// text is walked with `Substitution` as its tag via `scan_tagged`, the same
/// mechanism `walk_process_substitution` uses for an already-parsed process
/// substitution: an assignment prefix still outranks it, and a function body
/// found inside still overrides it, so the two substitution kinds resolve
/// position by the one shared rule instead of two.
fn rescan_substitution(inner: &str, depth: u8, scan: &mut Scan) {
    let next_depth = depth.saturating_add(1);
    match scan_tagged(inner, next_depth, Some(Position::Substitution)) {
        Ok(mut inner_scan) => {
            scan.invocations.append(&mut inner_scan.invocations);
            scan.unreduced.append(&mut inner_scan.unreduced);
        }
        Err(_) => scan.unreduced.push(Unreduced {
            text: inner.to_string(),
            reason: UnreducedReason::Unparsed,
            depth: next_depth,
        }),
    }
}

/// Walks an already-parsed process substitution (`<(...)`/`>(...)`), which
/// `brush-parser` hands the splitter as AST rather than raw text: there is no
/// fresh parse call here, and so no `ScanError` path, because the enclosing
/// `Command`'s own parse already produced this subshell's tree.
fn walk_process_substitution(text: &str, subshell: &SubshellCommand, depth: u8, scan: &mut Scan) {
    let next_depth = depth.saturating_add(1);
    if next_depth >= MAX_DEPTH {
        let raw = subshell
            .location()
            .map(|loc| slice_bytes(text, loc.start.offset, loc.end.offset))
            .unwrap_or_default();
        scan.unreduced.push(too_deep_unreduced(&raw, next_depth));
        return;
    }
    walk_list(
        text,
        &subshell.list,
        next_depth,
        Some(Position::Substitution),
        Position::First,
        scan,
    );
}

fn walk_function_body(text: &str, body: &ast::FunctionBody, depth: u8, scan: &mut Scan) {
    walk_compound_command_tagged(
        text,
        &body.0,
        depth,
        Some(Position::FunctionBody),
        Position::First,
        scan,
    );
    if let Some(redirects) = &body.1 {
        walk_redirect_list(text, redirects, depth, scan);
    }
}

/// `outer_position` is the `Position` `walk_command`'s `Compound` arm
/// resolved for this compound command as a whole (`First`, `AfterOperator`,
/// or `AfterAssignment`): the position it would have reported had it been a
/// simple command instead. A `BraceGroup` or `Subshell` is a single
/// sequence with no independent entry point of its own, so its first
/// command inherits `outer_position` rather than resetting to `First` --
/// `cat x | (grep a)` and `cat x | { grep a; }` report `grep` at
/// `AfterOperator`, matching the pipe it actually sits behind. Every other
/// variant keeps today's documented rule instead: `if`/`while`/`until`
/// conditions, `for`/`case`/coprocess bodies, and `then`/`else` branches
/// each get their own fresh `Position::First`, because each is a distinct
/// body the module's docs already describe as independently walked "the
/// same way the top-level program is walked" -- narrowing this fix to the
/// two verified constructs rather than re-deriving that rule for every
/// variant.
fn walk_compound_command_tagged(
    text: &str,
    compound: &CompoundCommand,
    depth: u8,
    tag: Tag,
    outer_position: Position,
    scan: &mut Scan,
) {
    match compound {
        CompoundCommand::BraceGroup(brace_group) => {
            walk_list(text, &brace_group.list, depth, tag, outer_position, scan)
        }
        CompoundCommand::Subshell(subshell) => {
            walk_list(text, &subshell.list, depth, tag, outer_position, scan)
        }
        CompoundCommand::ForClause(for_clause) => {
            if let Some(values) = &for_clause.values {
                for value in values {
                    walk_word(value, depth, scan);
                }
            }
            walk_list(
                text,
                &for_clause.body.list,
                depth,
                tag,
                Position::First,
                scan,
            );
        }
        CompoundCommand::CaseClause(case_clause) => {
            walk_word(&case_clause.value, depth, scan);
            for item in &case_clause.cases {
                for pattern in &item.patterns {
                    walk_word(pattern, depth, scan);
                }
                if let Some(body) = &item.cmd {
                    walk_list(text, body, depth, tag, Position::First, scan);
                }
            }
        }
        CompoundCommand::IfClause(if_clause) => {
            walk_list(
                text,
                &if_clause.condition,
                depth,
                tag,
                Position::First,
                scan,
            );
            walk_list(text, &if_clause.then, depth, tag, Position::First, scan);
            if let Some(elses) = &if_clause.elses {
                for else_clause in elses {
                    if let Some(condition) = &else_clause.condition {
                        walk_list(text, condition, depth, tag, Position::First, scan);
                    }
                    walk_list(text, &else_clause.body, depth, tag, Position::First, scan);
                }
            }
        }
        CompoundCommand::WhileClause(while_or_until)
        | CompoundCommand::UntilClause(while_or_until) => {
            walk_list(text, &while_or_until.0, depth, tag, Position::First, scan);
            walk_list(
                text,
                &while_or_until.1.list,
                depth,
                tag,
                Position::First,
                scan,
            );
        }
        CompoundCommand::Coprocess(coprocess) => {
            walk_command(
                text,
                &coprocess.body,
                depth,
                tag,
                tag.unwrap_or(Position::First),
                scan,
            );
        }
        CompoundCommand::ArithmeticForClause(arithmetic_for_clause) => {
            if let Some(initializer) = &arithmetic_for_clause.initializer {
                walk_embedded_text(&initializer.value, depth, scan);
            }
            if let Some(condition) = &arithmetic_for_clause.condition {
                walk_embedded_text(&condition.value, depth, scan);
            }
            if let Some(updater) = &arithmetic_for_clause.updater {
                walk_embedded_text(&updater.value, depth, scan);
            }
            walk_list(
                text,
                &arithmetic_for_clause.body.list,
                depth,
                tag,
                Position::First,
                scan,
            );
        }
        CompoundCommand::Arithmetic(arithmetic) => {
            walk_embedded_text(&arithmetic.expr.value, depth, scan);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn positions(scan: &Scan, binary: &str) -> Vec<Position> {
        scan.invocations
            .iter()
            .filter(|inv| inv.binary == binary)
            .map(|inv| inv.position)
            .collect()
    }

    fn args_for<'a>(scan: &'a Scan, binary: &str) -> &'a [String] {
        &scan
            .invocations
            .iter()
            .find(|inv| inv.binary == binary)
            .expect("binary present")
            .args
    }

    #[test]
    fn scan_at_carries_depth_across_a_reentry() {
        // A caller re-entering a wrapper payload (the router, once the
        // policy names the wrapper) passes its own depth through scan_at;
        // this is the mechanism that keeps a chain of re-entries bounded by
        // one shared counter.
        let scan = scan_at("grep -rn foo src", 3).expect("parses");
        assert_eq!(scan.invocations[0].position, Position::First);
        assert_eq!(scan.invocations[0].depth, 3);
    }

    #[test]
    fn scan_at_at_the_bound_is_too_deep_not_an_error() {
        // Entering exactly at MAX_DEPTH is refused as a region, not a
        // ScanError: the depth bound is a walk limit, not a parse failure.
        let scan = scan_at("grep -rn foo src", MAX_DEPTH).expect("a Scan, not a ScanError");
        assert!(scan.invocations.is_empty());
        assert_eq!(scan.unreduced.len(), 1);
        assert_eq!(scan.unreduced[0].reason, UnreducedReason::TooDeep);
        assert_eq!(scan.unreduced[0].text, "grep -rn foo src");
        assert_eq!(scan.unreduced[0].depth, MAX_DEPTH);
    }

    #[test]
    fn first_position_plain_command() {
        let scan = scan("grep -rn foo src").expect("parses");
        assert_eq!(positions(&scan, "grep"), vec![Position::First]);
        assert_eq!(scan.invocations[0].args, vec!["-rn", "foo", "src"]);
    }

    #[test]
    fn after_operator_pipe() {
        let scan = scan("cat file | grep foo").expect("parses");
        assert_eq!(positions(&scan, "grep"), vec![Position::AfterOperator]);
    }

    #[test]
    fn after_operator_list() {
        let scan = scan("git status && git push origin main").expect("parses");
        assert_eq!(
            positions(&scan, "git"),
            vec![Position::First, Position::AfterOperator]
        );
    }

    #[test]
    fn after_assignment_env_prefix() {
        let scan = scan("FOO=1 grep -n x").expect("parses");
        assert_eq!(positions(&scan, "grep"), vec![Position::AfterAssignment]);
    }

    #[test]
    fn after_assignment_outranks_pipe() {
        let scan = scan("cat x | FOO=1 grep y").expect("parses");
        assert_eq!(positions(&scan, "grep"), vec![Position::AfterAssignment]);
        assert_eq!(positions(&scan, "cat"), vec![Position::First]);
    }

    #[test]
    fn substitution_command_subst() {
        let scan = scan("echo $(grep -c x f)").expect("parses");
        assert_eq!(positions(&scan, "grep"), vec![Position::Substitution]);
    }

    #[test]
    fn substitution_backtick() {
        let scan = scan("for f in `ls src`; do echo $f; done").expect("parses");
        assert_eq!(positions(&scan, "ls"), vec![Position::Substitution]);
    }

    #[test]
    fn suffix_assignment_word_is_kept_as_an_ordinary_argument() {
        // Only a *leading* assignment (in the prefix) is an environment
        // assignment in bash; a NAME=VALUE-shaped word after the command
        // name is an ordinary argument the command receives verbatim.
        let scan = scan("grep a=b file").expect("parses");
        assert_eq!(args_for(&scan, "grep"), ["a=b", "file"]);
    }

    #[test]
    fn multiple_suffix_assignment_words_are_all_kept() {
        // `env` is an ordinary command word to the splitter (it holds no
        // wrapper table): `FOO=1 grep -rn foo src` are all `env`'s own
        // suffix words, and `FOO=1` looking like an assignment does not
        // make it disappear from `env`'s args.
        let scan = scan("env FOO=1 grep -rn foo src").expect("parses");
        assert_eq!(
            args_for(&scan, "env"),
            ["FOO=1", "grep", "-rn", "foo", "src"]
        );
    }

    #[test]
    fn suffix_process_substitution_is_kept_as_an_argument() {
        let scan = scan("diff <(ls a) <(ls b)").expect("parses");
        assert_eq!(args_for(&scan, "diff"), ["<(ls a)", "<(ls b)"]);
    }

    #[test]
    fn substitution_process_substitution() {
        let scan = scan("diff <(ls a) <(ls b)").expect("parses");
        assert_eq!(
            positions(&scan, "ls"),
            vec![Position::Substitution, Position::Substitution]
        );
    }

    #[test]
    fn function_body_tagged() {
        let scan = scan("function g { grep -rn \"$@\"; }; g foo src").expect("parses");
        assert_eq!(positions(&scan, "grep"), vec![Position::FunctionBody]);
        assert_eq!(positions(&scan, "g"), vec![Position::AfterOperator]);
    }

    #[test]
    fn posix_function_definition_is_not_a_call() {
        let scan = scan("g() { grep -rn \"$@\"; }; g foo src").expect("parses");
        assert_eq!(positions(&scan, "grep"), vec![Position::FunctionBody]);
        assert_eq!(positions(&scan, "g"), vec![Position::AfterOperator]);
    }

    #[test]
    fn function_body_inside_a_substitution_outranks_the_substitution_tag() {
        // A function body found inside a command substitution and one found
        // inside a process substitution must resolve identically: the
        // function body is the more immediate context, so it wins over the
        // Substitution tag either way.
        let command_sub = scan("echo $( g() { grep x; } )").expect("parses");
        assert_eq!(
            positions(&command_sub, "grep"),
            vec![Position::FunctionBody]
        );

        let process_sub = scan("diff <( g() { grep x; } )").expect("parses");
        assert_eq!(
            positions(&process_sub, "grep"),
            vec![Position::FunctionBody]
        );
    }

    #[test]
    fn arithmetic_for_clause_body_is_walked() {
        let scan = scan("for ((i=0; i<10; i++)); do rg secret .; done").expect("parses");
        assert_eq!(positions(&scan, "rg"), vec![Position::First]);
    }

    #[test]
    fn extended_test_words_are_walked_for_substitutions() {
        let scan = scan("[[ -n $(ls src) ]] && echo ok").expect("parses");
        assert_eq!(positions(&scan, "ls"), vec![Position::Substitution]);
        assert_eq!(positions(&scan, "echo"), vec![Position::AfterOperator]);
    }

    #[test]
    fn parameter_expansion_default_value_is_walked_for_substitutions() {
        let scan = scan("echo ${x:-$(hostname)}").expect("parses");
        assert_eq!(positions(&scan, "hostname"), vec![Position::Substitution]);
        let hostname = scan
            .invocations
            .iter()
            .find(|inv| inv.binary == "hostname")
            .expect("hostname invocation");
        assert_eq!(hostname.depth, 1);
    }

    #[test]
    fn arithmetic_expression_piece_is_walked_for_substitutions() {
        let scan = scan("echo $(( $(wc -l < f) + 1 ))").expect("parses");
        assert_eq!(positions(&scan, "wc"), vec![Position::Substitution]);
        let wc = scan
            .invocations
            .iter()
            .find(|inv| inv.binary == "wc")
            .expect("wc invocation");
        assert_eq!(wc.depth, 1);
    }

    #[test]
    fn arithmetic_command_expression_is_walked_for_substitutions() {
        // `CompoundCommand::Arithmetic` carries the same raw
        // `UnexpandedArithmeticExpr` text as the `ArithmeticExpression` word
        // piece above; a substitution in it must not be silently dropped.
        let scan = scan("(( n = $(ls | wc -l) ))").expect("parses");
        assert_eq!(positions(&scan, "ls"), vec![Position::Substitution]);
        assert_eq!(positions(&scan, "wc"), vec![Position::Substitution]);
    }

    #[test]
    fn arithmetic_for_clause_header_is_walked_for_substitutions() {
        // The initializer, condition, and updater expressions of a `for
        // ((...))` header are raw arithmetic text, the same as the body of
        // an ordinary `((...))` command; only the body list was walked
        // before this test.
        let scan = scan("for (( i=$(wc -l < f); i>0; i-- )); do rg x; done").expect("parses");
        assert_eq!(positions(&scan, "wc"), vec![Position::Substitution]);
        assert_eq!(positions(&scan, "rg"), vec![Position::First]);
    }

    #[test]
    fn subshell_after_a_pipe_reports_after_operator() {
        // The compound command's own sequence position (from the pipe or
        // list operator it sits behind) is threaded into the subshell's
        // first command instead of always resetting to First.
        let scan = scan("cat x | (grep a)").expect("parses");
        assert_eq!(positions(&scan, "grep"), vec![Position::AfterOperator]);
    }

    #[test]
    fn brace_group_after_a_pipe_reports_after_operator() {
        let scan = scan("cat x | { grep a; }").expect("parses");
        assert_eq!(positions(&scan, "grep"), vec![Position::AfterOperator]);
    }

    #[test]
    fn subshell_after_a_list_operator_reports_after_operator() {
        let scan = scan("a && (grep b)").expect("parses");
        assert_eq!(positions(&scan, "grep"), vec![Position::AfterOperator]);
    }

    #[test]
    fn subshell_in_first_position_still_reports_first() {
        let scan = scan("(grep a)").expect("parses");
        assert_eq!(positions(&scan, "grep"), vec![Position::First]);
    }

    #[test]
    fn ansi_c_quoted_command_word_resolves_the_real_basename() {
        // `$'grep'` carries no backslash escape, so its inner text is exact
        // literal text: the basename is built from the parsed pieces
        // instead of re-unquoting the raw source, so no leading `$`
        // survives into `binary`.
        let scan = scan("$'grep' -rn foo").expect("parses");
        assert_eq!(positions(&scan, "grep"), vec![Position::First]);
    }

    #[test]
    fn gettext_quoted_command_word_resolves_the_real_basename() {
        let scan = scan("$\"grep\" -rn foo").expect("parses");
        assert_eq!(positions(&scan, "grep"), vec![Position::First]);
    }

    #[test]
    fn ansi_c_quoted_command_word_with_an_escape_is_dynamic_not_a_wrong_literal() {
        // The crate hands back an ANSI-C quoted piece's inner text without
        // decoding its escapes, so `$'\x67rep'` cannot be reduced to a real
        // basename here; it must come back as an opaque DynamicName region
        // rather than a confidently-wrong literal like `\x67rep` or `$x67rep`.
        let scan = scan("$'\\x67rep' -rn foo").expect("parses");
        assert!(scan.invocations.is_empty());
        assert_eq!(scan.unreduced.len(), 1);
        assert_eq!(scan.unreduced[0].reason, UnreducedReason::DynamicName);
    }

    #[test]
    fn parameter_array_subscript_is_walked_for_substitutions() {
        let scan = scan("echo ${x[$(grep a)]}").expect("parses");
        assert_eq!(positions(&scan, "grep"), vec![Position::Substitution]);
    }

    #[test]
    fn parameter_length_array_subscript_is_walked_for_substitutions() {
        let scan = scan("echo ${#x[$(grep a)]}").expect("parses");
        assert_eq!(positions(&scan, "grep"), vec![Position::Substitution]);
    }

    #[test]
    fn case_subject_word_is_walked_for_substitutions() {
        // The case subject word (case_clause.value) is a word-parse site
        // FR-CMD-007's Behavior names alongside the for value list and the
        // case patterns; a substitution in it must not be lost.
        let scan = scan("case $(ls src) in a) rg foo ;; *) fd bar ;; esac").expect("parses");
        assert_eq!(positions(&scan, "ls"), vec![Position::Substitution]);
        assert_eq!(positions(&scan, "rg"), vec![Position::First]);
        assert_eq!(positions(&scan, "fd"), vec![Position::First]);
    }

    #[test]
    fn commit_message_heredoc_in_substitution() {
        // The apostrophe inside the heredoc body, itself inside `$(...)`,
        // inside a double-quoted argument, is the case that produced 142 of
        // 149 parse errors in the research toy -- it must not open a quote.
        let cmd = "git add src/a.rs && git commit -m \"$(cat <<'EOF'\nfix: don't trust the agent's \"quotes\" (it's a body)\nEOF\n)\" && git push origin HEAD";
        let scan = scan(cmd).expect("parses");
        assert_eq!(
            positions(&scan, "git"),
            vec![
                Position::First,
                Position::AfterOperator,
                Position::AfterOperator
            ]
        );
        // `cat`, run inside the `$(...)` to build the commit message, is the
        // command the substitution recovers; its own heredoc body stays an
        // opaque InterpreterBody region.
        assert_eq!(positions(&scan, "cat"), vec![Position::Substitution]);
        assert!(
            scan.unreduced
                .iter()
                .any(|u| u.reason == UnreducedReason::InterpreterBody)
        );
    }

    #[test]
    fn dynamic_command_word_from_substitution() {
        // The command word itself is DynamicName (the shell builds it at
        // runtime), but the substitution inside it is still grammar the
        // splitter re-enters: `echo` must not be silently lost just because
        // it happens to sit in command-word position.
        let scan = scan("\"$(echo grep)\" -rn foo src").expect("parses");
        assert_eq!(scan.unreduced.len(), 1);
        assert_eq!(scan.unreduced[0].reason, UnreducedReason::DynamicName);
        assert_eq!(positions(&scan, "echo"), vec![Position::Substitution]);
        let echo = scan
            .invocations
            .iter()
            .find(|inv| inv.binary == "echo")
            .expect("echo invocation");
        assert_eq!(echo.depth, 1);
    }

    #[test]
    fn dynamic_command_word_from_parameter() {
        let scan = scan("$SEARCH -rn foo src").expect("parses");
        assert!(scan.invocations.is_empty());
        assert_eq!(scan.unreduced[0].reason, UnreducedReason::DynamicName);
    }

    #[test]
    fn glob_like_binary_is_resolved_literally() {
        let scan = scan("gr?p -rn foo src").expect("parses");
        assert_eq!(scan.invocations[0].binary, "gr?p");
    }

    #[test]
    fn wrapper_payload_is_opaque_to_the_splitter() {
        let scan = scan("sh -c 'grep -rn foo src'").expect("parses");
        assert_eq!(positions(&scan, "sh"), vec![Position::First]);
        assert!(scan.invocations.iter().all(|inv| inv.binary != "grep"));
        assert!(scan.unreduced.is_empty());
    }

    #[test]
    fn heredoc_body_is_interpreter_body() {
        let scan = scan("bash <<'EOF'\ncd src\nrg -n 'fn main' .\nEOF").expect("parses");
        assert_eq!(positions(&scan, "bash"), vec![Position::First]);
        assert!(scan.invocations.iter().all(|inv| inv.binary != "rg"));
        assert_eq!(scan.unreduced.len(), 1);
        assert_eq!(scan.unreduced[0].reason, UnreducedReason::InterpreterBody);
    }

    #[test]
    fn too_deep_is_reported_not_walked() {
        let mut nested = String::from("grep -c x f");
        for _ in 0..30 {
            nested = format!("echo $({nested})");
        }
        let scan = scan(&nested).expect("parses");
        assert!(scan.invocations.iter().all(|inv| inv.binary != "grep"));
        assert!(
            scan.unreduced
                .iter()
                .any(|u| u.reason == UnreducedReason::TooDeep)
        );
    }

    #[test]
    fn shallow_nesting_is_not_too_deep() {
        let scan = scan("echo $(echo $(echo $(grep -c x f)))").expect("parses");
        assert_eq!(positions(&scan, "grep"), vec![Position::Substitution]);
    }

    #[test]
    fn malformed_command_is_a_scan_error_not_a_partial_scan() {
        let err = scan("grep -n 'oops src").expect_err("unterminated quote must error");
        assert!(matches!(err, ScanError::UnterminatedQuote { offset: 8 }));
    }

    #[test]
    fn quoted_unbalanced_brackets_do_not_trip_the_depth_guard() {
        // 26 independent, single-quoted, unclosed openers -- bracket
        // nesting depth 0 -- must not be refused as TooDeep: each is a
        // literal character inside a quote the tokenizer scans flat, never
        // recursing through it.
        let mut command = String::new();
        for i in 0..26 {
            if i > 0 {
                command.push_str(" && ");
            }
            command.push_str(&format!("rg '[' f{i}"));
        }
        let scan = scan(&command).expect("parses");
        assert_eq!(scan.invocations.len(), 26);
        assert!(
            scan.unreduced.is_empty(),
            "expected no unreduced regions, got {:?}",
            scan.unreduced
        );
    }

    #[test]
    fn escaped_unbalanced_brackets_do_not_trip_the_depth_guard() {
        // Same shape, unquoted but backslash-escaped: a literal character,
        // not an opener the grammar recurses through.
        let mut command = String::new();
        for i in 0..26 {
            if i > 0 {
                command.push_str(" && ");
            }
            command.push_str(&format!("rg \\( f{i}"));
        }
        let scan = scan(&command).expect("parses");
        assert_eq!(scan.invocations.len(), 26);
        assert!(scan.unreduced.is_empty());
    }

    #[test]
    fn real_bracket_nesting_past_the_bound_is_still_too_deep() {
        let mut nested = String::from("grep -c x f");
        for _ in 0..30 {
            nested = format!("echo $({nested})");
        }
        let scan = scan(&nested).expect("parses");
        assert!(scan.invocations.iter().all(|inv| inv.binary != "grep"));
        assert!(
            scan.unreduced
                .iter()
                .any(|u| u.reason == UnreducedReason::TooDeep)
        );
    }

    #[test]
    fn keyword_nesting_past_the_bound_is_guarded_before_parsing() {
        // Reserved-word nesting at bracket depth 0 must be caught by
        // exceeds_keyword_depth_guard directly: going through the real
        // parser at this depth is exactly what aborts the process, so the
        // guard function is exercised here rather than through `scan`.
        let mut nested = String::new();
        for _ in 0..30 {
            nested.push_str("if true; then ");
        }
        nested.push_str("grep a");
        for _ in 0..30 {
            nested.push_str("; fi");
        }
        assert!(exceeds_keyword_depth_guard(&nested, 0));
    }

    #[test]
    fn ordinary_keyword_use_does_not_trip_the_keyword_guard() {
        assert!(!exceeds_keyword_depth_guard("if true; then grep a; fi", 0));
        assert!(!exceeds_keyword_depth_guard(
            "for f in a b c; do grep x \"$f\"; done",
            0
        ));
    }

    #[test]
    fn quoted_keyword_text_does_not_trip_the_keyword_guard() {
        // The letters "if"/"for"/"case" appearing inside a single-quoted
        // argument are not reserved words; the guard must not count them.
        let mut command = String::new();
        for i in 0..40 {
            if i > 0 {
                command.push_str(" && ");
            }
            command.push_str("grep 'if for case until while' f");
        }
        assert!(!exceeds_keyword_depth_guard(&command, 0));
    }

    #[test]
    fn identifier_containing_a_keyword_does_not_trip_the_keyword_guard() {
        // `for_each`/`ifconfig`/`case_id` are single tokens, not the
        // reserved words `for`/`if`/`case` followed by something else.
        let command = "for_each=1 ifconfig case_id=2 grep a".repeat(40);
        assert!(!exceeds_keyword_depth_guard(&command, 0));
    }

    #[test]
    fn double_quoted_keyword_text_does_not_trip_the_keyword_guard() {
        // The letters "for" inside a double-quoted commit message are not a
        // reserved word -- only `exceeds_depth_guard`'s single-quote
        // handling was mirrored here before this test; double-quoted text
        // fell through uncounted, so the same message spelled with double
        // quotes tripped a false TooDeep once repeated past the budget.
        let mut command = String::new();
        for i in 0..40 {
            if i > 0 {
                command.push_str(" && ");
            }
            command.push_str("git commit -m \"fix for f\"");
        }
        assert!(!exceeds_keyword_depth_guard(&command, 0));
    }

    #[test]
    fn double_quoted_commit_message_keyword_is_visible_end_to_end() {
        // The guard-level check above in a full `scan`: a chain of ordinary
        // commits whose messages happen to contain reserved words must
        // resolve every stage as `visible`, not collapse the whole pipeline
        // into one TooDeep region.
        let mut command = String::new();
        for i in 0..26 {
            if i > 0 {
                command.push_str(" && ");
            }
            command.push_str("git commit -m \"fix for f\"");
        }
        let scan = scan(&command).expect("parses");
        assert_eq!(positions(&scan, "git").len(), 26);
        assert!(scan.unreduced.is_empty());
    }

    #[test]
    fn never_panics_on_deeply_nested_raw_input() {
        // A property-style sweep over generator-built inputs; see
        // proptest_never_panics for the seeded generator version.
        let mut opener_soup = String::new();
        for i in 0..5000u32 {
            opener_soup.push(match i % 6 {
                0 => '(',
                1 => ')',
                2 => '{',
                3 => '[',
                4 => '`',
                _ => '$',
            });
        }
        let _ = scan(&opener_soup);
    }

    #[test]
    fn parse_word_pieces_rejects_deep_nesting_before_calling_the_parser() {
        // The third guard site FR-CMD-007's Behavior names: a word's own raw
        // text, past the bracket-nesting bound, is refused before
        // `word::parse` ever runs over it -- the same discipline `scan_at`
        // applies to the top-level command and every re-entered payload.
        let deeply_nested: String = "(".repeat(usize::from(MAX_DEPTH) + 1);
        assert!(matches!(
            parse_word_pieces(&deeply_nested, 0),
            Err(UnreducedReason::TooDeep)
        ));
    }

    #[test]
    fn parse_word_pieces_reports_unparsed_for_a_word_the_parser_rejects() {
        assert!(matches!(
            parse_word_pieces("'unterminated", 0),
            Err(UnreducedReason::Unparsed)
        ));
    }

    #[test]
    fn classify_command_word_reports_unreduced_instead_of_a_silent_literal() {
        // Directly exercised because the TooDeep arm cannot be reached
        // through `scan`: `scan_at`'s own bracket-nesting guard already
        // refuses the whole command before a nested command word's text -- a
        // substring of it at the same budget -- could ever be deep enough to
        // trip the guard on its own. No `scan()` input reaching the
        // Unparsed arm was found either, so both are covered directly here.
        let too_deep = ast::Word {
            value: "(".repeat(usize::from(MAX_DEPTH) + 1),
            loc: None,
        };
        assert!(matches!(
            classify_command_word(&too_deep, 0),
            CommandWord::Unreduced(UnreducedReason::TooDeep)
        ));

        let unparsed = ast::Word {
            value: "'unterminated".to_string(),
            loc: None,
        };
        assert!(matches!(
            classify_command_word(&unparsed, 0),
            CommandWord::Unreduced(UnreducedReason::Unparsed)
        ));
    }

    #[test]
    fn walk_word_records_unparsed_instead_of_dropping_the_word() {
        // A word that fails to parse must surface as an Unreduced with
        // Unparsed, never be dropped silently.
        let word = ast::Word {
            value: "'unterminated".to_string(),
            loc: None,
        };
        let mut scan = Scan::default();
        walk_word(&word, 0, &mut scan);
        assert_eq!(scan.unreduced.len(), 1);
        assert_eq!(scan.unreduced[0].reason, UnreducedReason::Unparsed);
        assert_eq!(scan.unreduced[0].text, "'unterminated");
    }

    #[test]
    fn never_panics_on_deeply_nested_multi_byte_input() {
        // Multi-byte UTF-8 mixed into deeply nested command substitution
        // text must not panic, whatever recursion path it takes.
        let mut nested = String::from("grep -c x f");
        for _ in 0..30 {
            nested = format!("echo $({nested} \u{1F600})");
        }
        let _ = scan(&nested);
    }

    #[test]
    fn slice_bytes_falls_back_to_the_nearest_char_boundary() {
        // `slice_bytes` is `scan`'s only raw byte-range copy (used when a
        // too-deep process substitution's raw text is reported); a request
        // that lands mid-codepoint must not panic, and should widen to the
        // smallest slice that contains the requested range on valid
        // boundaries rather than losing the multi-byte character.
        let text = "grep \u{1F600} foo";
        // Byte 6 sits inside the 4-byte emoji that starts at byte 5.
        let sliced = slice_bytes(text, 0, 6);
        assert!(sliced.starts_with("grep "));
        assert!(sliced.contains('\u{1F600}'));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2000))]

        /// The property test FR-CMD-007's Error Handling asks for: `scan`
        /// must never panic over arbitrary strings, checked with a real
        /// shrinking fuzzer rather than a hand-rolled one. Unlike
        /// `seeded_alphabet_fuzz_never_panics` below (metacharacters only),
        /// this strategy's alphabet includes letters, digits, and path
        /// characters, so a generated string can actually contain a
        /// command word and exercise `classify_command_word`, `to_binary`,
        /// the `Literal` arm of `walk_simple_command`, and real
        /// `Invocation` construction -- not only the parse-failure and
        /// depth-guard paths.
        #[test]
        fn scan_never_panics_over_arbitrary_shell_like_strings(input in shell_like_string()) {
            let _ = scan(&input);
        }
    }

    /// A proptest strategy mixing shell metacharacters with alphanumerics
    /// and common path characters, so proptest's shrinking-capable
    /// generator can produce both well-formed-looking commands and
    /// adversarial nesting from the same alphabet.
    fn shell_like_string() -> impl Strategy<Value = String> {
        let alphabet: &'static [char] = &[
            'a', 'b', 'c', 'd', 'e', 'f', 'g', 'r', 'p', 'x', '0', '1', '2', '9', '_', '-', '.',
            '/', '(', ')', '[', ']', '{', '}', '`', '$', '"', '\'', '\\', '|', '&', ';', '<', '>',
            '*', ' ', '\t', '\n',
        ];
        prop::collection::vec(prop::sample::select(alphabet), 0..200)
            .prop_map(|chars| chars.into_iter().collect())
    }

    /// A small seeded generator over a byte alphabet that includes quotes,
    /// `$(`, backticks, braces, newlines, and high-UTF-8 bytes, plus
    /// deliberately deep nesting -- `scan` must never panic or abort on any
    /// of it. Reaching depth 25 here is probabilistic, not guaranteed by
    /// this generator; `too_deep_is_reported_not_walked` and
    /// `never_panics_on_deeply_nested_raw_input` cover the depth guard
    /// deterministically. Kept alongside the proptest above rather than
    /// replaced by it: this alphabet holds no alphanumerics by design, so it
    /// probes the parse-failure and guard paths harder than a realistic
    /// command string would, and it also injects raw non-UTF-8 bytes the
    /// proptest strategy above does not.
    #[test]
    fn seeded_alphabet_fuzz_never_panics() {
        let alphabet: &[u8] = b"()[]{}`$\"'\\|&;<>* \t\n";
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };

        for _ in 0..2000 {
            let len = (next() % 200) as usize;
            let mut bytes = Vec::with_capacity(len);
            for _ in 0..len {
                let choice = next() as usize;
                if choice.is_multiple_of(11) {
                    // Occasionally inject a raw high byte to probe non-UTF-8
                    // boundaries once mixed with ASCII.
                    bytes.push(0xF0);
                    bytes.push(0x9F);
                    bytes.push(0x98);
                    bytes.push(0x80);
                } else {
                    bytes.push(alphabet[choice % alphabet.len()]);
                }
            }
            let text = String::from_utf8_lossy(&bytes).to_string();
            let _ = scan(&text);
        }
    }

    #[test]
    fn single_simple_is_true_only_for_exactly_one_simple_command() {
        for text in [
            "git push",
            "git push origin main",
            "FOO=1 git push",
            "git push origin \"$BRANCH\"",
            "git push origin ${x}",
            "git push > out.txt",
        ] {
            assert!(scan(text).expect("parses").single_simple, "`{text}`");
        }
        for text in [
            "git push | head",
            "git push && echo done",
            "git push; echo done",
            "git push &",
            "! git push",
            "time git push",
            "(git push)",
            "{ git push; }",
            "[[ -f x ]]",
            "(( 0 ))",
            "X=1",
            "> out.txt",
            "",
            "git push $(echo main)",
            "git push `echo main`",
            "git push \"$(echo main)\"",
            "git push ${X:-$(echo main)}",
            "echo ${!x}",
            "${!x} foo",
            "git push $((1 + 2))",
            "FOO=$(> out.txt) git push",
            "git push > \"$(> out.txt)\"",
            "git push <<< \"$(> out.txt)\"",
            "git push > >(> out.txt)",
            "git push <(echo x)",
            "f() { git push; }",
        ] {
            assert!(!scan(text).expect("parses").single_simple, "`{text}`");
        }
    }
}
