//! `legion grep` and `legion rg` (#1334): each takes its own tool's
//! arguments exactly as typed and returns the answer that tool would give.
//!
//! sym answers when it can answer the search fully; everything else runs the
//! real tool with the arguments untouched, its output and exit code passed
//! through. Neither subcommand refuses a search and neither turns a sym
//! failure into an error of legion's own: every doubt falls back.
//!
//! The riskiest call is "sym answers this fully". A wrong yes returns fewer
//! matches than the tool would, so the accepted set is deliberately narrow
//! (#1354 widened it to every output shape sym's matches determine):
//! - the output is one the matches determine: `path:line:text` (`-n`),
//!   `path:text` (no flag), each matching path (`-l`), or `path:count`
//!   (`-c`); grep must be recursive, since it prints an error for a
//!   directory operand otherwise;
//! - the pattern must mean the same thing to sym's matcher as to the tool:
//!   a literal for grep (its BRE/ERE dialects are not Rust regex), any
//!   pattern for rg (same regex engine). `-i` and `-w` become the regex rg
//!   itself builds for them; for grep they also need every candidate line
//!   to be ASCII, where word characters and case folding mean the same in
//!   every locale;
//! - every operand must be a real directory inside a watched repo;
//! - the files searched are the ones the tool searches: the tool's own walk
//!   (`tool_files`) lists them, honoring `--hidden`, `--no-ignore`, `-g`,
//!   `-t` and `--include`, and sym scans exactly that list, uncapped. The
//!   scan must come back with nothing skipped or binary, and no matched file
//!   holding a CR or bytes that are not UTF-8.
//!
//! Anything outside that set -- another flag, a file operand, standard
//! input, a path outside every watched repo -- runs the real tool.

use std::ffi::{OsStr, OsString};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use crate::error::{LegionError, Result};
use crate::{data_dir, etc, watch};

/// The two tools these subcommands stand in for. Each reads its own flags:
/// `-v` is grep's invert-match, and the proxy never reads it as legion's
/// verbose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Grep,
    Rg,
}

impl Tool {
    /// The executable name the real tool runs as.
    fn program(self) -> &'static str {
        match self {
            Tool::Grep => "grep",
            Tool::Rg => "rg",
        }
    }
}

/// What the tool prints for the matches it found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// One line per matching line: `path:line:text` with `-n`, `path:text`
    /// without (rg prints no line numbers when piped, grep none unasked).
    Lines { numbered: bool },
    /// `-l`: each matching file's path, once.
    Files,
    /// `-c`: `path:count`. grep lists every file it searched, zero counts
    /// included; rg lists only files that matched.
    Counts,
}

/// grep `-w` over a literal: a candidate line counts only when the literal
/// occurs in it as a whole word.
#[derive(Debug, PartialEq, Eq)]
struct WordFilter {
    literal: String,
    ignore_case: bool,
}

/// A search sym can answer: the pattern as sym's matcher takes it, the
/// directory operands exactly as the agent typed them (output paths are
/// built from them, the way the tool builds its own), what to print, and
/// what selects the files the tool searches.
#[derive(Debug, PartialEq, Eq)]
struct SymSearch {
    pattern: String,
    fixed_strings: bool,
    operands: Vec<String>,
    shape: Shape,
    /// grep `-w`: the whole-word check applied to each candidate line.
    word: Option<WordFilter>,
    /// grep `-i` / `-w`: every candidate line must be ASCII, or the tool runs.
    ascii_lines: bool,
    /// grep `--include` globs.
    includes: Vec<String>,
    /// rg `--hidden`.
    hidden: bool,
    /// rg `--no-ignore`.
    no_ignore: bool,
    /// rg `-g` / `--glob`.
    globs: Vec<String>,
    /// rg `-t` / `--type`.
    types: Vec<String>,
}

/// Dispatch `legion grep ...` / `legion rg ...` from the raw process argv,
/// before clap sees it.
///
/// clap cannot carry these arguments untouched: legion's global `-v` would
/// eat grep's invert-match, its `-h` is grep's no-filename, and it consumes
/// a `--` the tool must see. Returns `None` when argv[1] names neither
/// subcommand, so the caller parses normally.
pub(crate) fn run_from_argv(argv: &[OsString]) -> Option<Result<()>> {
    let tool: Tool = match argv.get(1).and_then(|a| a.to_str()) {
        Some("grep") => Tool::Grep,
        Some("rg") => Tool::Rg,
        _ => return None,
    };
    Some(run(tool, argv.get(2..).unwrap_or(&[])))
}

/// Handle `legion grep` reached through clap (e.g. behind a leading legion
/// flag). Same behavior as the raw-argv path.
pub(crate) fn handle_grep(args: Vec<OsString>) -> Result<()> {
    run(Tool::Grep, &args)
}

/// Handle `legion rg` reached through clap. Same behavior as the raw-argv
/// path.
pub(crate) fn handle_rg(args: Vec<OsString>) -> Result<()> {
    run(Tool::Rg, &args)
}

/// sym's answer: the lines to print, and whether anything matched (grep
/// `-c` prints zero counts yet exits 1).
struct Answer {
    lines: Vec<String>,
    matched: bool,
}

/// Answer from sym when it can answer fully, otherwise run the real tool.
fn run(tool: Tool, args: &[OsString]) -> Result<()> {
    let answer: Option<Answer> = sym_answer(tool, args);
    match answer {
        Some(answer) => print_answer(&answer.lines, answer.matched),
        None => run_real_tool(tool, args),
    }
}

/// The whole sym attempt. `None` means "run the real tool": any parse doubt,
/// any error, and any sign the scan saw less than the tool would.
fn sym_answer(tool: Tool, args: &[OsString]) -> Option<Answer> {
    let search: SymSearch = match tool {
        Tool::Grep => {
            // GREP_OPTIONS injects flags the argv does not show.
            if std::env::var_os("GREP_OPTIONS").is_some_and(|v| !v.is_empty()) {
                return None;
            }
            parse_grep(args)?
        }
        Tool::Rg => {
            // A config file injects flags; a terminal switches rg to its
            // heading format, which is none of the shapes sym prints.
            if std::env::var_os("RIPGREP_CONFIG_PATH").is_some_and(|v| !v.is_empty())
                || std::io::stdout().is_terminal()
            {
                return None;
            }
            parse_rg(args)?
        }
    };
    let workdirs: Vec<PathBuf> = watched_workdirs()?;
    let cwd: PathBuf = std::env::current_dir().ok()?;
    let mut answer = Answer {
        lines: Vec::new(),
        matched: false,
    };
    for operand in &search.operands {
        let root: PathBuf = checked_operand(tool, operand, &workdirs)?;
        let files: Vec<String> = tool_files(tool, &search, operand, &root, &cwd)?;
        let hits: Vec<etc::ContentHit> = scan(&search, &root, &files)?;
        answer.matched |= !hits.is_empty();
        render(tool, &search, operand, &files, &hits, &mut answer.lines)?;
    }
    Some(answer)
}

/// Append one operand's output lines in the search's shape. A hit's `repo`
/// is its file's path relative to the operand (see `scan`).
fn render(
    tool: Tool,
    search: &SymSearch,
    operand: &str,
    files: &[String],
    hits: &[etc::ContentHit],
    lines: &mut Vec<String>,
) -> Option<()> {
    let shown =
        |rel: &str| -> Option<String> { Path::new(operand).join(rel).to_str().map(str::to_string) };
    match search.shape {
        Shape::Lines { numbered } => {
            for hit in hits {
                let path: String = shown(&hit.repo)?;
                lines.push(if numbered {
                    format!("{path}:{}:{}", hit.line, hit.text)
                } else {
                    format!("{path}:{}", hit.text)
                });
            }
        }
        Shape::Files => {
            let mut last: Option<&str> = None;
            for hit in hits {
                if last != Some(hit.repo.as_str()) {
                    lines.push(shown(&hit.repo)?);
                    last = Some(hit.repo.as_str());
                }
            }
        }
        Shape::Counts => {
            for rel in files {
                let count: usize = hits.iter().filter(|h| &h.repo == rel).count();
                if count > 0 || tool == Tool::Grep {
                    lines.push(format!("{}:{count}", shown(rel)?));
                }
            }
        }
    }
    Some(())
}

/// Canonical workdirs of every repo in watch.toml. `None` on any read
/// error; an empty list simply matches no operand.
fn watched_workdirs() -> Option<Vec<PathBuf>> {
    let path: PathBuf = data_dir().ok()?.join("watch.toml");
    let repos: Vec<watch::WatchRepoConfig> = watch::list_repos_in_config(&path).ok()?;
    Some(
        repos
            .iter()
            .filter_map(|r| std::fs::canonicalize(&r.workdir).ok())
            .collect(),
    )
}

/// Resolve one operand to the directory sym scans, or `None` when sym cannot
/// stand in for the tool on it: not a real directory (a file, a symlink,
/// standard input), a trailing-slash spelling tools print differently,
/// outside every watched repo, or (rg) outside a git checkout, where rg
/// ignores .gitignore and sym does not.
fn checked_operand(tool: Tool, operand: &str, workdirs: &[PathBuf]) -> Option<PathBuf> {
    if operand.is_empty() || operand == "-" || operand.ends_with('/') {
        return None;
    }
    let meta: std::fs::Metadata = std::fs::symlink_metadata(operand).ok()?;
    if !meta.is_dir() {
        return None;
    }
    let root: PathBuf = std::fs::canonicalize(operand).ok()?;
    if !workdirs.iter().any(|w| root.starts_with(w)) {
        return None;
    }
    if tool == Tool::Rg {
        let in_git: bool = root.ancestors().any(|a| a.join(".git").exists());
        let rgignore_above: bool = root.ancestors().any(|a| a.join(".rgignore").exists());
        if !in_git || rgignore_above {
            return None;
        }
    }
    Some(root)
}

/// The files the tool searches under one operand, as `/`-separated paths
/// relative to it, sorted -- or `None` when the tree holds something the
/// tool treats in a way sym does not follow.
///
/// Each list comes from the tool's own walk. grep -r reads every file,
/// hidden and gitignored alike, and reads `.git` too, which sym never does
/// -- so a tree holding a `.git` entry is the tool's; `--include` then keeps
/// the files whose base name matches. rg's walk is the one ripgrep builds
/// (the operand as typed, globs rooted at the working directory, default
/// file types, `.rgignore`), so `-g`, `-t`, `--hidden` and `--no-ignore`
/// select exactly the files rg would, including a gitignored file a glob
/// admits. Symlinks and special files are handled differently across tool
/// flavors, so any of them is the tool's, as is a directory holding an
/// `.rgignore` under rg.
fn tool_files(
    tool: Tool,
    search: &SymSearch,
    operand: &str,
    root: &Path,
    cwd: &Path,
) -> Option<Vec<String>> {
    let walker: ignore::Walk = match tool {
        Tool::Grep => {
            let mut builder: ignore::WalkBuilder = ignore::WalkBuilder::new(root);
            builder.standard_filters(false);
            builder.build()
        }
        Tool::Rg => rg_walk(search, operand, cwd)?,
    };
    let base: &Path = match tool {
        Tool::Grep => root,
        Tool::Rg => Path::new(operand),
    };
    let mut files: Vec<String> = Vec::new();
    for entry in walker {
        let entry: ignore::DirEntry = entry.ok()?;
        let file_type: std::fs::FileType = entry.file_type()?;
        if !(file_type.is_file() || file_type.is_dir()) || entry.file_name() == OsStr::new(".git") {
            return None;
        }
        if file_type.is_dir() {
            if tool == Tool::Rg && entry.path().join(".rgignore").exists() {
                return None;
            }
            continue;
        }
        let rel: String = entry
            .path()
            .strip_prefix(base)
            .ok()?
            .to_str()?
            .replace('\\', "/");
        let name: &str = entry.file_name().to_str()?;
        if search.includes.is_empty() || search.includes.iter().any(|g| include_matches(g, name)) {
            files.push(rel);
        }
    }
    files.sort();
    Some(files)
}

/// ripgrep's own directory walk for one operand, configured the way rg
/// configures it for these flags (ripgrep's `HiArgs::walk_builder`).
fn rg_walk(search: &SymSearch, operand: &str, cwd: &Path) -> Option<ignore::Walk> {
    let mut overrides = ignore::overrides::OverrideBuilder::new(cwd);
    for glob in &search.globs {
        overrides.add(glob).ok()?;
    }
    let mut types = ignore::types::TypesBuilder::new();
    types.add_defaults();
    for name in &search.types {
        types.select(name);
    }
    let no_ignore: bool = search.no_ignore;
    let mut builder: ignore::WalkBuilder = ignore::WalkBuilder::new(operand);
    builder
        .overrides(overrides.build().ok()?)
        .types(types.build().ok()?)
        .hidden(!search.hidden)
        .parents(!no_ignore)
        .ignore(!no_ignore)
        .git_global(!no_ignore)
        .git_ignore(!no_ignore)
        .git_exclude(!no_ignore)
        .require_git(true)
        .current_dir(cwd);
    if !no_ignore {
        builder.add_custom_ignore_filename(".rgignore");
    }
    Some(builder.build())
}

/// Whether a grep `--include` glob admits a file's base name. Only a literal
/// name or `*` followed by a literal suffix is accepted by `parse_grep`: GNU
/// grep matches the base name and BSD grep the base name or the whole path,
/// and for those two forms the answers agree.
fn include_matches(glob: &str, name: &str) -> bool {
    match glob.strip_prefix('*') {
        Some(suffix) => name.ends_with(suffix),
        None => name == glob,
    }
}

/// An `--include` glob `include_matches` reads the way every grep does.
fn include_is_plain(glob: &str) -> bool {
    let body: &str = glob.strip_prefix('*').unwrap_or(glob);
    (!body.is_empty() || glob == "*") && !body.contains(['*', '?', '[', ']', '{', '}', '\\', '/'])
}

/// Scan exactly `files` (paths relative to `root`) with sym's content
/// search. Each file is its own scan root, which no hidden or ignore rule
/// touches, and each hit's `repo` names its file. `None` when the scan
/// errors or reports anything it did not search the way the tool would.
fn scan(search: &SymSearch, root: &Path, files: &[String]) -> Option<Vec<etc::ContentHit>> {
    let repos: Vec<(String, PathBuf)> = files
        .iter()
        .map(|rel| (rel.clone(), root.join(rel)))
        .collect();
    let scope = etc::ContentScope {
        repos: &repos,
        ext: None,
        fixed_strings: search.fixed_strings,
        include_hidden: true,
        no_ignore: true,
        // The tools print every match in every file, however large; the
        // CLI's caps exist to keep find-content's own output short.
        max_file_size: u64::MAX,
        max_hits: usize::MAX,
    };
    let result: etc::FindContentResult = etc::find_content(&search.pattern, &scope).ok()?;
    if result.suppressed > 0
        || result.skipped_files > 0
        || result.binary_skipped > 0
        || !result.failed_repos.is_empty()
    {
        return None;
    }
    // sym trims a CR the tools print, and reads bad UTF-8 lossily where a
    // tool prints raw bytes or calls the file binary.
    let mut checked: Vec<&str> = Vec::new();
    for hit in &result.hits {
        if checked.contains(&hit.repo.as_str()) {
            continue;
        }
        let bytes: Vec<u8> = std::fs::read(root.join(&hit.repo)).ok()?;
        if bytes.contains(&b'\r') || std::str::from_utf8(&bytes).is_err() {
            return None;
        }
        checked.push(hit.repo.as_str());
    }
    if search.ascii_lines && result.hits.iter().any(|h| !h.text.is_ascii()) {
        return None;
    }
    let mut hits: Vec<etc::ContentHit> = result.hits;
    if let Some(word) = &search.word {
        hits.retain(|h| has_word(&h.text, word));
    }
    Some(hits)
}

/// grep `-w`: the literal occurs in the (ASCII) line with no word character
/// directly before or after it. Every occurrence is tried, as grep does.
fn has_word(line: &str, word: &WordFilter) -> bool {
    let (hay, needle): (String, String) = if word.ignore_case {
        (line.to_ascii_lowercase(), word.literal.to_ascii_lowercase())
    } else {
        (line.to_string(), word.literal.clone())
    };
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let bytes: &[u8] = hay.as_bytes();
    let mut from: usize = 0;
    while let Some(offset) = hay.get(from..).and_then(|rest| rest.find(needle.as_str())) {
        let start: usize = from + offset;
        let end: usize = start + needle.len();
        let before: bool = start == 0 || !is_word(bytes[start - 1]);
        let after: bool = end == bytes.len() || !is_word(bytes[end]);
        if before && after {
            return true;
        }
        from = start + 1;
    }
    false
}

/// The flags, pattern and operands of one argv, read with the tool's own
/// flag meanings. Only the flags sym can answer are known; any other flag
/// makes the argv a form sym does not answer.
#[derive(Default)]
struct ToolArgv {
    recursive: bool,
    line_numbers: bool,
    extended: bool,
    fixed: bool,
    ignore_case: bool,
    word: bool,
    files: bool,
    count: bool,
    hidden: bool,
    no_ignore: bool,
    globs: Vec<String>,
    types: Vec<String>,
    includes: Vec<String>,
    pattern: String,
    operands: Vec<String>,
}

/// Read `args` as `tool` reads them, or `None` for any flag outside the
/// known set, a second pattern, no operand, or an empty or multi-line
/// pattern. grep options must all precede the first operand: BSD grep stops
/// option parsing there and GNU grep does not, so a later option means
/// different things to the two. rg reads options anywhere before `--`.
fn read_argv(tool: Tool, args: &[OsString]) -> Option<ToolArgv> {
    let args: Vec<&str> = args.iter().map(|a| a.to_str()).collect::<Option<_>>()?;
    let grep: bool = tool == Tool::Grep;
    let mut argv = ToolArgv::default();
    let mut pattern: Option<String> = None;
    let mut i: usize = 0;
    let mut options_done = false;
    while i < args.len() {
        let arg: &str = args[i];
        i += 1;
        if options_done || !arg.starts_with('-') || arg == "-" {
            argv.operands.push(arg.to_string());
            continue;
        }
        if grep && !argv.operands.is_empty() {
            return None;
        }
        if arg == "--" {
            options_done = true;
            continue;
        }
        if let Some(long) = arg.strip_prefix("--") {
            let (name, inline): (&str, Option<&str>) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (long, None),
            };
            // A flag that takes a value reads it inline or from the next arg.
            let mut value = || -> Option<String> {
                match inline {
                    Some(v) => Some(v.to_string()),
                    None => {
                        let v: &str = args.get(i)?;
                        i += 1;
                        Some(v.to_string())
                    }
                }
            };
            match (name, inline.is_some()) {
                ("recursive", false) if grep => argv.recursive = true,
                ("extended-regexp", false) if grep => argv.extended = true,
                ("include", true) if grep => argv.includes.push(value()?),
                ("hidden", false) if !grep => argv.hidden = true,
                ("no-ignore", false) if !grep => argv.no_ignore = true,
                ("glob", _) if !grep => argv.globs.push(value()?),
                ("type", _) if !grep => argv.types.push(value()?),
                ("line-number", false) => argv.line_numbers = true,
                ("fixed-strings", false) => argv.fixed = true,
                ("ignore-case", false) => argv.ignore_case = true,
                ("word-regexp", false) => argv.word = true,
                ("files-with-matches", false) => argv.files = true,
                ("count", false) => argv.count = true,
                ("regexp", _) => set_once(&mut pattern, &value()?)?,
                _ => return None,
            }
            continue;
        }
        let cluster: &str = &arg[1..];
        for (pos, flag) in cluster.char_indices() {
            match flag {
                'r' | 'R' if grep => argv.recursive = true,
                'E' if grep => argv.extended = true,
                'H' if grep => {}
                'n' => argv.line_numbers = true,
                'F' => argv.fixed = true,
                'i' => argv.ignore_case = true,
                'w' => argv.word = true,
                'l' => argv.files = true,
                'c' => argv.count = true,
                'e' | 'g' | 't' if flag == 'e' || !grep => {
                    // The value is the rest of the cluster, or the next arg.
                    let rest: &str = &cluster[pos + 1..];
                    let value: String = if rest.is_empty() {
                        let v: &str = args.get(i)?;
                        i += 1;
                        v.to_string()
                    } else {
                        rest.to_string()
                    };
                    match flag {
                        'e' => set_once(&mut pattern, &value)?,
                        'g' => argv.globs.push(value),
                        _ => argv.types.push(value),
                    }
                    break;
                }
                _ => return None,
            }
        }
    }
    argv.pattern = match pattern {
        Some(p) => p,
        None if !argv.operands.is_empty() => argv.operands.remove(0),
        None => return None,
    };
    if argv.pattern.is_empty() || argv.pattern.contains('\n') || argv.operands.is_empty() {
        return None;
    }
    Some(argv)
}

/// The output shape an argv asks for. `-n` does not change `-l` or `-c`
/// output in either tool.
fn shape(argv: &ToolArgv) -> Shape {
    if argv.files {
        Shape::Files
    } else if argv.count {
        Shape::Counts
    } else {
        Shape::Lines {
            numbered: argv.line_numbers,
        }
    }
}

/// grep's argv as a sym search: recursive (grep answers a bare directory
/// operand with an error), one matching mode, one of `-l` / `-c`, and a
/// literal pattern -- grep's BRE and ERE are not Rust regex, but a literal
/// means the same to both. `-i` and `-w` also need an ASCII pattern, and
/// `--include` a glob every grep reads alike (`include_is_plain`).
fn parse_grep(args: &[OsString]) -> Option<SymSearch> {
    let argv: ToolArgv = read_argv(Tool::Grep, args)?;
    if !argv.recursive || (argv.extended && argv.fixed) || (argv.files && argv.count) {
        return None;
    }
    let special: &[char] = if argv.fixed {
        &[]
    } else if argv.extended {
        &[
            '\\', '.', '[', ']', '*', '^', '$', '+', '?', '(', ')', '{', '}', '|',
        ]
    } else {
        &['\\', '.', '[', '*', '^', '$']
    };
    if argv.pattern.contains(special) {
        return None;
    }
    let ascii_lines: bool = argv.ignore_case || argv.word;
    if ascii_lines && !argv.pattern.is_ascii() {
        return None;
    }
    if !argv.includes.iter().all(|g| include_is_plain(g)) {
        return None;
    }
    let (pattern, fixed_strings): (String, bool) = if argv.ignore_case {
        (format!("(?i){}", escape_regex(&argv.pattern)), false)
    } else {
        (argv.pattern.clone(), true)
    };
    Some(SymSearch {
        pattern,
        fixed_strings,
        shape: shape(&argv),
        word: argv.word.then(|| WordFilter {
            literal: argv.pattern.clone(),
            ignore_case: argv.ignore_case,
        }),
        ascii_lines,
        includes: argv.includes,
        operands: argv.operands,
        hidden: false,
        no_ignore: false,
        globs: Vec::new(),
        types: Vec::new(),
    })
}

/// rg's argv as a sym search. rg's regex is the engine sym's matcher is
/// built on, so a regex pattern is taken as-is, and `-i` / `-w` become the
/// pattern rg's matcher builds for them: case-insensitive, and wrapped in
/// half word boundaries (ripgrep's `-w` since 14). `-l` with `-c` is left
/// to rg.
fn parse_rg(args: &[OsString]) -> Option<SymSearch> {
    let argv: ToolArgv = read_argv(Tool::Rg, args)?;
    if argv.files && argv.count {
        return None;
    }
    let rewrite: bool = argv.ignore_case || argv.word;
    let mut pattern: String = if argv.fixed && rewrite {
        escape_regex(&argv.pattern)
    } else {
        argv.pattern.clone()
    };
    if rewrite {
        // rg reads the pattern as `(?:PATTERN)` text -- `a)|(b` is valid to
        // it -- then applies -i and -w to the whole parsed regex. Wrapping
        // the same text again keeps -i and -w over all of it; a pattern rg
        // rejects must be rejected before the extra group could balance it.
        grep_regex::RegexMatcher::new(&pattern).ok()?;
        pattern = format!("(?:{pattern})");
        if argv.word {
            pattern = format!(r"\b{{start-half}}(?:{pattern})\b{{end-half}}");
        }
        if argv.ignore_case {
            pattern = format!("(?i){pattern}");
        }
    }
    Some(SymSearch {
        pattern,
        fixed_strings: argv.fixed && !rewrite,
        shape: shape(&argv),
        word: None,
        ascii_lines: false,
        includes: Vec::new(),
        operands: argv.operands,
        hidden: argv.hidden,
        no_ignore: argv.no_ignore,
        globs: argv.globs,
        types: argv.types,
    })
}

/// A literal as a Rust regex matching exactly it: every character the regex
/// syntax treats as special is escaped.
fn escape_regex(literal: &str) -> String {
    let mut out: String = String::with_capacity(literal.len());
    for c in literal.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Record the one pattern a search may carry. A second pattern (`-e a -e b`)
/// is a form sym does not answer, so it returns `None`.
fn set_once(slot: &mut Option<String>, value: &str) -> Option<()> {
    if slot.is_some() {
        return None;
    }
    *slot = Some(value.to_string());
    Some(())
}

/// Print sym's answer the way the tool prints it, exiting 1 when nothing
/// matched (grep `-c` prints zero counts and still exits 1). A closed pipe
/// (`legion grep ... | head -1`) ends quietly with the status a
/// SIGPIPE-killed tool reports, instead of an error.
fn print_answer(lines: &[String], matched: bool) -> Result<()> {
    let written: std::io::Result<()> = (|| {
        let mut out = std::io::stdout().lock();
        for line in lines {
            writeln!(out, "{line}")?;
        }
        out.flush()
    })();
    match written {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Err(LegionError::ExitWith(141)),
        Err(e) => Err(LegionError::Io(e)),
        Ok(()) if !matched => Err(LegionError::ExitWith(1)),
        Ok(()) => Ok(()),
    }
}

/// Run the real tool with the arguments untouched and stdio inherited, and
/// pass its exit status through. A tool that cannot be started reports the
/// way a shell would (127 not found, 126 not runnable).
fn run_real_tool(tool: Tool, args: &[OsString]) -> Result<()> {
    let program: &str = tool.program();
    match Command::new(program).args(args).status() {
        Ok(status) => pass_through(status),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("{program}: command not found");
            Err(LegionError::ExitWith(127))
        }
        Err(e) => {
            eprintln!("{program}: {e}");
            Err(LegionError::ExitWith(126))
        }
    }
}

/// Map the tool's exit status onto legion's, unchanged.
fn pass_through(status: ExitStatus) -> Result<()> {
    let code: i32 = exit_code(status);
    if code == 0 {
        Ok(())
    } else {
        Err(LegionError::ExitWith(code))
    }
}

/// The exit code a shell would report: the code itself, or 128 plus the
/// signal that killed the tool.
fn exit_code(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    /// A plain search: numbered lines, no filters.
    fn search(pattern: &str, fixed: bool, operands: &[&str]) -> SymSearch {
        SymSearch {
            pattern: pattern.to_string(),
            fixed_strings: fixed,
            operands: operands.iter().map(|s| s.to_string()).collect(),
            shape: Shape::Lines { numbered: true },
            word: None,
            ascii_lines: false,
            includes: Vec::new(),
            hidden: false,
            no_ignore: false,
            globs: Vec::new(),
            types: Vec::new(),
        }
    }

    fn with_shape(mut s: SymSearch, shape: Shape) -> SymSearch {
        s.shape = shape;
        s
    }

    #[test]
    fn run_from_argv_claims_only_grep_and_rg() {
        assert!(run_from_argv(&os(&["legion", "recall"])).is_none());
        assert!(run_from_argv(&os(&["legion"])).is_none());
        assert!(run_from_argv(&os(&["legion", "-v", "grep"])).is_none());
    }

    #[test]
    fn grep_recursive_numbered_literal_is_answerable() {
        assert_eq!(
            parse_grep(&os(&["-rn", "needle", "src"])),
            Some(search("needle", true, &["src"]))
        );
        assert_eq!(
            parse_grep(&os(&["-r", "-n", "-e", "-v", "a", "b"])),
            Some(search("-v", true, &["a", "b"]))
        );
        assert_eq!(
            parse_grep(&os(&["--recursive", "--line-number", "--", "-x", "d"])),
            Some(search("-x", true, &["d"]))
        );
        assert_eq!(
            parse_grep(&os(&["-rnF", "a.b[", "d"])),
            Some(search("a.b[", true, &["d"]))
        );
        // BRE: + ? | ( ) { } are literal characters.
        assert_eq!(
            parse_grep(&os(&["-rn", "f(x)+", "d"])),
            Some(search("f(x)+", true, &["d"]))
        );
    }

    #[test]
    fn grep_output_shapes_follow_the_flags() {
        let plain = |args: &[&str]| parse_grep(&os(args)).map(|s| s.shape);
        assert_eq!(
            plain(&["-r", "x", "d"]),
            Some(Shape::Lines { numbered: false })
        );
        assert_eq!(plain(&["-rl", "x", "d"]), Some(Shape::Files));
        assert_eq!(plain(&["-rln", "x", "d"]), Some(Shape::Files));
        assert_eq!(plain(&["-rc", "x", "d"]), Some(Shape::Counts));
        assert_eq!(plain(&["-r", "--count", "x", "d"]), Some(Shape::Counts));
        assert_eq!(
            plain(&["-r", "--files-with-matches", "x", "d"]),
            Some(Shape::Files)
        );
    }

    #[test]
    fn grep_ignore_case_and_word_build_the_scan_and_filter() {
        let s: SymSearch = parse_grep(&os(&["-rwi", "f(x)+", "d"])).expect("answerable");
        assert_eq!(s.pattern, r"(?i)f\(x\)\+");
        assert!(!s.fixed_strings);
        assert!(s.ascii_lines);
        assert_eq!(
            s.word,
            Some(WordFilter {
                literal: "f(x)+".to_string(),
                ignore_case: true,
            })
        );
        let w: SymSearch = parse_grep(&os(&["-rw", "needle", "d"])).expect("answerable");
        assert_eq!(w.pattern, "needle");
        assert!(w.fixed_strings && w.ascii_lines);
        let inc: SymSearch = parse_grep(&os(&[
            "-r",
            "--include=*.rs",
            "--include=Makefile",
            "x",
            "d",
        ]))
        .expect("answerable");
        assert_eq!(inc.includes, vec!["*.rs", "Makefile"]);
    }

    #[test]
    fn grep_v_is_invert_match_and_goes_to_the_tool() {
        // -v means invert-match to grep; sym cannot answer an inverted
        // search, so the parse declines and the real grep runs.
        assert_eq!(parse_grep(&os(&["-rnv", "x", "d"])), None);
        assert_eq!(parse_grep(&os(&["-v", "-rn", "x", "d"])), None);
    }

    #[test]
    fn grep_forms_sym_cannot_answer_exactly_are_declined() {
        let declined: [&[&str]; 14] = [
            &["-n", "x", "d"],                   // not recursive
            &["-rn", "x"],                       // no operand: stdin or cwd by flavor
            &["-rn", "a.b", "d"],                // BRE metacharacter
            &["-rnE", "a|b", "d"],               // ERE metacharacter
            &["-rnEF", "x", "d"],                // conflicting modes
            &["-rlc", "x", "d"],                 // -l with -c prints by flavor
            &["-rni", "caf\u{e9}", "d"],         // case folding of non-ASCII
            &["-rn", "x", "d", "-l"],            // option after operand
            &["-rn", "-e", "a", "-e", "b", "d"], // two patterns
            &["-rn", "", "d"],                   // empty pattern
            &["-r", "--include=s*", "x", "d"],   // BSD matches the whole path
            &["-r", "--include=*.[ch]", "x", "d"],
            &["-r", "--include", "*.rs", "x", "d"],
            &["--color=always", "-rn", "x", "d"],
        ];
        for args in declined {
            assert_eq!(parse_grep(&os(args)), None, "{args:?}");
        }
    }

    #[test]
    fn rg_search_is_answerable_with_regex() {
        assert_eq!(
            parse_rg(&os(&["-n", r"fn \w+", "src"])),
            Some(search(r"fn \w+", false, &["src"]))
        );
        // rg reads options after operands.
        assert_eq!(
            parse_rg(&os(&["x.y", "src", "-nF"])),
            Some(search("x.y", true, &["src"]))
        );
        assert_eq!(
            parse_rg(&os(&["-n", "--regexp=-x", "a", "b"])),
            Some(search("-x", false, &["a", "b"]))
        );
        assert_eq!(
            parse_rg(&os(&["x", "src"])),
            Some(with_shape(
                search("x", false, &["src"]),
                Shape::Lines { numbered: false }
            ))
        );
        assert_eq!(
            parse_rg(&os(&["-ln", "x", "src"])),
            Some(with_shape(search("x", false, &["src"]), Shape::Files))
        );
        assert_eq!(
            parse_rg(&os(&["--count", "x", "src"])),
            Some(with_shape(search("x", false, &["src"]), Shape::Counts))
        );
    }

    #[test]
    fn rg_ignore_case_and_word_become_rgs_own_regex() {
        let s: SymSearch = parse_rg(&os(&["-i", "fn ROUTE", "d"])).expect("answerable");
        assert_eq!(s.pattern, "(?i)(?:fn ROUTE)");
        let w: SymSearch = parse_rg(&os(&["-wF", "f(x)+", "d"])).expect("answerable");
        assert_eq!(w.pattern, r"\b{start-half}(?:(?:f\(x\)\+))\b{end-half}");
        assert!(!w.fixed_strings);
        // rg takes `a)|(?:b` as `(?:a)|(?:b)`; -w must bound the whole
        // alternation, not one side of it.
        let alt: SymSearch = parse_rg(&os(&["-w", "a)|(?:b", "d"])).expect("answerable");
        assert_eq!(alt.pattern, r"\b{start-half}(?:(?:a)|(?:b))\b{end-half}");
        // A pattern rg rejects is not made valid by the extra group:
        // `(?:x))((y)` is unbalanced, `(?:(?:x))((y))` is not.
        assert_eq!(parse_rg(&os(&["-w", "x))((y", "d"])), None);
        assert_eq!(parse_rg(&os(&["-i", "x))((y", "d"])), None);
    }

    #[test]
    fn rg_file_selection_flags_are_read() {
        let s: SymSearch = parse_rg(&os(&[
            "--hidden",
            "--no-ignore",
            "-g",
            "*.rs",
            "--glob=!sub",
            "-gfoo",
            "-t",
            "rust",
            "--type=md",
            "x",
            "d",
        ]))
        .expect("answerable");
        assert!(s.hidden && s.no_ignore);
        assert_eq!(s.globs, vec!["*.rs", "!sub", "foo"]);
        assert_eq!(s.types, vec!["rust", "md"]);
    }

    #[test]
    fn rg_forms_sym_cannot_answer_exactly_are_declined() {
        let declined: [&[&str]; 6] = [
            &["-n", "x"], // no operand
            &["-n", "-S", "x", "d"],
            &["-nv", "x", "d"],
            &["-lc", "x", "d"],
            &["-n", "--hidden=yes", "x", "d"],
            &["-n", "-e", "a", "-e", "b", "d"],
        ];
        for args in declined {
            assert_eq!(parse_rg(&os(args)), None, "{args:?}");
        }
    }

    #[test]
    fn escape_regex_makes_every_special_character_literal() {
        let literal: &str = r"a.b*c(d)+[e]{f}|g^h$i\j?k#l&m-n~o";
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), format!("{literal}\nabc\n")).expect("write");
        let s = search(&format!("^{}$", escape_regex(literal)), false, &["."]);
        let hits: Vec<etc::ContentHit> =
            scan(&s, dir.path(), &["a.txt".to_string()]).expect("valid regex");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].text, literal);
    }

    #[test]
    fn has_word_needs_non_word_characters_around_the_literal() {
        let w = |literal: &str, ignore_case: bool| WordFilter {
            literal: literal.to_string(),
            ignore_case,
        };
        assert!(has_word("let needle = 1;", &w("needle", false)));
        assert!(has_word("needle", &w("needle", false)));
        // The first occurrence is inside a word; the second is whole.
        assert!(has_word("xneedle needle", &w("needle", false)));
        assert!(!has_word("Needles xneedle", &w("needle", true)));
        assert!(has_word("A NEEDLE.", &w("needle", true)));
        assert!(!has_word("A NEEDLE.", &w("needle", false)));
        assert!(has_word("f(x)+ needle -v", &w("-v", false)));
        assert!(!has_word("a_needle", &w("needle", false)));
    }

    #[test]
    fn include_matches_base_name_by_suffix_or_whole_name() {
        assert!(include_is_plain("*.rs"));
        assert!(include_is_plain("Makefile"));
        assert!(include_is_plain("*"));
        assert!(!include_is_plain(""));
        assert!(!include_is_plain("a*"));
        assert!(!include_is_plain("*.r?"));
        assert!(!include_is_plain("src/*.rs"));
        assert!(include_matches("*.rs", "a.rs"));
        assert!(!include_matches("*.rs", "a.rsx"));
        assert!(include_matches("Makefile", "Makefile"));
        assert!(!include_matches("Makefile", "Makefile.am"));
    }

    #[test]
    fn operands_outside_watched_repos_or_not_directories_are_declined() {
        let watched = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(watched.path().join("src")).expect("mkdir");
        std::fs::write(watched.path().join("f.txt"), "x\n").expect("write");
        let workdirs: Vec<PathBuf> =
            vec![std::fs::canonicalize(watched.path()).expect("canonicalize")];
        let src: String = watched.path().join("src").display().to_string();
        assert!(checked_operand(Tool::Grep, &src, &workdirs).is_some());
        let out: String = outside.path().display().to_string();
        assert!(checked_operand(Tool::Grep, &out, &workdirs).is_none());
        let file: String = watched.path().join("f.txt").display().to_string();
        assert!(checked_operand(Tool::Grep, &file, &workdirs).is_none());
        assert!(checked_operand(Tool::Grep, "-", &workdirs).is_none());
        assert!(checked_operand(Tool::Grep, &format!("{src}/"), &workdirs).is_none());
        // rg needs a git checkout: outside one it ignores .gitignore.
        assert!(checked_operand(Tool::Rg, &src, &workdirs).is_none());
    }

    #[test]
    fn grep_tree_with_git_dir_is_the_tools() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join(".git")).expect("mkdir");
        let op: String = dir.path().display().to_string();
        let s = search("x", true, &[op.as_str()]);
        assert!(tool_files(Tool::Grep, &s, &op, dir.path(), dir.path()).is_none());
        // rg skips hidden .git, so the same tree is fine for rg.
        assert_eq!(
            tool_files(Tool::Rg, &s, &op, dir.path(), dir.path()),
            Some(Vec::new())
        );
    }

    #[test]
    fn rg_tree_with_rgignore_is_the_tools() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("sub")).expect("mkdir");
        std::fs::write(dir.path().join("sub/.rgignore"), "*.txt\n").expect("write");
        let op: String = dir.path().display().to_string();
        let s = search("x", true, &[op.as_str()]);
        assert!(tool_files(Tool::Rg, &s, &op, dir.path(), dir.path()).is_none());
    }

    #[test]
    fn tool_files_lists_what_each_tool_searches() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(".gitignore"), "*.log\n").expect("write");
        std::fs::create_dir(dir.path().join("sub")).expect("mkdir");
        std::fs::write(dir.path().join("sub/a.rs"), "x\n").expect("write");
        std::fs::write(dir.path().join("b.log"), "x\n").expect("write");
        std::fs::write(dir.path().join(".hidden.rs"), "x\n").expect("write");
        let op: String = dir.path().display().to_string();
        let mut grep = search("x", true, &[op.as_str()]);
        assert_eq!(
            tool_files(Tool::Grep, &grep, &op, dir.path(), dir.path()),
            Some(vec![
                ".gitignore".to_string(),
                ".hidden.rs".to_string(),
                "b.log".to_string(),
                "sub/a.rs".to_string(),
            ])
        );
        grep.includes = vec!["*.rs".to_string()];
        assert_eq!(
            tool_files(Tool::Grep, &grep, &op, dir.path(), dir.path()),
            Some(vec![".hidden.rs".to_string(), "sub/a.rs".to_string()])
        );
        // A type (like a glob) admits a hidden file it names, as in rg.
        let mut rg = search("x", true, &[op.as_str()]);
        rg.types = vec!["rust".to_string()];
        assert_eq!(
            tool_files(Tool::Rg, &rg, &op, dir.path(), dir.path()),
            Some(vec![".hidden.rs".to_string(), "sub/a.rs".to_string()])
        );
        rg.types = vec!["no-such-type".to_string()];
        assert_eq!(tool_files(Tool::Rg, &rg, &op, dir.path(), dir.path()), None);
    }

    #[test]
    fn scan_declines_crlf_and_binary_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), "needle\r\n").expect("write");
        let s = search("needle", true, &["."]);
        let one = |name: &str| vec![name.to_string()];
        assert!(scan(&s, dir.path(), &one("a.txt")).is_none());

        let bin = tempfile::tempdir().expect("tempdir");
        std::fs::write(bin.path().join("b.bin"), b"needle\0\n").expect("write");
        assert!(scan(&s, bin.path(), &one("b.bin")).is_none());

        let ok = tempfile::tempdir().expect("tempdir");
        std::fs::write(ok.path().join("c.txt"), "a\nneedle here\n").expect("write");
        let hits: Vec<etc::ContentHit> = scan(&s, ok.path(), &one("c.txt")).expect("answerable");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].repo, "c.txt");
        assert_eq!(hits[0].line, 2);
        assert_eq!(hits[0].text, "needle here");
    }

    #[test]
    fn scan_searches_exactly_the_listed_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(".gitignore"), "ignored.txt\n").expect("write");
        std::fs::write(dir.path().join("ignored.txt"), "needle\n").expect("write");
        std::fs::write(dir.path().join(".hidden"), "needle\n").expect("write");
        std::fs::write(dir.path().join("seen.txt"), "needle\n").expect("write");
        let s = search("needle", true, &["."]);
        let listed: Vec<String> = vec![".hidden".to_string(), "ignored.txt".to_string()];
        let hits: Vec<etc::ContentHit> = scan(&s, dir.path(), &listed).expect("scan");
        let paths: Vec<&str> = hits.iter().map(|h| h.repo.as_str()).collect();
        assert_eq!(paths, vec![".hidden", "ignored.txt"]);
    }

    #[test]
    fn scan_grep_declines_non_ascii_candidates_and_filters_words() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), "needle\nxneedle\n").expect("write");
        std::fs::write(dir.path().join("b.txt"), "\u{e9}needle\n").expect("write");
        let s: SymSearch = parse_grep(&os(&["-rw", "needle", "d"])).expect("answerable");
        let hits: Vec<etc::ContentHit> =
            scan(&s, dir.path(), &["a.txt".to_string()]).expect("ascii");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 1);
        assert!(scan(&s, dir.path(), &["b.txt".to_string()]).is_none());
    }

    #[test]
    fn render_prints_each_shape() {
        let hit = |rel: &str, line: u64, text: &str| etc::ContentHit {
            repo: rel.to_string(),
            path: String::new(),
            line,
            text: text.to_string(),
        };
        let hits: Vec<etc::ContentHit> = vec![hit("a.rs", 1, "x"), hit("a.rs", 3, "xx")];
        let files: Vec<String> = vec!["a.rs".to_string(), "b.rs".to_string()];
        let lines_of = |tool: Tool, shape: Shape| {
            let s = with_shape(search("x", false, &["src"]), shape);
            let mut lines: Vec<String> = Vec::new();
            render(tool, &s, "src", &files, &hits, &mut lines).expect("render");
            lines
        };
        assert_eq!(
            lines_of(Tool::Rg, Shape::Lines { numbered: true }),
            vec!["src/a.rs:1:x", "src/a.rs:3:xx"]
        );
        assert_eq!(
            lines_of(Tool::Rg, Shape::Lines { numbered: false }),
            vec!["src/a.rs:x", "src/a.rs:xx"]
        );
        assert_eq!(lines_of(Tool::Grep, Shape::Files), vec!["src/a.rs"]);
        assert_eq!(lines_of(Tool::Rg, Shape::Counts), vec!["src/a.rs:2"]);
        assert_eq!(
            lines_of(Tool::Grep, Shape::Counts),
            vec!["src/a.rs:2", "src/b.rs:0"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn exit_code_reports_signal_as_128_plus() {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(exit_code(ExitStatus::from_raw(13)), 141);
        assert_eq!(exit_code(ExitStatus::from_raw(2 << 8)), 2);
    }
}
