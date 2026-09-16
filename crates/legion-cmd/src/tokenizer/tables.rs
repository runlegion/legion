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
/// `next_step.if_yes` with `watch`, `ssh`, `parallel`, and `setsid`, and per
/// FR-CMD-007 revision 6 with the single-word JS package runners.
/// `docker exec` and the two-word JS runner forms (`pnpm exec`, `pnpm dlx`,
/// `npm exec`, `yarn exec`, `yarn dlx`) are handled separately, by
/// [`TWO_WORD_WRAPPERS`].
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
    // JS package runners, single-word form (FR-CMD-007 revision 6, operator
    // decision 2026-09-16 / reflection 01a0ac2d): `npx`/`pnpx`/`bunx -y
    // rg ...` reach the wrapped command the same way `env`/`sudo` do. `-p`
    // and `--package` take a value; `-y`/`--yes` is boolean and needs no
    // entry.
    ("npx", &["-p", "--package"], 0),
    ("pnpx", &["-p", "--package"], 0),
    ("bunx", &["-p", "--package"], 0),
];

/// `docker`'s own global options that can precede the `exec` subcommand
/// (`docker -H tcp://host exec box grep ...`), the ones that take a
/// separate value word. Boolean flags (`-D`/`--debug`, `--tls`,
/// `--tlsverify`, ...) need no table entry: the option-skip walk already
/// skips any unlisted `-flag`/`--flag` as a single token.
pub(super) const DOCKER_GLOBAL_VALUE_OPTS: &[&str] = &[
    "-H",
    "--host",
    "-c",
    "--context",
    "--config",
    "-l",
    "--log-level",
    "--tlscacert",
    "--tlscert",
    "--tlskey",
];

/// `docker exec`'s own options that take a separate value word, used by
/// its [`TWO_WORD_WRAPPERS`] row.
pub(super) const DOCKER_EXEC_VALUE_OPTS: &[&str] =
    &["-u", "-w", "-e", "--user", "--workdir", "--env"];

/// `pnpm`'s own global options that can precede `exec`/`dlx` and take a
/// separate value word: `-F`/`--filter <selector>`, `-C`/`--dir <path>`,
/// `--config <path>`.
const PNPM_GLOBAL_VALUE_OPTS: &[&str] = &["-F", "--filter", "-C", "--dir", "--config"];

/// `npm`'s own global options that can precede `exec` and take a separate
/// value word: `--prefix <path>`, `-w`/`--workspace <name>`, `--userconfig
/// <path>`. npm has no `-C`/`--cwd`-shaped flag (checked against npm's own
/// docs rather than assumed from git's or pnpm's `-C`), so none is listed.
const NPM_GLOBAL_VALUE_OPTS: &[&str] = &["--prefix", "-w", "--workspace", "--userconfig"];

/// `yarn`'s own global option that can precede `exec`/`dlx` and takes a
/// separate value word: `--cwd <path>`.
const YARN_GLOBAL_VALUE_OPTS: &[&str] = &["--cwd"];

/// (first word, second word, the first word's own options that can precede
/// the second word, the second word's own options, positional arguments
/// consumed before the wrapped command -- `docker exec` needs the
/// container name, the JS runners do not).
pub(super) type TwoWordWrapper = (
    &'static str,
    &'static str,
    &'static [&'static str],
    &'static [&'static str],
    usize,
);

/// Two-word inline wrappers (FR-CMD-007 revision 6, operator decision
/// 2026-09-16 / reflection 01a0ac2d): `docker exec` and the JS
/// package-runner `exec`/`dlx` forms, one shared mechanism rather than a
/// special case per tool. A bare `pnpm <name>` (no `exec`/`dlx`) is not a
/// wrapper at all: it falls through to an ordinary invocation.
pub(super) const TWO_WORD_WRAPPERS: &[TwoWordWrapper] = &[
    (
        "docker",
        "exec",
        DOCKER_GLOBAL_VALUE_OPTS,
        DOCKER_EXEC_VALUE_OPTS,
        1,
    ),
    (
        "pnpm",
        "exec",
        PNPM_GLOBAL_VALUE_OPTS,
        &["--package", "-p"],
        0,
    ),
    (
        "pnpm",
        "dlx",
        PNPM_GLOBAL_VALUE_OPTS,
        &["--package", "-p"],
        0,
    ),
    (
        "npm",
        "exec",
        NPM_GLOBAL_VALUE_OPTS,
        &["--package", "-p"],
        0,
    ),
    ("yarn", "exec", YARN_GLOBAL_VALUE_OPTS, &[], 0),
    ("yarn", "dlx", YARN_GLOBAL_VALUE_OPTS, &[], 0),
];

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
