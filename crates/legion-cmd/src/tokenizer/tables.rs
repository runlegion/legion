//! The closed name tables `resolve` dispatches on: keywords, the inline
//! wrapper table (FR-CMD-007, hardened per RESEARCH-CMD-bounded-tokenizer's
//! `next_step.if_yes`), shells, interpreters, lookups, and script
//! extensions. Pure data -- no logic lives here.

pub(super) const PREFIX_KEYWORDS: &[&str] = &[
    "if", "then", "else", "elif", "do", "while", "until", "!", "{", "coproc",
];
pub(super) const END_KEYWORDS: &[&str] = &["fi", "done", "esac", "}"];
/// Words whose remaining siblings are data, never a command position.
pub(super) const CLAUSE_KEYWORDS: &[&str] = &[
    "for", "case", "select", "in", "local", "declare", "export", "readonly", "typeset", "unset",
    "alias", "trap", "return", "exit", "break", "continue", "shift", "set", "read", "wait",
];

/// (wrapper name, options that take a separate value word, positional
/// arguments consumed before the wrapped command) -- FR-CMD-007's inline
/// wrapper table, hardened per RESEARCH-CMD-bounded-tokenizer's
/// `next_step.if_yes` with `watch`, `ssh`, `parallel`, and `setsid`.
/// `docker exec` is handled separately: it is a two-word wrapper.
pub(super) const WRAPPERS: &[(&str, &[&str], usize)] = &[
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

/// `docker exec`'s options that take a separate value word. Not a
/// [`WRAPPERS`] row because that table is keyed on a single word; `docker`
/// alone is not a wrapper (`docker build`, `docker ps`, ... are ordinary
/// invocations), only `docker exec` is.
pub(super) const DOCKER_EXEC_VALUE_OPTS: &[&str] =
    &["-u", "-w", "-e", "--user", "--workdir", "--env"];

/// Launchers this scanner recognizes as wrapper-shaped -- a bare command
/// followed by the command it runs -- but which FR-CMD-007's table does not
/// name. Kept small and explicit on purpose: only entries here become
/// [`super::Opaque::UnknownWrapper`]; every other unrecognized binary is an
/// ordinary invocation, not a wrapper at all.
pub(super) const UNKNOWN_WRAPPERS: &[&str] = &["flock", "chpst", "unbuffer"];

pub(super) const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "fish"];
pub(super) const INTERPRETERS: &[&str] = &[
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
pub(super) const LOOKUPS: &[&str] = &["which", "type", "whereis", "man", "hash", "help", "tldr"];

/// Extensions that mark a word as a separate script file, not resolvable
/// without reading it -- regardless of whether it is named by a path
/// (`./scripts/release.sh`) or a bare name given to an interpreter
/// (`node scripts/scan.mjs`).
pub(super) const SCRIPT_EXTENSIONS: &[&str] = &[
    ".sh", ".py", ".js", ".mjs", ".ts", ".rb", ".pl", ".bash", ".zsh",
];
