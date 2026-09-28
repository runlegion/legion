//! `legion grep` and `legion rg` (#1334): each takes its own tool's
//! arguments exactly as typed and returns the answer that tool would give.
//!
//! sym answers when it can answer the search fully; everything else runs the
//! real tool with the arguments untouched, its output and exit code passed
//! through. Neither subcommand refuses a search and neither turns a sym
//! failure into an error of legion's own: every doubt falls back.
//!
//! The riskiest call is "sym answers this fully". A wrong yes returns fewer
//! matches than the tool would, so the accepted set is deliberately narrow:
//! - the output shape must be `path:line:text`, the one sym prints: a
//!   recursive, line-numbered search over explicit directory operands;
//! - the pattern must mean the same thing to sym's matcher as to the tool:
//!   a literal for grep (its BRE/ERE dialects are not Rust regex), any
//!   pattern for rg (same regex engine);
//! - every operand must be a real directory inside a watched repo;
//! - the tree must be one sym walks the way the tool does (see
//!   `tree_matches_tool`), and the scan must come back with nothing
//!   skipped, capped, or binary, and no matched file holding a CR or bytes
//!   that are not UTF-8.
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

/// A search sym can answer: one pattern, how to match it, and the directory
/// operands exactly as the agent typed them (output paths are built from
/// them, the way the tool builds its own).
#[derive(Debug, PartialEq, Eq)]
struct SymSearch {
    pattern: String,
    fixed_strings: bool,
    operands: Vec<String>,
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

/// Answer from sym when it can answer fully, otherwise run the real tool.
fn run(tool: Tool, args: &[OsString]) -> Result<()> {
    let answer: Option<Vec<String>> = sym_answer(tool, args);
    match answer {
        Some(lines) => print_answer(&lines),
        None => run_real_tool(tool, args),
    }
}

/// The whole sym attempt. `None` means "run the real tool": any parse doubt,
/// any error, and any sign the scan saw less than the tool would.
fn sym_answer(tool: Tool, args: &[OsString]) -> Option<Vec<String>> {
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
            // heading format, which is not `path:line:text`.
            if std::env::var_os("RIPGREP_CONFIG_PATH").is_some_and(|v| !v.is_empty())
                || std::io::stdout().is_terminal()
            {
                return None;
            }
            parse_rg(args)?
        }
    };
    let workdirs: Vec<PathBuf> = watched_workdirs()?;
    let mut lines: Vec<String> = Vec::new();
    for operand in &search.operands {
        let root: PathBuf = checked_operand(tool, operand, &workdirs)?;
        let mut hits: Vec<etc::ContentHit> = scan(tool, &search, &root)?;
        hits.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
        for hit in hits {
            let shown: PathBuf = Path::new(operand).join(&hit.path);
            lines.push(format!("{}:{}:{}", shown.to_str()?, hit.line, hit.text));
        }
    }
    Some(lines)
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
    if !tree_matches_tool(tool, &root) {
        return None;
    }
    Some(root)
}

/// Walk `root` the way the tool will and confirm sym's walk visits the same
/// files. grep -r reads every file, hidden and gitignored alike, and reads
/// `.git` too, which sym never does -- so a tree holding a `.git` entry is
/// the tool's. Symlinks and special files are handled differently across
/// grep flavors, so any of them is the tool's too. rg honors `.rgignore`,
/// which sym's walk does not read.
fn tree_matches_tool(tool: Tool, root: &Path) -> bool {
    let mut builder: ignore::WalkBuilder = ignore::WalkBuilder::new(root);
    if tool == Tool::Grep {
        builder.standard_filters(false);
    }
    for entry in builder.build() {
        let Ok(entry) = entry else {
            return false;
        };
        let Some(file_type) = entry.file_type() else {
            return false;
        };
        if !(file_type.is_file() || file_type.is_dir()) {
            return false;
        }
        match tool {
            Tool::Grep if entry.file_name() == OsStr::new(".git") => return false,
            Tool::Rg if file_type.is_dir() && entry.path().join(".rgignore").exists() => {
                return false;
            }
            _ => {}
        }
    }
    true
}

/// Scan one operand with sym's content search, scoped the way the tool
/// scopes it. `None` when the scan errors or reports anything it did not
/// search the way the tool would.
fn scan(tool: Tool, search: &SymSearch, root: &Path) -> Option<Vec<etc::ContentHit>> {
    let repos: Vec<(String, PathBuf)> = vec![(String::new(), root.to_path_buf())];
    let scope = etc::ContentScope {
        repos: &repos,
        ext: None,
        fixed_strings: search.fixed_strings,
        // grep -r searches hidden and ignored files; rg skips both.
        include_hidden: tool == Tool::Grep,
        no_ignore: tool == Tool::Grep,
        max_file_size: etc::MAX_FILE_SIZE,
        max_hits: etc::MAX_HITS,
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
        if checked.contains(&hit.path.as_str()) {
            continue;
        }
        let bytes: Vec<u8> = std::fs::read(root.join(&hit.path)).ok()?;
        if bytes.contains(&b'\r') || std::str::from_utf8(&bytes).is_err() {
            return None;
        }
        checked.push(hit.path.as_str());
    }
    Some(result.hits)
}

/// Parse grep's argv into a sym search, or `None` for any form outside the
/// accepted set. Options must all precede the first operand: BSD grep stops
/// option parsing there and GNU grep does not, so a later option means
/// different things to the two.
fn parse_grep(args: &[OsString]) -> Option<SymSearch> {
    let args: Vec<&str> = args.iter().map(|a| a.to_str()).collect::<Option<_>>()?;
    let mut recursive = false;
    let mut line_numbers = false;
    let mut extended = false;
    let mut fixed = false;
    let mut pattern: Option<String> = None;
    let mut positionals: Vec<String> = Vec::new();
    let mut i: usize = 0;
    let mut options_done = false;
    while i < args.len() {
        let arg: &str = args[i];
        i += 1;
        if options_done || !arg.starts_with('-') || arg == "-" {
            positionals.push(arg.to_string());
            continue;
        }
        // An option after an operand: GNU reads it, BSD reads a file name.
        if !positionals.is_empty() {
            return None;
        }
        if arg == "--" {
            options_done = true;
            continue;
        }
        if let Some(long) = arg.strip_prefix("--") {
            match long {
                "recursive" => recursive = true,
                "line-number" => line_numbers = true,
                "extended-regexp" => extended = true,
                "fixed-strings" => fixed = true,
                "regexp" => {
                    set_once(&mut pattern, args.get(i)?)?;
                    i += 1;
                }
                other => {
                    let value: &str = other.strip_prefix("regexp=")?;
                    set_once(&mut pattern, value)?;
                }
            }
            continue;
        }
        let cluster: &str = &arg[1..];
        for (pos, flag) in cluster.char_indices() {
            match flag {
                'r' | 'R' => recursive = true,
                'n' => line_numbers = true,
                'E' => extended = true,
                'F' => fixed = true,
                'H' => {}
                'e' => {
                    let rest: &str = &cluster[pos + 1..];
                    if rest.is_empty() {
                        set_once(&mut pattern, args.get(i)?)?;
                        i += 1;
                    } else {
                        set_once(&mut pattern, rest)?;
                    }
                    break;
                }
                _ => return None,
            }
        }
    }
    if !recursive || !line_numbers || (extended && fixed) {
        return None;
    }
    if pattern.is_none() {
        if positionals.is_empty() {
            return None;
        }
        pattern = Some(positionals.remove(0));
    }
    let pattern: String = pattern?;
    // Only literals mean the same to grep (BRE or ERE) and to sym.
    let special: &[char] = if fixed {
        &[]
    } else if extended {
        &[
            '\\', '.', '[', ']', '*', '^', '$', '+', '?', '(', ')', '{', '}', '|',
        ]
    } else {
        &['\\', '.', '[', '*', '^', '$']
    };
    if pattern.is_empty() || pattern.contains('\n') || pattern.contains(special) {
        return None;
    }
    if positionals.is_empty() {
        return None;
    }
    Some(SymSearch {
        pattern,
        fixed_strings: true,
        operands: positionals,
    })
}

/// Parse rg's argv into a sym search, or `None` for any form outside the
/// accepted set. rg reads options anywhere before `--`, and its regex is the
/// engine sym's matcher is built on, so a regex pattern is accepted as-is.
fn parse_rg(args: &[OsString]) -> Option<SymSearch> {
    let args: Vec<&str> = args.iter().map(|a| a.to_str()).collect::<Option<_>>()?;
    let mut line_numbers = false;
    let mut fixed = false;
    let mut pattern: Option<String> = None;
    let mut positionals: Vec<String> = Vec::new();
    let mut i: usize = 0;
    let mut options_done = false;
    while i < args.len() {
        let arg: &str = args[i];
        i += 1;
        if options_done || !arg.starts_with('-') || arg == "-" {
            positionals.push(arg.to_string());
            continue;
        }
        if arg == "--" {
            options_done = true;
            continue;
        }
        if let Some(long) = arg.strip_prefix("--") {
            match long {
                "line-number" => line_numbers = true,
                "fixed-strings" => fixed = true,
                "regexp" => {
                    set_once(&mut pattern, args.get(i)?)?;
                    i += 1;
                }
                other => {
                    let value: &str = other.strip_prefix("regexp=")?;
                    set_once(&mut pattern, value)?;
                }
            }
            continue;
        }
        let cluster: &str = &arg[1..];
        for (pos, flag) in cluster.char_indices() {
            match flag {
                'n' => line_numbers = true,
                'F' => fixed = true,
                'e' => {
                    let rest: &str = &cluster[pos + 1..];
                    if rest.is_empty() {
                        set_once(&mut pattern, args.get(i)?)?;
                        i += 1;
                    } else {
                        set_once(&mut pattern, rest)?;
                    }
                    break;
                }
                _ => return None,
            }
        }
    }
    if !line_numbers {
        return None;
    }
    if pattern.is_none() {
        if positionals.is_empty() {
            return None;
        }
        pattern = Some(positionals.remove(0));
    }
    let pattern: String = pattern?;
    if pattern.is_empty() || pattern.contains('\n') || positionals.is_empty() {
        return None;
    }
    Some(SymSearch {
        pattern,
        fixed_strings: fixed,
        operands: positionals,
    })
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
/// matched. A closed pipe (`legion grep ... | head -1`) ends quietly with
/// the status a SIGPIPE-killed tool reports, instead of an error.
fn print_answer(lines: &[String]) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in lines {
        match writeln!(out, "{line}") {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                return Err(LegionError::ExitWith(141));
            }
            Err(e) => return Err(LegionError::Io(e)),
        }
    }
    match out.flush() {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
            return Err(LegionError::ExitWith(141));
        }
        Err(e) => return Err(LegionError::Io(e)),
    }
    if lines.is_empty() {
        return Err(LegionError::ExitWith(1));
    }
    Ok(())
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

    fn search(pattern: &str, fixed: bool, operands: &[&str]) -> SymSearch {
        SymSearch {
            pattern: pattern.to_string(),
            fixed_strings: fixed,
            operands: operands.iter().map(|s| s.to_string()).collect(),
        }
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
    fn grep_v_is_invert_match_and_goes_to_the_tool() {
        // -v means invert-match to grep; sym cannot answer an inverted
        // search, so the parse declines and the real grep runs.
        assert_eq!(parse_grep(&os(&["-rnv", "x", "d"])), None);
        assert_eq!(parse_grep(&os(&["-v", "-rn", "x", "d"])), None);
    }

    #[test]
    fn grep_forms_sym_cannot_answer_exactly_are_declined() {
        let declined: [&[&str]; 11] = [
            &["-n", "x", "d"],                   // not recursive
            &["-r", "x", "d"],                   // no line numbers
            &["-rn", "x"],                       // no operand: stdin or cwd by flavor
            &["-rn", "a.b", "d"],                // BRE metacharacter
            &["-rnE", "a|b", "d"],               // ERE metacharacter
            &["-rnEF", "x", "d"],                // conflicting modes
            &["-rni", "x", "d"],                 // case folding differs
            &["-rn", "x", "d", "-l"],            // option after operand
            &["-rn", "-e", "a", "-e", "b", "d"], // two patterns
            &["-rn", "", "d"],                   // empty pattern
            &["--color=always", "-rn", "x", "d"],
        ];
        for args in declined {
            assert_eq!(parse_grep(&os(args)), None, "{args:?}");
        }
    }

    #[test]
    fn rg_numbered_search_is_answerable_with_regex() {
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
    }

    #[test]
    fn rg_forms_sym_cannot_answer_exactly_are_declined() {
        let declined: [&[&str]; 6] = [
            &["x", "src"],           // no -n: rg prints no line numbers when piped
            &["-n", "x"],            // no operand
            &["-n", "-i", "x", "d"], // other flag
            &["-n", "--hidden", "x", "d"],
            &["-nv", "x", "d"],
            &["-n", "-e", "a", "-e", "b", "d"],
        ];
        for args in declined {
            assert_eq!(parse_rg(&os(args)), None, "{args:?}");
        }
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
        assert!(!tree_matches_tool(Tool::Grep, dir.path()));
        // rg skips hidden .git, so the same tree is fine for rg.
        assert!(tree_matches_tool(Tool::Rg, dir.path()));
    }

    #[test]
    fn rg_tree_with_rgignore_is_the_tools() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("sub")).expect("mkdir");
        std::fs::write(dir.path().join("sub/.rgignore"), "*.txt\n").expect("write");
        assert!(!tree_matches_tool(Tool::Rg, dir.path()));
    }

    #[test]
    fn scan_declines_crlf_and_binary_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), "needle\r\n").expect("write");
        let s = search("needle", true, &["."]);
        assert!(scan(Tool::Grep, &s, dir.path()).is_none());

        let bin = tempfile::tempdir().expect("tempdir");
        std::fs::write(bin.path().join("b.bin"), b"needle\0\n").expect("write");
        assert!(scan(Tool::Grep, &s, bin.path()).is_none());

        let ok = tempfile::tempdir().expect("tempdir");
        std::fs::write(ok.path().join("c.txt"), "a\nneedle here\n").expect("write");
        let hits: Vec<etc::ContentHit> = scan(Tool::Grep, &s, ok.path()).expect("answerable");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 2);
        assert_eq!(hits[0].text, "needle here");
    }

    #[test]
    fn scan_grep_includes_hidden_and_ignored_files_rg_skips_them() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(".gitignore"), "ignored.txt\n").expect("write");
        std::fs::write(dir.path().join("ignored.txt"), "needle\n").expect("write");
        std::fs::write(dir.path().join(".hidden"), "needle\n").expect("write");
        std::fs::write(dir.path().join("seen.txt"), "needle\n").expect("write");
        let s = search("needle", true, &["."]);
        let grep_hits: Vec<etc::ContentHit> = scan(Tool::Grep, &s, dir.path()).expect("grep");
        assert_eq!(grep_hits.len(), 3);
        let rg_hits: Vec<etc::ContentHit> = scan(Tool::Rg, &s, dir.path()).expect("rg");
        let paths: Vec<&str> = rg_hits.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, vec!["seen.txt"]);
    }

    #[cfg(unix)]
    #[test]
    fn exit_code_reports_signal_as_128_plus() {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(exit_code(ExitStatus::from_raw(13)), 141);
        assert_eq!(exit_code(ExitStatus::from_raw(2 << 8)), 2);
    }
}
