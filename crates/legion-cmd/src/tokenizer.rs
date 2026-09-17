//! The bounded shell tokenizer (FR-CMD-007, RESEARCH-CMD-bounded-tokenizer).
//!
//! `scan` is a pure, hand-written scanner that finds every command position
//! in a Bash command string, to depth 2, and marks every region it cannot
//! see as [`Opaque`]. It is not a shell interpreter: it never executes
//! anything, reads no file, and performs no I/O of any kind (NFR-CMD-001).
//! It resolves grammar -- quoting, operators, substitutions, heredocs,
//! nesting depth -- which is the scanner's own code. What counts as a
//! wrapper, an interpreter, a shell, or a script extension is tool
//! knowledge, kept in the private [`tables`] module as one declarative
//! table the resolver reads, not as per-tool branches scattered through the
//! grammar (FR-CMD-011's policy-as-data principle, applied to the
//! scanner's own tool tables since `route`'s later policy file is a
//! separate concern this crate does not yet wire up).
//!
//! `route` (a later slice, #1227) decides what an [`Invocation`] or
//! [`Opaque`] region means. This module only resolves structure.

use tables::{Tables, WrapperMatch};

/// Tool knowledge for the bounded tokenizer: every specific name this
/// module treats specially -- inline wrappers, shells, interpreters,
/// script extensions, `eval`, `source`/`.`, and `find` -- plus how each
/// wrapper's own flags are shaped (FR-CMD-007's Behavior section names
/// this exact set).
///
/// Every specific tool name the tokenizer resolves lives here, as data a
/// generic resolver reads, not as a name literal scattered through the
/// grammar -- mirroring the policy-as-data shape FR-CMD-011 requires of
/// `route`'s later policy file. `route` itself (#1227) reads a much
/// larger, externally supplied policy for managed-binary rules; this
/// table is smaller and fixed, because it exists only to resolve grammar
/// positions, never to decide anything about the resolved command. The
/// resolver's own code still names the shell keywords that guard a
/// wrapper's target (`if`, `while`, `until`) directly: those are shell
/// grammar, the scanner's own concern (RESEARCH-CMD-bounded-tokenizer),
/// not tool knowledge.
mod tables {
    /// One inline wrapper's shape: which of its own flags take a
    /// following value (so that value is never mistaken for the wrapped
    /// command), which flag's value is itself a full command line to
    /// parse (e.g. `env -S`), which flags turn the call into a lookup
    /// rather than a wrapper (e.g. `command -v`), how many bare
    /// positional words to skip before the wrapped command (e.g.
    /// `ssh host cmd`, `docker exec container cmd`), and -- for wrappers
    /// that are only sometimes wrappers, like `pnpm` -- which leading
    /// subcommand word gates the unwrap at all.
    pub(super) struct WrapperSpec {
        pub name: &'static str,
        pub value_flags: &'static [&'static str],
        pub command_line_flags: &'static [&'static str],
        pub suppress_flags: &'static [&'static str],
        pub positional_skip: usize,
        pub subcommand_gate: Option<&'static [&'static str]>,
    }

    /// One interpreter's shape: which flags carry an inline program as
    /// their value (`-c`, `-e`, `-r`, ...), and whether -- like `awk` --
    /// its program is a bare positional argument instead of behind a
    /// flag.
    pub(super) struct InterpreterSpec {
        pub inline_flags: &'static [&'static str],
        pub first_positional_is_body: bool,
    }

    /// The result of matching a wrapper's own leading flags/subcommand
    /// against the words that follow it.
    pub(super) enum WrapperMatch<'a> {
        /// The wrapper's own flags mark this as a lookup, not a wrapper
        /// (e.g. `command -v`): nothing after it is invoked.
        Suppressed,
        /// The wrapper's subcommand gate did not match (e.g. bare `pnpm`
        /// without `exec`/`dlx`), or no word remained after its flags:
        /// the wrapper is an ordinary command, not a wrapper here.
        NoTarget,
        /// A flag's value is itself a full command line (e.g. `env -S`).
        CommandLine { line_word: Option<&'a super::Word> },
        /// The remaining words, starting at the wrapped command.
        Target(&'a [super::Word]),
    }

    pub(super) struct Tables {
        wrappers: &'static [WrapperSpec],
        shells: &'static [&'static str],
        interpreters: &'static [(&'static str, InterpreterSpec)],
        script_extensions: &'static [&'static str],
        /// Names that make the resolver treat their whole argument list as
        /// an unparsed `Opaque::Eval` region (FR-CMD-007).
        eval_names: &'static [&'static str],
        /// Names that make the resolver treat their argument as an
        /// `Opaque::Sourced` file or substitution (FR-CMD-007).
        source_names: &'static [&'static str],
        /// The name whose `-exec`/`-execdir` argument spec the resolver
        /// follows as a nested command (FR-CMD-007's `find` case).
        find_name: &'static str,
    }

    /// The tokenizer's whole tool-knowledge table, built once as a
    /// compile-time constant so [`scan`](super::scan) borrows it instead
    /// of reallocating roughly twenty `Vec`s per call -- RESEARCH-CMD-
    /// bounded-tokenizer measured the scanner at p99 12 microseconds per
    /// command, a budget a per-call allocation ate into for no reason.
    pub(super) static TABLES: Tables = Tables::new();

    impl Tables {
        const fn new() -> Self {
            Self {
                wrappers: &[
                    WrapperSpec {
                        name: "env",
                        value_flags: &["-u", "-C", "-P", "-chdir"],
                        command_line_flags: &["-S"],
                        suppress_flags: &[],
                        positional_skip: 0,
                        subcommand_gate: None,
                    },
                    WrapperSpec {
                        name: "sudo",
                        value_flags: &["-u", "-g", "-h", "-p", "-U"],
                        command_line_flags: &[],
                        suppress_flags: &[],
                        positional_skip: 0,
                        subcommand_gate: None,
                    },
                    WrapperSpec {
                        name: "timeout",
                        value_flags: &["--signal", "-k", "--kill-after"],
                        command_line_flags: &[],
                        suppress_flags: &[],
                        positional_skip: 1,
                        subcommand_gate: None,
                    },
                    WrapperSpec {
                        name: "nice",
                        value_flags: &["-n"],
                        command_line_flags: &[],
                        suppress_flags: &[],
                        positional_skip: 0,
                        subcommand_gate: None,
                    },
                    WrapperSpec {
                        name: "nohup",
                        value_flags: &[],
                        command_line_flags: &[],
                        suppress_flags: &[],
                        positional_skip: 0,
                        subcommand_gate: None,
                    },
                    WrapperSpec {
                        name: "stdbuf",
                        value_flags: &["-i", "-o", "-e"],
                        command_line_flags: &[],
                        suppress_flags: &[],
                        positional_skip: 0,
                        subcommand_gate: None,
                    },
                    WrapperSpec {
                        name: "xargs",
                        value_flags: &["-I", "-n", "-P", "-L", "-s", "-d", "-a", "-E"],
                        command_line_flags: &[],
                        suppress_flags: &[],
                        positional_skip: 0,
                        subcommand_gate: None,
                    },
                    WrapperSpec {
                        name: "command",
                        value_flags: &[],
                        command_line_flags: &[],
                        suppress_flags: &["-v", "-V"],
                        positional_skip: 0,
                        subcommand_gate: None,
                    },
                    WrapperSpec {
                        name: "exec",
                        value_flags: &[],
                        command_line_flags: &[],
                        suppress_flags: &[],
                        positional_skip: 0,
                        subcommand_gate: None,
                    },
                    WrapperSpec {
                        name: "time",
                        value_flags: &[],
                        command_line_flags: &[],
                        suppress_flags: &[],
                        positional_skip: 0,
                        subcommand_gate: None,
                    },
                    WrapperSpec {
                        name: "npx",
                        value_flags: &["-p"],
                        command_line_flags: &[],
                        suppress_flags: &[],
                        positional_skip: 0,
                        subcommand_gate: None,
                    },
                    WrapperSpec {
                        name: "pnpx",
                        value_flags: &[],
                        command_line_flags: &[],
                        suppress_flags: &[],
                        positional_skip: 0,
                        subcommand_gate: None,
                    },
                    WrapperSpec {
                        name: "bunx",
                        value_flags: &[],
                        command_line_flags: &[],
                        suppress_flags: &[],
                        positional_skip: 0,
                        subcommand_gate: None,
                    },
                    // A bare `pnpm <name>` is an ordinary pnpm invocation
                    // (FR-CMD-007); only `pnpm exec`/`pnpm dlx` unwrap.
                    WrapperSpec {
                        name: "pnpm",
                        value_flags: &[],
                        command_line_flags: &[],
                        suppress_flags: &[],
                        positional_skip: 0,
                        subcommand_gate: Some(&["exec", "dlx"]),
                    },
                    WrapperSpec {
                        name: "npm",
                        value_flags: &[],
                        command_line_flags: &[],
                        suppress_flags: &[],
                        positional_skip: 0,
                        subcommand_gate: Some(&["exec"]),
                    },
                    WrapperSpec {
                        name: "yarn",
                        value_flags: &[],
                        command_line_flags: &[],
                        suppress_flags: &[],
                        positional_skip: 0,
                        subcommand_gate: Some(&["exec", "dlx"]),
                    },
                    // Hardening additions (RESEARCH-CMD-bounded-tokenizer
                    // next_step.if_yes).
                    WrapperSpec {
                        name: "watch",
                        value_flags: &["-n", "-d"],
                        command_line_flags: &[],
                        suppress_flags: &[],
                        positional_skip: 0,
                        subcommand_gate: None,
                    },
                    WrapperSpec {
                        name: "ssh",
                        value_flags: &["-p", "-i", "-o", "-l"],
                        command_line_flags: &[],
                        suppress_flags: &[],
                        positional_skip: 1,
                        subcommand_gate: None,
                    },
                    WrapperSpec {
                        name: "parallel",
                        value_flags: &["-j"],
                        command_line_flags: &[],
                        suppress_flags: &[],
                        positional_skip: 0,
                        subcommand_gate: None,
                    },
                    WrapperSpec {
                        name: "setsid",
                        value_flags: &[],
                        command_line_flags: &[],
                        suppress_flags: &[],
                        positional_skip: 0,
                        subcommand_gate: None,
                    },
                    WrapperSpec {
                        name: "docker",
                        value_flags: &[],
                        command_line_flags: &[],
                        suppress_flags: &[],
                        positional_skip: 1,
                        subcommand_gate: Some(&["exec"]),
                    },
                ],
                shells: &["sh", "bash", "zsh", "ksh", "dash"],
                interpreters: &[
                    (
                        "python",
                        InterpreterSpec {
                            inline_flags: &["-c"],
                            first_positional_is_body: false,
                        },
                    ),
                    (
                        "python3",
                        InterpreterSpec {
                            inline_flags: &["-c"],
                            first_positional_is_body: false,
                        },
                    ),
                    (
                        "python2",
                        InterpreterSpec {
                            inline_flags: &["-c"],
                            first_positional_is_body: false,
                        },
                    ),
                    (
                        "node",
                        InterpreterSpec {
                            inline_flags: &["-e", "--eval"],
                            first_positional_is_body: false,
                        },
                    ),
                    (
                        "nodejs",
                        InterpreterSpec {
                            inline_flags: &["-e", "--eval"],
                            first_positional_is_body: false,
                        },
                    ),
                    (
                        "ruby",
                        InterpreterSpec {
                            inline_flags: &["-e"],
                            first_positional_is_body: false,
                        },
                    ),
                    (
                        "perl",
                        InterpreterSpec {
                            inline_flags: &["-e", "-ne", "-pe"],
                            first_positional_is_body: false,
                        },
                    ),
                    (
                        "php",
                        InterpreterSpec {
                            inline_flags: &["-r"],
                            first_positional_is_body: false,
                        },
                    ),
                    (
                        "lua",
                        InterpreterSpec {
                            inline_flags: &["-e"],
                            first_positional_is_body: false,
                        },
                    ),
                    (
                        "awk",
                        InterpreterSpec {
                            inline_flags: &[],
                            first_positional_is_body: true,
                        },
                    ),
                ],
                script_extensions: &[
                    ".sh", ".bash", ".py", ".rb", ".js", ".mjs", ".cjs", ".pl", ".php", ".lua",
                ],
                eval_names: &["eval"],
                source_names: &["source", "."],
                find_name: "find",
            }
        }
    }

    impl Tables {
        pub fn wrapper(&self, binary: &str) -> Option<&WrapperSpec> {
            self.wrappers.iter().find(|w| w.name == binary)
        }

        pub fn shell(&self, binary: &str) -> Option<&'static str> {
            self.shells.iter().find(|s| **s == binary).copied()
        }

        pub fn interpreter(&self, binary: &str) -> Option<&InterpreterSpec> {
            self.interpreters
                .iter()
                .find(|(name, _)| *name == binary)
                .map(|(_, spec)| spec)
        }

        pub fn has_script_extension(&self, binary: &str) -> bool {
            self.script_extensions
                .iter()
                .any(|ext| binary.ends_with(ext))
        }

        pub fn is_eval(&self, binary: &str) -> bool {
            self.eval_names.contains(&binary)
        }

        pub fn is_source(&self, binary: &str) -> bool {
            self.source_names.contains(&binary)
        }

        pub fn is_find(&self, binary: &str) -> bool {
            self.find_name == binary
        }
    }
}

/// How a command position was reached (FR-CMD-007).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    First,
    /// After `|`, `&&`, `||`, `;`, `&`, a newline, or a shell keyword that
    /// starts a new command in a list (`if`, `then`, `do`, ...): both are
    /// list boundaries in the shell grammar, and `Position` has no separate
    /// keyword variant (see the tokenizer's module tests for the fixtures
    /// this covers).
    AfterOperator,
    /// Behind an environment prefix, e.g. `FOO=1 grep`.
    AfterAssignment,
    /// Reached by unwrapping an inline wrapper from the tool table.
    Wrapper,
    /// Inside `sh -c` / `bash -c '<inline>'`.
    InlineShell,
    /// Inside `bash <<'EOF' ... EOF`.
    HeredocShell,
    /// Inside `find -exec` / `-execdir`.
    FindExec,
    FunctionBody,
    /// Inside `$(...)`, backticks, `<(...)`, or `>(...)`.
    Substitution,
}

/// A resolved command invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// Basename, quotes and leading backslash removed.
    pub binary: String,
    pub args: Vec<String>,
    pub position: Position,
    /// 0 = top level; at most 2.
    pub depth: u8,
}

/// A region the scanner cannot see into (FR-CMD-007).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opaque {
    /// `python -c`, `node -e`, an `awk` program body, ...; body kept for
    /// route's search patterns (FR-CMD-007). The scanner does not
    /// interpret the body.
    Interpreter {
        interpreter: String,
        body: String,
    },
    /// `bash script.sh`, `./x.sh`.
    ScriptFile,
    Eval,
    /// A command name built at runtime.
    DynamicCommand,
    /// An interpreter reading its program from stdin or a heredoc.
    StdinScript,
    /// `source` / `.` of a file or substitution.
    Sourced,
    /// A shell alias definition invoked by name (e.g. `git -c
    /// alias.s='!grep -rn foo src' s`). No current path constructs this
    /// variant: an alias expansion is semantic, not grammatical
    /// (RESEARCH-CMD-bounded-tokenizer), so the honest bounded result is
    /// an ordinary invocation with the alias definition as a literal
    /// argument (see the `silent-miss-git-bang-alias` fixture).
    Alias,
    /// A wrapper outside the table. No current path constructs this
    /// variant: the scanner recognizes a wrapper only by table lookup
    /// (FR-CMD-011), so it has no generic rule for "this binary is a
    /// wrapper but is not in the table" to detect one it does not know.
    UnknownWrapper,
    /// Anything beyond depth 2.
    TooDeep,
}

/// The result of scanning one command string.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    pub invocations: Vec<Invocation>,
    pub opaque: Vec<Opaque>,
}

/// Why `scan` could not resolve a command. Each variant names the
/// construct that failed and the byte offset where it started.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScanError {
    #[error("unterminated single quote starting at byte {0}")]
    UnterminatedSingleQuote(usize),

    #[error("unterminated double quote starting at byte {0}")]
    UnterminatedDoubleQuote(usize),

    #[error("unterminated command or process substitution starting at byte {0}")]
    UnterminatedSubstitution(usize),

    #[error("unbalanced parenthesis starting at byte {0}")]
    UnbalancedParen(usize),

    #[error("unterminated heredoc '{delimiter}' starting at byte {offset}")]
    UnterminatedHeredoc { delimiter: String, offset: usize },

    #[error("dangling escape character at byte {0}")]
    DanglingEscape(usize),
}

/// Maximum recursion depth `scan` will resolve into (FR-CMD-007): a new
/// text region entered beyond this depth is recorded as [`Opaque::TooDeep`]
/// rather than parsed.
const MAX_DEPTH: u8 = 2;

/// Resolves every command position in `command`, to depth 2, marking every
/// region it cannot see as [`Opaque`] (FR-CMD-007).
///
/// Pure: no filesystem, network, database, environment, or process access
/// (NFR-CMD-001). A malformed command returns [`ScanError`], never a
/// partial [`Scan`].
pub fn scan(command: &str) -> Result<Scan, ScanError> {
    let chars: Vec<char> = command.chars().collect();
    let byte_offsets: Vec<usize> = char_byte_offsets(command);
    let mut ctx = ScanCtx {
        chars: &chars,
        byte_offsets: &byte_offsets,
        end_byte: command.len(),
        tables: &tables::TABLES,
        invocations: Vec::new(),
        opaque: Vec::new(),
    };
    let mut parser = ListParser::new(&mut ctx, 0, chars.len(), 0);
    parser.run()?;
    Ok(Scan {
        invocations: ctx.invocations,
        opaque: ctx.opaque,
    })
}

/// Byte offset of each char index, plus one trailing entry for the string's
/// total byte length, so a caller can always look up an end-of-range offset
/// without a bounds check at the input's end.
fn char_byte_offsets(s: &str) -> Vec<usize> {
    let mut offsets: Vec<usize> = s.char_indices().map(|(i, _)| i).collect();
    offsets.push(s.len());
    offsets
}

/// Shared, append-only state for one `scan` call: the char buffer, the
/// tool tables, and the invocations/opaque regions collected so far.
struct ScanCtx<'a> {
    chars: &'a [char],
    byte_offsets: &'a [usize],
    end_byte: usize,
    tables: &'a Tables,
    invocations: Vec<Invocation>,
    opaque: Vec<Opaque>,
}

impl ScanCtx<'_> {
    fn byte_at(&self, char_index: usize) -> usize {
        self.byte_offsets
            .get(char_index)
            .copied()
            .unwrap_or(self.end_byte)
    }
}

/// One shell "word" as read by the scanner: its literal text (quotes
/// stripped, escapes resolved) plus whether it looked like an assignment
/// prefix (`NAME=value`) and whether it still contains an unresolved `$`
/// or backtick expansion after quote-removal (a dynamic fragment route
/// cannot see through, e.g. `"$@"` or `$SEARCH`).
#[derive(Debug, Clone, Default)]
struct Word {
    text: String,
    is_assignment: bool,
    has_dynamic_fragment: bool,
    leading_backslash_literal: bool,
}

/// Parses a sequence of simple commands separated by operators (`|`, `&&`,
/// `||`, `;`, `&`, newline) or shell keywords, over `chars[start..end]`, at
/// a fixed recursion `depth`. Each simple command found is classified and
/// appended to the shared [`ScanCtx`].
struct ListParser<'ctx, 'a> {
    ctx: &'ctx mut ScanCtx<'a>,
    pos: usize,
    end: usize,
    depth: u8,
    pending_heredocs: Vec<PendingHeredoc>,
}

/// One resolved simple command: its words plus the [`Position`] it was
/// reached at (assignments already split out from `words`).
struct SimpleCommand {
    words: Vec<Word>,
    position: Position,
}

impl<'ctx, 'a> ListParser<'ctx, 'a> {
    fn new(ctx: &'ctx mut ScanCtx<'a>, start: usize, end: usize, depth: u8) -> Self {
        Self {
            ctx,
            pos: start,
            end,
            depth,
            pending_heredocs: Vec::new(),
        }
    }

    fn peek(&self) -> Option<char> {
        if self.pos < self.end {
            self.ctx.chars.get(self.pos).copied()
        } else {
            None
        }
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        let idx = self.pos + offset;
        if idx < self.end {
            self.ctx.chars.get(idx).copied()
        } else {
            None
        }
    }

    fn byte_here(&self) -> usize {
        self.ctx.byte_at(self.pos)
    }

    /// True if the chars starting at `self.pos` are a run of one or more
    /// digits immediately followed by `<` or `>` (a redirect's fd number,
    /// e.g. the `2` in `2>&1`).
    fn at_fd_redirect_prefix(&self) -> bool {
        let mut i = self.pos;
        let mut saw_digit = false;
        while matches!(self.ctx.chars.get(i), Some(c) if c.is_ascii_digit()) {
            i += 1;
            saw_digit = true;
        }
        saw_digit && matches!(self.ctx.chars.get(i), Some('<') | Some('>'))
    }

    /// Drives the whole list: repeatedly skips separators, reads one
    /// simple command, classifies it, then looks at what follows to decide
    /// the next command's [`Position`]. Used at the top level ([`scan`])
    /// and for subshell groups, where each command's own position must be
    /// derived from what precedes it rather than forced to a single
    /// label -- see [`Self::run_as`] for the forced-label case.
    fn run(&mut self) -> Result<(), ScanError> {
        self.run_with(None)
    }

    /// Called at every exit point of [`Self::run`]/[`Self::run_as`]: a
    /// heredoc operator queued but never reached its terminating line (the
    /// input ran out first) is a malformed command, not a silently
    /// dropped one.
    fn finish(&mut self) -> Result<(), ScanError> {
        if let Some(heredoc) = self.pending_heredocs.first() {
            return Err(ScanError::UnterminatedHeredoc {
                delimiter: heredoc.delimiter.clone(),
                offset: self.byte_here(),
            });
        }
        Ok(())
    }

    /// Whitespace and comments are insignificant between words; they never
    /// start or end a command by themselves.
    fn skip_insignificant(&mut self) -> Result<(), ScanError> {
        loop {
            match self.peek() {
                Some(c) if c == ' ' || c == '\t' || c == '\r' => {
                    self.pos += 1;
                }
                Some('#') if self.at_word_boundary_before_hash() => {
                    while let Some(c) = self.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    fn at_word_boundary_before_hash(&self) -> bool {
        // A `#` only starts a comment at the start of a word, i.e.
        // preceded by nothing or whitespace/operator, not mid-word
        // (`foo#bar` is one word, not a comment).
        match self.pos.checked_sub(1).and_then(|i| self.ctx.chars.get(i)) {
            None => true,
            Some(c) => c.is_whitespace() || matches!(c, '|' | '&' | ';' | '(' | '\n'),
        }
    }

    /// True if the parser sits at a keyword (`then`, `fi`, `done`, `esac`,
    /// `elif`, `else`, `}`) that closes the caller's construct rather than
    /// starting a new simple command here.
    fn at_closing_keyword(&self) -> bool {
        matches!(
            self.peek_keyword().as_deref(),
            Some("then" | "fi" | "done" | "esac" | "elif" | "else" | "}")
        )
    }

    /// Peeks the next bare word without consuming it, for keyword
    /// recognition. Returns `None` if the next token is not a plain
    /// unquoted word (e.g. it starts with a quote or `$`).
    fn peek_keyword(&self) -> Option<String> {
        let mut i = self.pos;
        let mut s = String::new();
        while i < self.end {
            let c = *self.ctx.chars.get(i)?;
            if c.is_whitespace() || matches!(c, '|' | '&' | ';' | '(' | ')' | '<' | '>') {
                break;
            }
            s.push(c);
            i += 1;
        }
        if s.is_empty() { None } else { Some(s) }
    }

    /// Consumes one operator or keyword boundary and returns the
    /// [`Position`] the following command should carry, or `None` if the
    /// list has ended.
    fn consume_separator(&mut self) -> Result<Option<Position>, ScanError> {
        match self.peek() {
            Some('\n') => {
                // A real newline ends the current logical line: this is
                // where any heredoc bodies queued by commands on that line
                // are read (RESEARCH-CMD-bounded-tokenizer), never at the
                // first operator inside the line (a pipe before the
                // heredoc's own line end must not truncate the line).
                if self.pending_heredocs.is_empty() {
                    self.pos += 1;
                } else {
                    self.drain_pending_heredocs()?;
                }
                Ok(Some(Position::AfterOperator))
            }
            Some(';') => {
                self.pos += 1;
                if self.peek() == Some(';') {
                    self.pos += 1;
                }
                Ok(Some(Position::AfterOperator))
            }
            Some('&') => {
                self.pos += 1;
                if self.peek() == Some('&') {
                    self.pos += 1;
                }
                Ok(Some(Position::AfterOperator))
            }
            Some('|') => {
                self.pos += 1;
                if self.peek() == Some('|') {
                    self.pos += 1;
                }
                Ok(Some(Position::AfterOperator))
            }
            Some('(') => {
                // Subshell group: recurse at the same depth (a subshell is
                // not a new text region route can't see; it is the same
                // command string, just grouped).
                let start_paren = self.byte_here();
                self.pos += 1;
                let group_end = find_balanced_paren(self.ctx, self.pos, self.end, start_paren)?;
                {
                    let mut inner = ListParser::new(self.ctx, self.pos, group_end, self.depth);
                    inner.run()?;
                }
                self.pos = group_end + 1;
                Ok(Some(Position::AfterOperator))
            }
            Some(_) | None => {
                if let Some(word) = self.peek_keyword()
                    && matches!(word.as_str(), "then" | "do" | "else" | "elif" | "{" | "in")
                {
                    self.pos += word.chars().count();
                    return Ok(Some(Position::AfterOperator));
                }
                Ok(None)
            }
        }
    }

    /// Reads one simple command: optional `NAME=value` assignment words,
    /// then the command word and its arguments, up to the next operator or
    /// keyword boundary. Handles the shell keywords that introduce a
    /// command (`if`, `while`, `until`, `for ... in ...; do`) by consuming
    /// them and recursing into the guarded command.
    fn read_simple_command(
        &mut self,
        position: Position,
    ) -> Result<Option<SimpleCommand>, ScanError> {
        // Transparent list-boundary keywords: they mark where a new
        // command begins (the same grammatical role as `;`) but carry no
        // invocation of their own. This is the path `do`/`then`/`else`/
        // `elif` take when `run`'s own loop starts reading the next
        // command right after one of these (e.g. a `for ... ; do <cmd>`
        // body), since `consume_separator` only recognizes them when they
        // sit between two already-read commands.
        loop {
            match self.peek_keyword() {
                Some(kw)
                    if matches!(
                        kw.as_str(),
                        "then" | "do" | "else" | "elif" | "if" | "while" | "until"
                    ) =>
                {
                    self.pos += kw.chars().count();
                    self.skip_insignificant()?;
                }
                Some(kw) if kw == "{" => {
                    self.pos += 1;
                    self.skip_insignificant()?;
                }
                _ => break,
            }
        }
        if let Some(word) = self.peek_keyword() {
            match word.as_str() {
                "for" => {
                    self.pos += word.chars().count();
                    // Skip the loop variable and, if present, `in <list>`;
                    // none of these are invocations.
                    self.skip_insignificant()?;
                    let _ = self.peek_keyword();
                    self.skip_word_no_classify()?;
                    self.skip_insignificant()?;
                    if self.peek_keyword().as_deref() == Some("in") {
                        self.skip_word_no_classify()?;
                        loop {
                            self.skip_insignificant()?;
                            match self.peek() {
                                Some(';') | Some('\n') | None => break,
                                _ => {
                                    if self.peek_keyword().as_deref() == Some("do") {
                                        break;
                                    }
                                    self.skip_word_no_classify()?;
                                }
                            }
                        }
                    }
                    return Ok(None);
                }
                "function" => {
                    self.pos += word.chars().count();
                    self.skip_insignificant()?;
                    self.skip_word_no_classify()?;
                    self.skip_insignificant()?;
                    // An optional `()` after the name, e.g. `function g() { ... }`.
                    if self.peek() == Some('(') && self.peek_at(1) == Some(')') {
                        self.pos += 2;
                        self.skip_insignificant()?;
                    }
                    if self.peek_keyword().as_deref() == Some("{") {
                        self.pos += 1;
                    }
                    self.parse_function_body()?;
                    return Ok(None);
                }
                "case" => {
                    self.pos += word.chars().count();
                    self.parse_case()?;
                    return Ok(None);
                }
                "select" => {
                    self.pos += word.chars().count();
                    self.skip_insignificant()?;
                    self.skip_word_no_classify()?;
                    return Ok(None);
                }
                "{" => {
                    self.pos += 1;
                    return Ok(None);
                }
                "!" => {
                    self.pos += 1;
                    return Ok(None);
                }
                _ => {}
            }
        }

        // `NAME() { ... }` function definition, POSIX form.
        if let Some(after) = self.try_match_posix_function_def()? {
            self.pos = after;
            return Ok(None);
        }

        let mut words = Vec::new();
        loop {
            self.skip_horizontal_space();
            match self.peek() {
                None => break,
                Some(c) if c == '\n' || matches!(c, '|' | '&' | ';' | '(' | ')') => {
                    break;
                }
                _ => {}
            }
            if self.at_closing_keyword() {
                break;
            }
            if let Some(word) = self.read_word(&words)? {
                words.push(word);
            }
        }
        if words.is_empty() {
            return Ok(None);
        }
        Ok(Some(SimpleCommand { words, position }))
    }

    fn skip_horizontal_space(&mut self) {
        while matches!(self.peek(), Some(' ') | Some('\t') | Some('\r')) {
            self.pos += 1;
        }
    }

    /// True when the next character ends a simple command outright (end of
    /// input, a newline, or a `;`/`|`/`&` operator) rather than starting
    /// another word. Shared by every site that needs to tell "nothing more
    /// to read here" from "there is another word."
    fn at_command_boundary(&self) -> bool {
        matches!(
            self.peek(),
            None | Some('\n') | Some(';') | Some('|') | Some('&')
        )
    }

    /// Reads and discards one word (a loop variable, a `for ... in` list
    /// item) without classifying it as an invocation, but still resolving
    /// any substitutions it contains so nested invocations are not missed.
    fn skip_word_no_classify(&mut self) -> Result<(), ScanError> {
        self.skip_horizontal_space();
        if self.at_command_boundary() {
            return Ok(());
        }
        self.read_word(&[])?;
        Ok(())
    }

    /// Reads one shell word starting at `self.pos`, resolving quotes,
    /// escapes, and substitutions. Recognized redirects (`>`, `<`, `>>`,
    /// `2>&1`, `<<`, `<<-`, process substitution) are consumed here too,
    /// since they can appear mid-word-list without ending the command.
    fn read_word(&mut self, words_so_far: &[Word]) -> Result<Option<Word>, ScanError> {
        // A bare fd number immediately before a redirect (`2>&1`,
        // `2>/dev/null`) is part of the redirect operator, not a
        // positional argument, and must never surface as a stray word.
        if self.at_fd_redirect_prefix() {
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        let mut word = Word::default();
        let mut produced = false;
        loop {
            match self.peek() {
                None => break,
                Some(c) if c.is_whitespace() => break,
                Some('|') | Some('&') | Some(';') | Some('(') | Some(')') => break,
                Some('\'') => {
                    self.read_single_quoted(&mut word)?;
                    produced = true;
                }
                Some('"') => {
                    self.read_double_quoted(&mut word)?;
                    produced = true;
                }
                Some('\\') => {
                    if self.read_escape(&mut word)? {
                        produced = true;
                    }
                }
                Some('$') => {
                    self.read_dollar(&mut word)?;
                    produced = true;
                }
                Some('`') => {
                    self.read_backtick(&mut word)?;
                    produced = true;
                }
                Some('<') if self.peek_at(1) == Some('(') => {
                    self.read_process_substitution(&mut word)?;
                    produced = true;
                }
                Some('>') if self.peek_at(1) == Some('(') => {
                    self.read_process_substitution(&mut word)?;
                    produced = true;
                }
                Some('<') if self.peek_at(1) == Some('<') && self.peek_at(2) == Some('<') => {
                    // A herestring (`<<<`) feeds one word as stdin
                    // content; it is a redirect, not a heredoc body, so it
                    // is consumed like `read_redirect` (its word is still
                    // scanned for substitutions, just not kept as argv).
                    if produced {
                        break;
                    }
                    self.pos += 3;
                    self.skip_horizontal_space();
                    if !self.at_command_boundary() {
                        let _ = self.read_word(&[])?;
                    }
                    return Ok(None);
                }
                Some('<') if self.peek_at(1) == Some('<') => {
                    // A heredoc operator ends the word that precedes it
                    // (if any) and is never itself part of a word.
                    if produced {
                        break;
                    }
                    self.read_heredoc_operator(words_so_far)?;
                    return Ok(None);
                }
                Some('<') | Some('>') => {
                    if produced {
                        break;
                    }
                    self.read_redirect()?;
                    return Ok(None);
                }
                Some(c) => {
                    word.text.push(c);
                    self.pos += 1;
                    produced = true;
                }
            }
        }
        if !produced {
            return Ok(None);
        }
        if word.text.starts_with('\\') && word.text.len() > 1 {
            word.leading_backslash_literal = true;
        }
        self.detect_assignment(&mut word);
        Ok(Some(word))
    }

    fn read_single_quoted(&mut self, word: &mut Word) -> Result<(), ScanError> {
        let open = self.byte_here();
        self.pos += 1; // opening quote
        loop {
            match self.peek() {
                None => return Err(ScanError::UnterminatedSingleQuote(open)),
                Some('\'') => {
                    self.pos += 1;
                    break;
                }
                Some(c) => {
                    word.text.push(c);
                    self.pos += 1;
                }
            }
        }
        Ok(())
    }

    fn read_double_quoted(&mut self, word: &mut Word) -> Result<(), ScanError> {
        let open = self.byte_here();
        self.pos += 1; // opening quote
        loop {
            match self.peek() {
                None => return Err(ScanError::UnterminatedDoubleQuote(open)),
                Some('"') => {
                    self.pos += 1;
                    break;
                }
                Some('\\') => match self.peek_at(1) {
                    Some(c) if matches!(c, '"' | '\\' | '$' | '`' | '\n') => {
                        self.pos += 2;
                        if c != '\n' {
                            word.text.push(c);
                        }
                    }
                    _ => {
                        word.text.push('\\');
                        self.pos += 1;
                    }
                },
                Some('$') => {
                    self.read_dollar(word)?;
                }
                Some('`') => {
                    self.read_backtick(word)?;
                }
                Some(c) => {
                    word.text.push(c);
                    self.pos += 1;
                }
            }
        }
        Ok(())
    }

    /// Reads a `\X` escape. Returns whether it appended a character to
    /// `word`: a line continuation (`\` followed by a newline) consumes
    /// both characters but appends nothing, so it must not by itself mark
    /// the caller's word as non-empty (otherwise a bare line continuation
    /// followed by whitespace would surface as a stray empty-string arg).
    fn read_escape(&mut self, word: &mut Word) -> Result<bool, ScanError> {
        let open = self.byte_here();
        self.pos += 1;
        match self.peek() {
            None => Err(ScanError::DanglingEscape(open)),
            Some('\n') => {
                // Line continuation: consumes both chars, adds nothing.
                self.pos += 1;
                Ok(false)
            }
            Some(c) => {
                if word.text.is_empty() {
                    word.leading_backslash_literal = true;
                }
                word.text.push(c);
                self.pos += 1;
                Ok(true)
            }
        }
    }

    /// `$name`, `${name}`, `$(...)` (command substitution), or a bare `$`.
    fn read_dollar(&mut self, word: &mut Word) -> Result<(), ScanError> {
        if self.peek_at(1) == Some('(') && self.peek_at(2) == Some('(') {
            // Arithmetic expansion `$((...))`: not a command substitution
            // at all (RESEARCH-CMD-bounded-tokenizer fixture
            // `arithmetic-not-subst`). Its content is never shell grammar,
            // so it is never recursed into.
            let open = self.byte_here();
            self.pos += 2;
            let inner_start = self.pos;
            let inner_end = find_substitution_end(self.ctx, inner_start, self.end, open)?;
            word.has_dynamic_fragment = true;
            word.text.push_str("$((...))");
            self.pos = inner_end + 1;
            return Ok(());
        }
        if self.peek_at(1) == Some('(') {
            let open = self.byte_here();
            self.pos += 2;
            let inner_start = self.pos;
            let inner_end = find_substitution_end(self.ctx, inner_start, self.end, open)?;
            self.scan_nested(inner_start, inner_end)?;
            word.has_dynamic_fragment = true;
            word.text.push_str("$(...)");
            self.pos = inner_end + 1;
            return Ok(());
        }
        // `$name`, `${...}`, `$@`, `$1`, or a bare `$`.
        word.has_dynamic_fragment = true;
        word.text.push('$');
        self.pos += 1;
        if self.peek() == Some('{') {
            let mut depth = 1usize;
            self.pos += 1;
            while depth > 0 {
                match self.peek() {
                    None => break,
                    Some('{') => {
                        depth += 1;
                        self.pos += 1;
                    }
                    Some('}') => {
                        depth -= 1;
                        self.pos += 1;
                    }
                    Some(c) => {
                        word.text.push(c);
                        self.pos += 1;
                    }
                }
            }
        } else {
            while let Some(c) = self.peek() {
                if c.is_alphanumeric() || c == '_' {
                    word.text.push(c);
                    self.pos += 1;
                } else {
                    break;
                }
            }
        }
        Ok(())
    }

    fn read_backtick(&mut self, word: &mut Word) -> Result<(), ScanError> {
        let open = self.byte_here();
        self.pos += 1;
        let inner_start = self.pos;
        loop {
            match self.peek() {
                None => return Err(ScanError::UnterminatedSubstitution(open)),
                Some('`') => break,
                Some('\\') if matches!(self.peek_at(1), Some('`') | Some('\\')) => {
                    self.pos += 2;
                }
                _ => self.pos += 1,
            }
        }
        let inner_end = self.pos;
        self.scan_nested(inner_start, inner_end)?;
        word.has_dynamic_fragment = true;
        word.text.push_str("`...`");
        self.pos += 1; // closing backtick
        Ok(())
    }

    fn read_process_substitution(&mut self, word: &mut Word) -> Result<(), ScanError> {
        let open = self.byte_here();
        self.pos += 2; // `<(` or `>(`
        let inner_start = self.pos;
        let inner_end = find_substitution_end(self.ctx, inner_start, self.end, open)?;
        self.scan_nested(inner_start, inner_end)?;
        word.has_dynamic_fragment = true;
        word.text.push_str("<(...)");
        self.pos = inner_end + 1;
        Ok(())
    }

    /// Recurses `scan` into a substitution/backtick's inner text, at
    /// `depth + 1`, unless that would exceed [`MAX_DEPTH`], in which case
    /// it records [`Opaque::TooDeep`] and does not parse the region.
    fn scan_nested(&mut self, start: usize, end: usize) -> Result<(), ScanError> {
        if self.depth >= MAX_DEPTH {
            self.ctx.opaque.push(Opaque::TooDeep);
            return Ok(());
        }
        let mut inner = ListParser::new(self.ctx, start, end, self.depth + 1);
        inner.run_as(Position::Substitution)
    }

    fn read_redirect(&mut self) -> Result<(), ScanError> {
        // Skip fd-less `<`, `>`, `>>`, `<&`, `>&`. The target word (a
        // filename) is not part of the invoked binary's argv.
        self.pos += 1;
        if self.peek() == Some('>') || self.peek() == Some('&') {
            self.pos += 1;
        }
        self.skip_horizontal_space();
        if !self.at_command_boundary() {
            let _ = self.read_word(&[])?;
        }
        Ok(())
    }

    /// Consumes a `<<`, `<<-`, or `<<~` operator and its delimiter word,
    /// then queues the heredoc body to be consumed once the current
    /// logical line ends (see [`Self::drain_pending_heredocs`]). This
    /// method only records the delimiter; body consumption happens in
    /// `run` after the line's remaining operators are parsed, matching
    /// real shell behavior.
    fn read_heredoc_operator(&mut self, words_so_far: &[Word]) -> Result<(), ScanError> {
        self.pos += 2; // `<<`
        let strip_tabs = if self.peek() == Some('-') {
            self.pos += 1;
            true
        } else {
            false
        };
        if self.peek() == Some('~') {
            self.pos += 1;
        }
        self.skip_horizontal_space();
        let mut delim_word = Word::default();
        match self.peek() {
            Some('\'') => self.read_single_quoted(&mut delim_word)?,
            Some('"') => self.read_double_quoted(&mut delim_word)?,
            _ => {
                while let Some(c) = self.peek() {
                    if c.is_whitespace() || matches!(c, '|' | '&' | ';') {
                        break;
                    }
                    delim_word.text.push(c);
                    self.pos += 1;
                }
            }
        }
        let owner_binary = words_so_far
            .iter()
            .find(|w| !w.is_assignment)
            .map(|w| basename(&w.text));
        let shell_owner = owner_binary
            .as_deref()
            .is_some_and(|b| self.ctx.tables.shell(b).is_some());
        self.pending_heredocs.push(PendingHeredoc {
            delimiter: delim_word.text,
            strip_tabs,
            shell_owner,
        });
        Ok(())
    }

    fn try_match_posix_function_def(&mut self) -> Result<Option<usize>, ScanError> {
        let mut i = self.pos;
        let mut name = String::new();
        while i < self.end {
            let Some(&c) = self.ctx.chars.get(i) else {
                break;
            };
            if c.is_alphanumeric() || c == '_' || c == '-' || c == '.' {
                name.push(c);
                i += 1;
            } else {
                break;
            }
        }
        if name.is_empty() {
            return Ok(None);
        }
        if self.ctx.chars.get(i) != Some(&'(') || self.ctx.chars.get(i + 1) != Some(&')') {
            return Ok(None);
        }
        i += 2;
        let saved = self.pos;
        self.pos = i;
        self.skip_insignificant()?;
        if self.peek_keyword().as_deref() != Some("{") {
            self.pos = saved;
            return Ok(None);
        }
        self.pos += 1;
        self.parse_function_body()?;
        Ok(Some(self.pos))
    }

    fn parse_function_body(&mut self) -> Result<(), ScanError> {
        let open = self.byte_here();
        let body_end = find_balanced_brace(self.ctx, self.pos, self.end, open)?;
        if self.depth >= MAX_DEPTH {
            self.ctx.opaque.push(Opaque::TooDeep);
        } else {
            let mut inner = ListParser::new(self.ctx, self.pos, body_end, self.depth + 1);
            inner.run_as(Position::FunctionBody)?;
        }
        self.pos = body_end + 1;
        Ok(())
    }

    fn parse_case(&mut self) -> Result<(), ScanError> {
        self.skip_insignificant()?;
        self.skip_word_no_classify()?; // the case subject
        self.skip_insignificant()?;
        if self.peek_keyword().as_deref() == Some("in") {
            self.pos += 2;
        }
        loop {
            self.skip_insignificant()?;
            if self.peek_keyword().as_deref() == Some("esac") {
                self.pos += 4;
                break;
            }
            if self.pos >= self.end {
                break;
            }
            // Pattern list up to `)`.
            while self.peek().is_some() && self.peek() != Some(')') {
                self.pos += 1;
            }
            if self.peek() == Some(')') {
                self.pos += 1;
            }
            // Body up to `;;` or `esac`.
            loop {
                self.skip_insignificant()?;
                if self.peek() == Some(';') && self.peek_at(1) == Some(';') {
                    self.pos += 2;
                    break;
                }
                if self.peek_keyword().as_deref() == Some("esac") || self.pos >= self.end {
                    break;
                }
                let command = self.read_simple_command(Position::AfterOperator)?;
                if let Some(command) = command {
                    self.classify(command)?;
                }
                self.skip_insignificant()?;
                match self.peek() {
                    Some(';') if self.peek_at(1) != Some(';') => {
                        self.pos += 1;
                    }
                    Some('\n') | Some('|') | Some('&') if self.consume_separator()?.is_none() => {
                        break;
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// Drains queued heredoc bodies once the current logical line ends.
    /// Each body is read as raw lines up to (and excluding) a line that
    /// exactly matches its delimiter. A heredoc whose owning command is a
    /// recognized shell is parsed as [`Position::HeredocShell`]; any other
    /// heredoc body is skipped as literal text (this is the fix that
    /// resolves the `$(cat <<'EOF' ... EOF)` idiom, RESEARCH-CMD-bounded-
    /// tokenizer).
    fn drain_pending_heredocs(&mut self) -> Result<(), ScanError> {
        let pending = std::mem::take(&mut self.pending_heredocs);
        if self.peek() == Some('\n') {
            self.pos += 1;
        }
        for heredoc in pending {
            let body_start = self.pos;
            let open = self.byte_here();
            loop {
                if self.pos >= self.end {
                    return Err(ScanError::UnterminatedHeredoc {
                        delimiter: heredoc.delimiter.clone(),
                        offset: open,
                    });
                }
                let line_start = self.pos;
                let mut line = String::new();
                while let Some(c) = self.peek() {
                    if c == '\n' {
                        break;
                    }
                    line.push(c);
                    self.pos += 1;
                }
                if heredoc_line_closes(&line, &heredoc.delimiter, heredoc.strip_tabs) {
                    let body_end = line_start;
                    if heredoc.shell_owner {
                        if self.depth >= MAX_DEPTH {
                            self.ctx.opaque.push(Opaque::TooDeep);
                        } else {
                            let mut inner =
                                ListParser::new(self.ctx, body_start, body_end, self.depth + 1);
                            inner.run_as(Position::HeredocShell)?;
                        }
                    }
                    if self.pos < self.end {
                        self.pos += 1; // trailing newline after delimiter
                    }
                    break;
                }
                if self.pos < self.end {
                    self.pos += 1; // the newline ending this body line
                }
            }
        }
        Ok(())
    }

    fn detect_assignment(&self, word: &mut Word) {
        if word.leading_backslash_literal {
            return;
        }
        let bytes: Vec<char> = word.text.chars().collect();
        if bytes.is_empty() || !(bytes[0].is_alphabetic() || bytes[0] == '_') {
            return;
        }
        let mut i = 1;
        while i < bytes.len() && (bytes[i].is_alphanumeric() || bytes[i] == '_') {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == '=' {
            word.is_assignment = true;
        }
    }

    /// Classifies one fully-read [`SimpleCommand`]: splits off leading
    /// assignments, resolves wrapper unwrapping, and emits either an
    /// [`Invocation`] or an [`Opaque`] region. The position derives from
    /// assignment/operator context, per [`Self::classify_with`].
    fn classify(&mut self, command: SimpleCommand) -> Result<(), ScanError> {
        self.classify_with(command, None)
    }

    /// Resolves an already-assignment-stripped word list into an
    /// invocation or opaque region, following wrapper chains.
    fn resolve_from(&mut self, words: &[Word], position: Position) -> Result<(), ScanError> {
        let head = &words[0];
        if head.has_dynamic_fragment || head.text.is_empty() {
            self.ctx.opaque.push(Opaque::DynamicCommand);
            return Ok(());
        }
        let binary = basename(&head.text);

        if self.ctx.tables.is_eval(&binary) {
            self.ctx.opaque.push(Opaque::Eval);
            return Ok(());
        }
        if self.ctx.tables.is_source(&binary) {
            self.ctx.opaque.push(Opaque::Sourced);
            return Ok(());
        }

        if let Some(spec) = self.ctx.tables.wrapper(&binary) {
            match wrapper_match(spec, &words[1..]) {
                WrapperMatch::Suppressed | WrapperMatch::NoTarget => {
                    self.emit_invocation(binary, &words[1..], position);
                    return Ok(());
                }
                WrapperMatch::CommandLine { line_word } => {
                    if let Some(line) = line_word {
                        self.scan_command_line_as(&line.text, Position::Wrapper)?;
                    } else {
                        self.ctx.opaque.push(Opaque::DynamicCommand);
                    }
                    return Ok(());
                }
                WrapperMatch::Target(rest) => {
                    if rest.is_empty() {
                        self.emit_invocation(binary, &[], position);
                        return Ok(());
                    }
                    // `time` before a shell keyword (RESEARCH-CMD-bounded-
                    // tokenizer hardening: "time as a wrapper before a
                    // keyword"): the keyword itself is not a command, so
                    // skip it rather than treat it as the wrapped binary.
                    if matches!(rest[0].text.as_str(), "if" | "while" | "until") {
                        return self.resolve_from(&rest[1..], Position::Wrapper);
                    }
                    return self.resolve_from(rest, Position::Wrapper);
                }
            }
        }

        if self.ctx.tables.shell(&binary).is_some() {
            if let Some(script_arg) = find_flag_value(&words[1..], "-c") {
                self.scan_command_line_as(&script_arg.text, Position::InlineShell)?;
                return Ok(());
            }
            if words[1..].iter().any(|w| !w.text.starts_with('-')) {
                // A shell given a plain file argument runs it as a script
                // file it cannot see into.
                self.ctx.opaque.push(Opaque::ScriptFile);
                return Ok(());
            }
            self.emit_invocation(binary, &words[1..], position);
            return Ok(());
        }

        if let Some(interp) = self.ctx.tables.interpreter(&binary) {
            if let Some(body_word) = interp
                .inline_flags
                .iter()
                .find_map(|flag| find_flag_value(&words[1..], flag))
            {
                self.ctx.opaque.push(Opaque::Interpreter {
                    interpreter: binary,
                    body: body_word.text.clone(),
                });
                return Ok(());
            }
            if interp.first_positional_is_body
                && let Some(body_word) = words[1..].iter().find(|w| !w.text.starts_with('-'))
            {
                self.ctx.opaque.push(Opaque::Interpreter {
                    interpreter: binary,
                    body: body_word.text.clone(),
                });
                return Ok(());
            }
            if words[1..].iter().any(|w| !w.text.starts_with('-')) {
                self.ctx.opaque.push(Opaque::ScriptFile);
                return Ok(());
            }
            // No inline flag, no script argument: reads its program from
            // stdin.
            self.ctx.opaque.push(Opaque::StdinScript);
            return Ok(());
        }

        let script_extension_hit = self.ctx.tables.has_script_extension(&binary);
        if script_extension_hit {
            self.ctx.opaque.push(Opaque::ScriptFile);
            return Ok(());
        }

        if self.ctx.tables.is_find(&binary)
            && let Some(spec_words) = find_exec_spec(&words[1..])
        {
            self.emit_invocation(binary, &words[1..], position);
            if !spec_words.is_empty() {
                if self.depth >= MAX_DEPTH {
                    self.ctx.opaque.push(Opaque::TooDeep);
                } else {
                    self.depth += 1;
                    let result = self.resolve_from(spec_words, Position::FindExec);
                    self.depth -= 1;
                    result?;
                }
            }
            return Ok(());
        }

        self.emit_invocation(binary, &words[1..], position);
        Ok(())
    }

    fn emit_invocation(&mut self, binary: String, args: &[Word], position: Position) {
        self.ctx.invocations.push(Invocation {
            binary,
            args: args.iter().map(|w| w.text.clone()).collect(),
            position,
            depth: self.depth,
        });
    }

    /// Parses `script` as a new command list at `depth + 1`, labeling
    /// every invocation found with `label`: `sh -c` / `bash -c` labels it
    /// `InlineShell`, `env -S` labels it `Wrapper` since `env` (not a
    /// shell) is doing the unwrapping. Both are a whitespace/quote-split
    /// command line embedded in one argument, structurally the same shape.
    fn scan_command_line_as(&mut self, script: &str, label: Position) -> Result<(), ScanError> {
        if self.depth >= MAX_DEPTH {
            self.ctx.opaque.push(Opaque::TooDeep);
            return Ok(());
        }
        let chars: Vec<char> = script.chars().collect();
        let byte_offsets = char_byte_offsets(script);
        let mut nested_ctx = ScanCtx {
            chars: &chars,
            byte_offsets: &byte_offsets,
            end_byte: script.len(),
            tables: self.ctx.tables,
            invocations: Vec::new(),
            opaque: Vec::new(),
        };
        {
            let mut parser = ListParser::new(&mut nested_ctx, 0, chars.len(), self.depth + 1);
            parser.run_as(label)?;
        }
        self.ctx.invocations.extend(nested_ctx.invocations);
        self.ctx.opaque.extend(nested_ctx.opaque);
        Ok(())
    }

    /// Like [`run`](Self::run), but every command found at this level is
    /// labeled `first_position` -- never re-derived from an operator or an
    /// assignment prefix -- since the whole region shares one [`Position`]
    /// label, as substitutions, wrappers, inline shells, heredoc shells,
    /// and function bodies do.
    fn run_as(&mut self, first_position: Position) -> Result<(), ScanError> {
        self.run_with(Some(first_position))
    }

    /// Shared driver behind [`Self::run`] (`label` is `None`: each
    /// command's position is derived from what precedes it) and
    /// [`Self::run_as`] (`label` is `Some(position)`: every command in the
    /// region is forced to `position`, and the separator's own position
    /// never overrides it).
    fn run_with(&mut self, label: Option<Position>) -> Result<(), ScanError> {
        let mut position = label.unwrap_or(Position::First);
        loop {
            self.skip_insignificant()?;
            if self.pos >= self.end || self.at_closing_keyword() {
                return self.finish();
            }
            let command = self.read_simple_command(position)?;
            if let Some(command) = command {
                match label {
                    Some(forced) => self.classify_with(command, Some(forced))?,
                    None => self.classify(command)?,
                }
            }
            self.skip_insignificant()?;
            match self.consume_separator()? {
                Some(next) => {
                    if label.is_none() {
                        position = next;
                    }
                }
                None => return self.finish(),
            }
        }
    }

    /// Splits off leading assignments, then resolves the remaining words
    /// at either a forced `label` or, when `label` is `None`, at
    /// `Position::AfterAssignment` (an assignment prefix was stripped) or
    /// the command's own recorded position. Shared by [`Self::classify`]
    /// (label `None`) and [`Self::run_with`]'s forced-label branch (used
    /// by regions -- substitution, wrapper, inline shell, heredoc shell,
    /// find -exec, function body -- whose whole contents share one
    /// [`Position`] label).
    fn classify_with(
        &mut self,
        command: SimpleCommand,
        label: Option<Position>,
    ) -> Result<(), ScanError> {
        let mut i = 0;
        while i < command.words.len() && command.words[i].is_assignment {
            i += 1;
        }
        let had_assignment = i > 0;
        let words = &command.words[i..];
        if words.is_empty() {
            return Ok(());
        }
        let position = label.unwrap_or(if had_assignment {
            Position::AfterAssignment
        } else {
            command.position
        });
        self.resolve_from(words, position)
    }
}

struct PendingHeredoc {
    delimiter: String,
    strip_tabs: bool,
    shell_owner: bool,
}

/// Finds a `-exec` / `-execdir` in `find`'s arguments and returns the
/// command-spec words between it and its `;`/`+` terminator (exclusive),
/// or `None` if `find` carries no `-exec`. `find` positional-only knowledge
/// is one binary-specific case named directly by FR-CMD-007's Behavior
/// section, not a general per-tool table.
fn find_exec_spec(args: &[Word]) -> Option<&[Word]> {
    let idx = args
        .iter()
        .position(|w| w.text == "-exec" || w.text == "-execdir")?;
    let start = idx + 1;
    let mut end = args.len();
    for (i, w) in args.iter().enumerate().skip(start) {
        if w.text == ";" || w.text == "+" {
            end = i;
            break;
        }
    }
    Some(&args[start..end])
}

fn basename(text: &str) -> String {
    let stripped = text.strip_prefix('\\').unwrap_or(text);
    let trimmed = stripped.trim_matches('"').trim_matches('\'');
    match trimmed.rsplit('/').next() {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => trimmed.to_string(),
    }
}

fn find_flag_value<'a>(words: &'a [Word], flag: &str) -> Option<&'a Word> {
    let mut i = 0;
    while i < words.len() {
        if words[i].text == flag {
            return words.get(i + 1);
        }
        i += 1;
    }
    None
}

/// Skips a single-quoted span opened at `chars[start]` (the opening `'`),
/// returning the index just past the closing quote, or `None` if `end` is
/// reached first. Shared by [`find_substitution_end`] and [`find_balanced`]
/// so the two scans cannot disagree on where a single-quoted span ends.
fn skip_single_quoted(chars: &[char], start: usize, end: usize) -> Option<usize> {
    let mut i = start + 1;
    while i < end && chars[i] != '\'' {
        i += 1;
    }
    if i < end { Some(i + 1) } else { None }
}

/// Skips a double-quoted span opened at `chars[start]` (the opening `"`),
/// returning the index just past the closing quote, or the overrun index
/// (used for the error offset) if `end` is reached first. Shared by
/// [`find_substitution_end`] and [`find_balanced`] so the two scans cannot
/// disagree on where a double-quoted span ends, including its
/// backslash-escaping rule.
fn skip_double_quoted(chars: &[char], start: usize, end: usize) -> Result<usize, usize> {
    let mut i = start + 1;
    while i < end {
        match chars[i] {
            '"' => break,
            '\\' => i += 2,
            _ => i += 1,
        }
    }
    if i < end { Ok(i + 1) } else { Err(i) }
}

/// Finds the byte offset of the `)` matching a `$(` / `<(` / `>(` opened at
/// `open` (used for the error message), scanning `chars[start..end]`.
/// Handles nested parens, quotes, and skips heredoc bodies as literal text
/// (RESEARCH-CMD-bounded-tokenizer: this is the fix for
/// `git commit -m "$(cat <<'EOF' ... EOF)"`).
fn find_substitution_end(
    ctx: &ScanCtx,
    start: usize,
    end: usize,
    open: usize,
) -> Result<usize, ScanError> {
    let mut i = start;
    let mut depth = 1i32;
    while i < end {
        match ctx.chars[i] {
            '\'' => {
                i = skip_single_quoted(ctx.chars, i, end)
                    .ok_or_else(|| ScanError::UnterminatedSingleQuote(ctx.byte_at(end)))?;
            }
            '"' => {
                i = skip_double_quoted(ctx.chars, i, end)
                    .map_err(|overrun| ScanError::UnterminatedDoubleQuote(ctx.byte_at(overrun)))?;
            }
            '\\' => {
                i += 2;
            }
            '(' => {
                depth += 1;
                i += 1;
            }
            ')' => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    return Ok(i - 1);
                }
            }
            '<' if ctx.chars.get(i + 1) == Some(&'<') => {
                i = skip_heredoc_as_literal(ctx, i, end)?;
            }
            _ => i += 1,
        }
    }
    Err(ScanError::UnterminatedSubstitution(open))
}

/// True when `line` is the line that closes a heredoc opened with
/// `delimiter`, applying the `<<-` tab-stripping rule when `strip_tabs` is
/// set. Shared by [`ListParser::drain_pending_heredocs`] (a heredoc
/// consumed as real input) and [`skip_heredoc_as_literal`] (one skipped as
/// literal text inside a substitution), so the two heredoc readers cannot
/// drift on what counts as a closing line.
fn heredoc_line_closes(line: &str, delimiter: &str, strip_tabs: bool) -> bool {
    let trimmed = if strip_tabs {
        line.trim_start_matches('\t')
    } else {
        line
    };
    trimmed == delimiter
}

/// Skips a `<<DELIM`/`<<'DELIM'`/`<<-DELIM` heredoc as literal text,
/// starting at the `<<` and returning the index just past the line that
/// closes it. Used only inside substitution scanning, where the heredoc's
/// body is data for the owning command (e.g. `cat`), not shell grammar the
/// scanner needs to enter.
fn skip_heredoc_as_literal(ctx: &ScanCtx, at: usize, end: usize) -> Result<usize, ScanError> {
    let open = ctx.byte_at(at);
    let mut i = at + 2;
    let mut strip_tabs = false;
    if ctx.chars.get(i) == Some(&'-') {
        strip_tabs = true;
        i += 1;
    }
    if ctx.chars.get(i) == Some(&'~') {
        i += 1;
    }
    while matches!(ctx.chars.get(i), Some(c) if *c == ' ' || *c == '\t') {
        i += 1;
    }
    let mut delimiter = String::new();
    match ctx.chars.get(i) {
        Some(&quote @ ('\'' | '"')) => {
            i += 1;
            while ctx.chars.get(i) != Some(&quote) {
                if i >= end {
                    return Err(ScanError::UnterminatedHeredoc {
                        delimiter,
                        offset: open,
                    });
                }
                delimiter.push(ctx.chars[i]);
                i += 1;
            }
            i += 1;
        }
        _ => {
            while let Some(c) = ctx.chars.get(i) {
                if c.is_whitespace() || matches!(c, '|' | '&' | ';' | ')') {
                    break;
                }
                delimiter.push(*c);
                i += 1;
            }
        }
    }
    while ctx.chars.get(i) != Some(&'\n') {
        if i >= end {
            return Err(ScanError::UnterminatedHeredoc {
                delimiter,
                offset: open,
            });
        }
        i += 1;
    }
    i += 1; // the newline ending the operator's own line
    loop {
        let mut line = String::new();
        while i < end && ctx.chars.get(i) != Some(&'\n') {
            line.push(ctx.chars[i]);
            i += 1;
        }
        if heredoc_line_closes(&line, &delimiter, strip_tabs) {
            if i < end {
                i += 1;
            }
            return Ok(i);
        }
        if i >= end {
            return Err(ScanError::UnterminatedHeredoc {
                delimiter,
                offset: open,
            });
        }
        i += 1;
    }
}

/// Finds the byte offset of `close_ch` matching an `open_ch` already
/// consumed at `open`, scanning `chars[start..end]` and skipping quoted
/// text (via the same [`skip_single_quoted`]/[`skip_double_quoted`] helpers
/// [`find_substitution_end`] uses, so the two never disagree on where a
/// quote ends). Unlike a substitution, an unterminated quote here is not
/// itself reported: the outer loop simply runs out and reports
/// [`ScanError::UnbalancedParen`], the only such variant `ScanError`
/// defines. Shared by [`find_balanced_paren`] and [`find_balanced_brace`],
/// which differ only in which character pair they balance.
fn find_balanced(
    ctx: &ScanCtx,
    start: usize,
    end: usize,
    open: usize,
    open_ch: char,
    close_ch: char,
) -> Result<usize, ScanError> {
    let mut i = start;
    let mut depth = 1i32;
    while i < end {
        match ctx.chars[i] {
            '\'' => {
                i = skip_single_quoted(ctx.chars, i, end).unwrap_or(end);
            }
            '"' => {
                i = skip_double_quoted(ctx.chars, i, end).unwrap_or_else(|overrun| overrun);
            }
            c if c == open_ch => {
                depth += 1;
                i += 1;
            }
            c if c == close_ch => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    return Ok(i - 1);
                }
            }
            _ => i += 1,
        }
    }
    Err(ScanError::UnbalancedParen(open))
}

fn find_balanced_paren(
    ctx: &ScanCtx,
    start: usize,
    end: usize,
    open: usize,
) -> Result<usize, ScanError> {
    find_balanced(ctx, start, end, open, '(', ')')
}

fn find_balanced_brace(
    ctx: &ScanCtx,
    start: usize,
    end: usize,
    open: usize,
) -> Result<usize, ScanError> {
    find_balanced(ctx, start, end, open, '{', '}')
}

fn wrapper_match<'a>(spec: &tables::WrapperSpec, rest: &'a [Word]) -> WrapperMatch<'a> {
    let mut i = 0;
    if let Some(gate) = spec.subcommand_gate {
        match rest.first() {
            Some(w) if gate.contains(&w.text.as_str()) => {
                i += 1;
            }
            _ => return WrapperMatch::NoTarget,
        }
    }
    while i < rest.len() {
        let w = &rest[i];
        if w.text.starts_with('-') {
            if spec.suppress_flags.contains(&w.text.as_str()) {
                return WrapperMatch::Suppressed;
            }
            if spec.command_line_flags.contains(&w.text.as_str()) {
                return WrapperMatch::CommandLine {
                    line_word: rest.get(i + 1),
                };
            }
            if spec.value_flags.contains(&w.text.as_str()) {
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if w.is_assignment {
            i += 1;
            continue;
        }
        break;
    }
    let mut skip = spec.positional_skip;
    while skip > 0 && i < rest.len() {
        i += 1;
        skip -= 1;
    }
    if i >= rest.len() {
        return WrapperMatch::NoTarget;
    }
    WrapperMatch::Target(&rest[i..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invocations(cmd: &str) -> Vec<Invocation> {
        scan(cmd)
            .unwrap_or_else(|e| panic!("scan({cmd:?}) failed: {e}"))
            .invocations
    }

    fn opaque(cmd: &str) -> Vec<Opaque> {
        scan(cmd)
            .unwrap_or_else(|e| panic!("scan({cmd:?}) failed: {e}"))
            .opaque
    }

    // -- First position ---------------------------------------------------

    #[test]
    fn first_position_plain_command() {
        let inv = invocations("grep -rn foo src");
        assert_eq!(inv.len(), 1);
        assert_eq!(inv[0].binary, "grep");
        assert_eq!(inv[0].args, vec!["-rn", "foo", "src"]);
        assert_eq!(inv[0].position, Position::First);
        assert_eq!(inv[0].depth, 0);
    }

    // -- After a pipe or list operator (NFR-CMD-002) ----------------------

    #[test]
    fn after_pipe_is_routed_identically_except_position() {
        let inv = invocations("cat src/a.rs | grep -c foo");
        let grep = inv
            .iter()
            .find(|i| i.binary == "grep")
            .expect("grep resolved");
        assert_eq!(grep.args, vec!["-c", "foo"]);
        assert_eq!(grep.position, Position::AfterOperator);
        assert_eq!(grep.depth, 0);
    }

    #[test]
    fn after_and_list_operator() {
        let inv = invocations("git status && git push origin main");
        assert!(inv.iter().any(|i| i.binary == "git"
            && i.args == vec!["push", "origin", "main"]
            && i.position == Position::AfterOperator));
    }

    #[test]
    fn after_semicolon() {
        let inv = invocations("echo hi; grep -c x f");
        let grep = inv
            .iter()
            .find(|i| i.binary == "grep")
            .expect("grep resolved");
        assert_eq!(grep.position, Position::AfterOperator);
    }

    // -- Behind an environment prefix (NFR-CMD-002) ------------------------

    #[test]
    fn behind_environment_prefix() {
        let inv = invocations("FOO=1 grep -n foo bar.txt");
        assert_eq!(inv.len(), 1);
        assert_eq!(inv[0].binary, "grep");
        assert_eq!(inv[0].args, vec!["-n", "foo", "bar.txt"]);
        assert_eq!(inv[0].position, Position::AfterAssignment);
    }

    #[test]
    fn multiple_assignments_before_command() {
        let inv = invocations("env -u PAGER LC_ALL=C rg -n foo".to_string().as_str());
        let rg = inv.iter().find(|i| i.binary == "rg").expect("rg resolved");
        assert_eq!(rg.args, vec!["-n", "foo"]);
        assert_eq!(rg.position, Position::Wrapper);
    }

    #[test]
    fn quoted_value_assignment_is_still_recognized() {
        let inv = invocations("FOO=\"a b\" grep -n x f");
        let grep = inv
            .iter()
            .find(|i| i.binary == "grep")
            .expect("grep resolved");
        assert_eq!(grep.position, Position::AfterAssignment);
    }

    // -- Inside an inline wrapper (NFR-CMD-002) ----------------------------

    #[test]
    fn inline_wrapper_timeout() {
        let inv = invocations("timeout 30 gh run watch 123456");
        let gh = inv.iter().find(|i| i.binary == "gh").expect("gh resolved");
        assert_eq!(gh.args, vec!["run", "watch", "123456"]);
        assert_eq!(gh.position, Position::Wrapper);
    }

    #[test]
    fn inline_wrapper_xargs() {
        let inv = invocations("git ls-files '*.rs' | xargs grep -l claim_inbox");
        let grep = inv
            .iter()
            .find(|i| i.binary == "grep")
            .expect("grep resolved");
        assert_eq!(grep.args, vec!["-l", "claim_inbox"]);
        assert_eq!(grep.position, Position::Wrapper);
    }

    #[test]
    fn command_dash_v_is_a_lookup_not_a_wrapper() {
        let inv = invocations("command -v gh >/dev/null");
        assert_eq!(inv.len(), 1);
        assert_eq!(inv[0].binary, "command");
    }

    #[test]
    fn bare_pnpm_is_ordinary_not_unwrapped() {
        let inv = invocations("pnpm wrangler deploy --env prod");
        assert_eq!(inv.len(), 1);
        assert_eq!(inv[0].binary, "pnpm");
        assert_eq!(inv[0].args, vec!["wrangler", "deploy", "--env", "prod"]);
    }

    #[test]
    fn pnpm_exec_is_a_wrapper() {
        let inv = invocations("pnpm exec wrangler tail");
        let wrangler = inv
            .iter()
            .find(|i| i.binary == "wrangler")
            .expect("wrangler resolved");
        assert_eq!(wrangler.position, Position::Wrapper);
    }

    // -- Inside sh -c or a shell heredoc -----------------------------------

    #[test]
    fn inline_shell_sh_dash_c() {
        let inv = invocations("sh -c 'curl -s localhost:8080/health | grep -q ok && echo up'");
        let grep = inv
            .iter()
            .find(|i| i.binary == "grep")
            .expect("grep resolved");
        assert_eq!(grep.position, Position::InlineShell);
        assert_eq!(grep.depth, 1);
    }

    #[test]
    fn heredoc_shell() {
        let inv = invocations("bash <<'EOF'\ncd src\nrg -n 'fn main' .\nEOF");
        let rg = inv.iter().find(|i| i.binary == "rg").expect("rg resolved");
        assert_eq!(rg.position, Position::HeredocShell);
        assert_eq!(rg.depth, 1);
    }

    #[test]
    fn heredoc_inside_substitution_is_skipped_as_literal() {
        let cmd = "git add src/a.rs && git commit -m \"$(cat <<'EOF'\nfix: don't trust the agent's \"quotes\" (it's a body)\nEOF\n)\" && git push origin HEAD";
        let inv = invocations(cmd);
        let commit = inv
            .iter()
            .find(|i| i.binary == "git" && i.args.first().map(String::as_str) == Some("commit"));
        assert!(commit.is_some(), "git commit should resolve: {inv:?}");
        let push = inv
            .iter()
            .find(|i| i.binary == "git" && i.args.first().map(String::as_str) == Some("push"));
        assert!(push.is_some(), "git push should resolve: {inv:?}");
    }

    // -- find -exec ---------------------------------------------------------

    #[test]
    fn find_exec() {
        let inv = invocations("find src -name '*.rs' -exec grep -l 'fn route' {} \\;");
        let find = inv
            .iter()
            .find(|i| i.binary == "find")
            .expect("find resolved");
        assert_eq!(find.position, Position::First);
        let grep = inv
            .iter()
            .find(|i| i.binary == "grep")
            .expect("grep resolved");
        assert_eq!(grep.position, Position::FindExec);
        assert_eq!(grep.depth, 1);
    }

    // -- function body --------------------------------------------------

    #[test]
    fn function_keyword_body() {
        let inv = invocations("function g { grep -rn \"$@\"; }");
        let grep = inv
            .iter()
            .find(|i| i.binary == "grep")
            .expect("grep resolved");
        assert_eq!(grep.position, Position::FunctionBody);
        assert_eq!(grep.depth, 1);
    }

    #[test]
    fn posix_function_def_body() {
        let inv = invocations("g() { rg -n foo; }");
        let rg = inv.iter().find(|i| i.binary == "rg").expect("rg resolved");
        assert_eq!(rg.position, Position::FunctionBody);
    }

    // -- substitution -----------------------------------------------------

    #[test]
    fn command_substitution() {
        let inv = invocations("wc -l $(find src -name '*.rs')");
        let find = inv
            .iter()
            .find(|i| i.binary == "find")
            .expect("find resolved");
        assert_eq!(find.position, Position::Substitution);
        assert_eq!(find.depth, 1);
    }

    #[test]
    fn backtick_substitution() {
        let inv = invocations("for f in `ls src`; do echo $f; done");
        let ls = inv.iter().find(|i| i.binary == "ls").expect("ls resolved");
        assert_eq!(ls.position, Position::Substitution);
    }

    #[test]
    fn process_substitution() {
        let inv = invocations("diff <(ls a) <(ls b)");
        let ls_calls: Vec<_> = inv.iter().filter(|i| i.binary == "ls").collect();
        assert_eq!(ls_calls.len(), 2);
        for ls in ls_calls {
            assert_eq!(ls.position, Position::Substitution);
        }
    }

    // -- depth cap (FR-CMD-007) --------------------------------------------

    #[test]
    fn too_deep_beyond_depth_two() {
        let opq = opaque("echo $(echo $(echo $(grep -c x f)))");
        assert!(opq.contains(&Opaque::TooDeep), "{opq:?}");
    }

    #[test]
    fn find_exec_sh_c_reaches_depth_two_not_too_deep() {
        let inv = invocations("find . -name '*.md' -exec sh -c 'grep -l TODO \"$1\"' _ {} \\;");
        let grep = inv
            .iter()
            .find(|i| i.binary == "grep")
            .expect("grep resolved");
        assert_eq!(grep.depth, 2);
    }

    // -- opaque classes (FR-CMD-007) ----------------------------------------

    #[test]
    fn opaque_interpreter_python_c() {
        let opq = opaque("python3 -c \"print(1)\"");
        assert!(
            matches!(&opq[0], Opaque::Interpreter { interpreter, .. } if interpreter == "python3")
        );
    }

    #[test]
    fn opaque_script_file() {
        let opq = opaque("./scripts/release.sh --dry-run");
        assert_eq!(opq, vec![Opaque::ScriptFile]);
    }

    #[test]
    fn opaque_script_file_via_interpreter() {
        let opq = opaque("bash run2.sh");
        assert_eq!(opq, vec![Opaque::ScriptFile]);
    }

    #[test]
    fn opaque_eval() {
        let opq = opaque("eval \"$(ssh-agent -s)\"");
        assert!(opq.contains(&Opaque::Eval));
    }

    #[test]
    fn opaque_dynamic_command_variable() {
        let opq = opaque("$SEARCH -rn foo src");
        assert_eq!(opq, vec![Opaque::DynamicCommand]);
    }

    #[test]
    fn opaque_dynamic_command_substitution() {
        let opq = opaque("$(which rg) -n foo");
        assert_eq!(opq, vec![Opaque::DynamicCommand]);
    }

    #[test]
    fn opaque_stdin_script() {
        let opq = opaque("python3 <<'PY'\nprint(1)\nPY");
        assert!(opq.contains(&Opaque::StdinScript));
    }

    #[test]
    fn opaque_sourced() {
        let opq = opaque("source <(echo 'grep -rn foo src')");
        assert!(opq.contains(&Opaque::Sourced));
    }

    // -- malformed input never yields a partial Scan (Error Handling) -----

    #[test]
    fn unterminated_quote_is_an_error() {
        let result = scan("grep -n 'oops src");
        assert!(matches!(result, Err(ScanError::UnterminatedSingleQuote(_))));
    }

    #[test]
    fn unterminated_quote_inside_substitution_is_reported() {
        // find_substitution_end reports an unterminated quote directly.
        let result = scan("echo $(grep -n 'oops src)");
        assert!(matches!(result, Err(ScanError::UnterminatedSingleQuote(_))));
    }

    #[test]
    fn unterminated_quote_inside_brace_group_is_unbalanced() {
        // find_balanced (used for function bodies/brace groups) does not
        // report the unterminated quote itself: it runs past it and the
        // outer scan reports the brace as never closing instead. This is
        // the documented divergence from find_substitution_end -- the two
        // callers of the shared quote-skip helpers choose different
        // errors on purpose, not by accident.
        let result = scan("foo() { echo 'oops");
        assert!(matches!(result, Err(ScanError::UnbalancedParen(_))));
    }

    #[test]
    fn never_panics_on_arbitrary_strings() {
        // Deterministic pseudo-random property test (no external crate):
        // a small LCG picks characters from a shell-ish, multibyte-
        // inclusive alphabet, including operators, quotes, `$(`, `<<`,
        // and truncated heredoc markers, so both ASCII-boundary bugs and
        // multibyte slicing bugs would surface as a panic.
        let alphabet: Vec<char> = "grep -c foo|;&()$`'\"\\<>{}=@#!\n\t \u{e9}\u{4e2d}\u{6587}"
            .chars()
            .collect();
        let mut state: u64 = 0x9E3779B97F4A7C15;
        for _ in 0..2000 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let len = ((state >> 33) % 40) as usize;
            let mut s = String::new();
            for _ in 0..len {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                let idx = ((state >> 33) as usize) % alphabet.len();
                s.push(alphabet[idx]);
            }
            // Must not panic, regardless of the Result it returns.
            let _ = scan(&s);
        }
    }

    #[test]
    fn empty_command_scans_to_nothing() {
        let scan_result = scan("").expect("empty command is not malformed");
        assert!(scan_result.invocations.is_empty());
        assert!(scan_result.opaque.is_empty());
    }
}
