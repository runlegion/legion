//! `legion grep` / `legion rg` (#1334): each subcommand's output and exit
//! code against the real tool's for the same arguments.
//!
//! Three kinds of test, because an output comparison alone cannot tell a
//! sym answer from a correct fallback:
//! - parity: legion and the real tool, same argv, same cwd, compared;
//! - sym-only: `PATH` holds no grep or rg at all, so only a sym answer can
//!   produce the tool's output -- run on the forms sym claims to answer;
//! - stub: a fake grep/rg first on `PATH` echoes its argv and stdin, writes
//!   to stderr, and exits with a chosen code, proving the fallback hands the
//!   tool the arguments untouched and passes its stderr and exit through.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use crate::common::legion_cmd;

/// A watched git checkout holding the fixture tree, plus the data dir whose
/// watch.toml names it.
struct Fixture {
    data: tempfile::TempDir,
    repo: tempfile::TempDir,
}

fn write(root: &Path, rel: &str, content: &str) {
    let path: PathBuf = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(path, content).expect("write fixture");
}

fn fixture() -> Fixture {
    let data = tempfile::tempdir().expect("data dir");
    let repo = tempfile::tempdir().expect("repo dir");
    let init = Command::new("git")
        .current_dir(repo.path())
        .args(["init", "-q"])
        .output()
        .expect("git init");
    assert!(init.status.success(), "git init failed");
    write(repo.path(), ".gitignore", "*.log\n");
    write(
        repo.path(),
        "src/a.rs",
        "fn main() {}\nlet needle = 1;\n// needle again\n",
    );
    write(repo.path(), "src/sub/b.txt", "no\nneedle\n");
    write(repo.path(), "src/.hidden.txt", "needle hidden\n");
    write(repo.path(), "src/ignored.log", "needle ignored\n");
    write(repo.path(), "src/c.md", "f(x)+ needle -v\n");
    write(repo.path(), "src/d.txt", "NEEDLE upper\nxneedle_y\n");
    write(repo.path(), "src/empty.txt", "");
    std::fs::write(
        data.path().join("watch.toml"),
        format!(
            "[[repos]]\nname = \"fixture\"\nworkdir = \"{}\"\n",
            repo.path().display()
        ),
    )
    .expect("seed watch.toml");
    Fixture { data, repo }
}

/// `legion <tool> <args>` from `cwd` with an explicit `PATH`.
fn legion(fx: &Fixture, cwd: &Path, path_env: &str, tool: &str, args: &[&str]) -> Output {
    legion_cmd(fx.data.path())
        .current_dir(cwd)
        .env("PATH", path_env)
        .env_remove("GREP_OPTIONS")
        .env_remove("RIPGREP_CONFIG_PATH")
        .arg(tool)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("spawn legion")
}

/// The real tool, same argv and cwd.
fn real(cwd: &Path, tool: &str, args: &[&str]) -> Output {
    Command::new(tool)
        .current_dir(cwd)
        .env_remove("GREP_OPTIONS")
        .env_remove("RIPGREP_CONFIG_PATH")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("spawn real tool")
}

fn tool_present(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// stdout lines, sorted: grep -r walks in directory order and rg in
/// parallel, so the tools themselves print matches in no fixed order.
fn sorted_lines(out: &Output) -> Vec<String> {
    let mut lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    lines.sort();
    lines
}

fn assert_same(label: &str, ours: &Output, theirs: &Output) {
    assert_eq!(
        sorted_lines(ours),
        sorted_lines(theirs),
        "{label}: stdout differs\nlegion stderr: {}\ntool stderr: {}",
        String::from_utf8_lossy(&ours.stderr),
        String::from_utf8_lossy(&theirs.stderr),
    );
    assert_eq!(
        ours.status.code(),
        theirs.status.code(),
        "{label}: exit code differs"
    );
}

/// Forms sym claims to answer. Each runs three ways: legion with the real
/// tool on PATH, the real tool, and legion with no tool on PATH at all.
/// Only forms GNU and BSD grep answer alike are listed for grep.
const GREP_SYM_FORMS: [&[&str]; 25] = [
    &["-rn", "needle", "src"],
    &["-r", "-n", "needle", "./src"],
    &["-rnF", "f(x)+", "src"],
    &["-rn", "f(x)+", "src"],
    &["-rn", "-e", "-v", "src"],
    &["-rn", "absent-pattern", "src"],
    &["-r", "needle", "src"],
    &["-rl", "needle", "src"],
    &["-rln", "needle", "src"],
    &["-r", "--files-with-matches", "needle", "src"],
    &["-rc", "needle", "src"],
    &["-r", "--count", "needle", "src"],
    &["-rc", "absent-pattern", "src"],
    &["-ri", "NEEDLE", "src"],
    &["-r", "--ignore-case", "-n", "Needle", "src"],
    &["-rE", "needle", "src"],
    &["-rF", "f(x)+", "src"],
    &["-rw", "needle", "src"],
    &["-r", "--word-regexp", "-e", "-v", "src"],
    &["-rwi", "NEEDLE", "src"],
    &["-rwF", "f(x)+", "src"],
    &["-rci", "needle", "src"],
    &["-rn", "--include=*.rs", "needle", "src"],
    &["-rc", "--include=*.txt", "--include=c.md", "needle", "src"],
    &["-rl", "needle", "src", "./src"],
];

const RG_SYM_FORMS: [&[&str]; 30] = [
    &["-n", "needle", "src"],
    &["-n", "needle", "./src"],
    &["-n", r"ne+dle \w+", "src"],
    &["-nF", "f(x)+", "src"],
    &["needle", "src", "-n"],
    &["-n", "absent-pattern", "src"],
    &["needle", "src"],
    &["-l", "needle", "src"],
    &["--files-with-matches", "-n", "needle", "src"],
    &["-c", "needle", "src"],
    &["--count", "needle", "src"],
    &["-c", "absent-pattern", "src"],
    &["-i", "NEEDLE", "src"],
    &["--ignore-case", "-n", "Needle", "src"],
    &["-F", "f(x)+", "src"],
    &["--fixed-strings", "-n", "f(x)+", "src"],
    &["-w", "needle", "src"],
    &["--word-regexp", "-e", "-v", "src"],
    &["-w", "-e", "needle)|(?:NEEDLE", "src"],
    &["-iwc", "NEEDLE", "src"],
    &["-iF", "F(X)+", "src"],
    &["--hidden", "needle", "src"],
    &["--no-ignore", "-l", "needle", "src"],
    &["--hidden", "--no-ignore", "-c", "needle", "src"],
    &["-g", "*.rs", "needle", "src"],
    &["--glob=!sub", "-n", "needle", "src"],
    &["-g", "*.log", "needle", "src"],
    &["-t", "rust", "-n", "needle", "src"],
    &["--type=md", "-c", "needle", "src"],
    &["-l", "-g", "*.txt", "-i", "NEEDLE", "src", "./src"],
];

/// Forms sym declines: each must still give the tool's exact answer.
const GREP_TOOL_FORMS: [&[&str]; 8] = [
    &["-rnv", "needle", "src"],
    &["-rn", "ne.dle", "src"],
    &["-rn", "needle", "."],
    &["-n", "needle", "src/a.rs"],
    &["-rn", "needle", "src/"],
    &["-n", "needle", "src"],
    &["-rlc", "needle", "src"],
    &["-r", "--include=s*", "needle", "src"],
];

const RG_TOOL_FORMS: [&[&str]; 4] = [
    &["-n", "-S", "NEEDLE", "src"],
    &["-nv", "needle", "src"],
    &["-lc", "needle", "src"],
    &["-n", "needle", "src/a.rs"],
];

fn empty_path() -> tempfile::TempDir {
    tempfile::tempdir().expect("empty PATH dir")
}

/// Each form must be answered by sym alone (no tool on PATH, so a fallback
/// exits 127), and where the real tool exists, legion with and without it
/// must match the tool.
fn assert_sym_forms(tool: &str, forms: &[&[&str]]) {
    let fx = fixture();
    let cwd: &Path = fx.repo.path();
    let path_env: String = std::env::var("PATH").unwrap_or_default();
    let no_tools = empty_path();
    let no_tools_path: String = no_tools.path().display().to_string();
    for args in forms {
        let label: String = format!("{tool} {args:?}");
        let sym_only: Output = legion(&fx, cwd, &no_tools_path, tool, args);
        assert!(
            matches!(sym_only.status.code(), Some(0) | Some(1)),
            "{label}: sym did not answer (exit {:?}): {}",
            sym_only.status.code(),
            String::from_utf8_lossy(&sym_only.stderr)
        );
        if tool_present(tool) {
            let theirs: Output = real(cwd, tool, args);
            assert_same(&label, &legion(&fx, cwd, &path_env, tool, args), &theirs);
            assert_same(&format!("{label} (sym only)"), &sym_only, &theirs);
        }
    }
}

/// Each form must give the real tool's answer. Skipped where the tool is
/// not installed.
fn assert_tool_forms(tool: &str, forms: &[&[&str]]) {
    if !tool_present(tool) {
        return;
    }
    let fx = fixture();
    let cwd: &Path = fx.repo.path();
    let path_env: String = std::env::var("PATH").unwrap_or_default();
    for args in forms {
        assert_same(
            &format!("{tool} {args:?}"),
            &legion(&fx, cwd, &path_env, tool, args),
            &real(cwd, tool, args),
        );
    }
}

#[test]
fn grep_sym_forms_match_real_grep_and_need_no_grep() {
    assert_sym_forms("grep", &GREP_SYM_FORMS);
}

#[test]
fn grep_sym_answer_prints_path_line_text() {
    let fx = fixture();
    let no_tools = empty_path();
    let out: Output = legion(
        &fx,
        fx.repo.path(),
        &no_tools.path().display().to_string(),
        "grep",
        &["-rn", "needle", "src"],
    );
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        sorted_lines(&out),
        vec![
            "src/.hidden.txt:1:needle hidden",
            "src/a.rs:2:let needle = 1;",
            "src/a.rs:3:// needle again",
            "src/c.md:1:f(x)+ needle -v",
            "src/d.txt:2:xneedle_y",
            "src/ignored.log:1:needle ignored",
            "src/sub/b.txt:2:needle",
        ]
    );
}

/// sym's answer (no tool on PATH) for `args`, against the lines the tool
/// prints (sorted) and its exit code. The expected lines were taken from
/// rg 15.2 and grep over this fixture; they pin each shape where the real
/// tool is not installed.
fn assert_sym_prints(tool: &str, args: &[&str], expected: &[&str], code: i32) {
    let fx = fixture();
    let no_tools = empty_path();
    let out: Output = legion(
        &fx,
        fx.repo.path(),
        &no_tools.path().display().to_string(),
        tool,
        args,
    );
    assert_eq!(
        out.status.code(),
        Some(code),
        "{tool} {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut want: Vec<String> = expected.iter().map(|l| l.to_string()).collect();
    want.sort();
    assert_eq!(sorted_lines(&out), want, "{tool} {args:?}");
}

#[test]
fn rg_sym_answers_print_what_rg_prints() {
    let cases: [(&[&str], &[&str], i32); 11] = [
        (
            &["needle", "src"],
            &[
                "src/a.rs:let needle = 1;",
                "src/a.rs:// needle again",
                "src/c.md:f(x)+ needle -v",
                "src/d.txt:xneedle_y",
                "src/sub/b.txt:needle",
            ],
            0,
        ),
        (
            &["-c", "needle", "src"],
            &["src/a.rs:2", "src/c.md:1", "src/d.txt:1", "src/sub/b.txt:1"],
            0,
        ),
        (
            &["-ln", "needle", "src"],
            &["src/a.rs", "src/c.md", "src/d.txt", "src/sub/b.txt"],
            0,
        ),
        (
            &["-i", "NEEDLE", "src"],
            &[
                "src/a.rs:let needle = 1;",
                "src/a.rs:// needle again",
                "src/c.md:f(x)+ needle -v",
                "src/d.txt:NEEDLE upper",
                "src/d.txt:xneedle_y",
                "src/sub/b.txt:needle",
            ],
            0,
        ),
        (
            &["-w", "needle", "src"],
            &[
                "src/a.rs:let needle = 1;",
                "src/a.rs:// needle again",
                "src/c.md:f(x)+ needle -v",
                "src/sub/b.txt:needle",
            ],
            0,
        ),
        (
            &["-iwc", "NEEDLE", "src"],
            &["src/a.rs:2", "src/c.md:1", "src/d.txt:1", "src/sub/b.txt:1"],
            0,
        ),
        (
            &["-g", "*.log", "needle", "src"],
            &["src/ignored.log:needle ignored"],
            0,
        ),
        (
            &["-t", "md", "-n", "needle", "src"],
            &["src/c.md:1:f(x)+ needle -v"],
            0,
        ),
        (
            &["--hidden", "-c", "needle", "src"],
            &[
                "src/.hidden.txt:1",
                "src/a.rs:2",
                "src/c.md:1",
                "src/d.txt:1",
                "src/sub/b.txt:1",
            ],
            0,
        ),
        (
            &["--no-ignore", "-l", "needle", "src"],
            &[
                "src/a.rs",
                "src/c.md",
                "src/d.txt",
                "src/ignored.log",
                "src/sub/b.txt",
            ],
            0,
        ),
        (&["-c", "absent-pattern", "src"], &[], 1),
    ];
    for (args, expected, code) in cases {
        assert_sym_prints("rg", args, expected, code);
    }
}

#[test]
fn grep_count_lists_every_file_with_zero_counts() {
    assert_sym_prints(
        "grep",
        &["-rc", "needle", "src"],
        &[
            "src/.hidden.txt:1",
            "src/a.rs:2",
            "src/c.md:1",
            "src/d.txt:1",
            "src/empty.txt:0",
            "src/ignored.log:1",
            "src/sub/b.txt:1",
        ],
        0,
    );
    // Every count zero: grep still prints them, and exits 1.
    assert_sym_prints(
        "grep",
        &["-rc", "absent-pattern", "src"],
        &[
            "src/.hidden.txt:0",
            "src/a.rs:0",
            "src/c.md:0",
            "src/d.txt:0",
            "src/empty.txt:0",
            "src/ignored.log:0",
            "src/sub/b.txt:0",
        ],
        1,
    );
    assert_sym_prints(
        "grep",
        &["-rwi", "NEEDLE", "src"],
        &[
            "src/.hidden.txt:needle hidden",
            "src/a.rs:let needle = 1;",
            "src/a.rs:// needle again",
            "src/c.md:f(x)+ needle -v",
            "src/d.txt:NEEDLE upper",
            "src/ignored.log:needle ignored",
            "src/sub/b.txt:needle",
        ],
        0,
    );
}

#[test]
fn rg_sym_forms_match_real_rg_and_need_no_rg() {
    assert_sym_forms("rg", &RG_SYM_FORMS);
}

#[test]
fn grep_tool_forms_match_real_grep() {
    assert_tool_forms("grep", &GREP_TOOL_FORMS);
}

#[test]
fn rg_tool_forms_match_real_rg() {
    assert_tool_forms("rg", &RG_TOOL_FORMS);
}

/// A stub named `tool` that prints each argument on its own line, then its
/// stdin, writes a marker to stderr, and exits with `$STUB_EXIT`.
fn stub_dir(tool: &str) -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("stub dir");
    let script: PathBuf = dir.path().join(tool);
    std::fs::write(
        &script,
        "#!/bin/sh\nfor a in \"$@\"; do printf 'arg:%s\\n' \"$a\"; done\n\
         while IFS= read -r line; do printf 'stdin:%s\\n' \"$line\"; done\n\
         echo stub-stderr >&2\nexit \"${STUB_EXIT:-0}\"\n",
    )
    .expect("write stub");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    dir
}

fn stub_run(fx: &Fixture, tool: &str, args: &[&str], stdin: &str, exit: &str) -> Output {
    use std::io::Write;
    let stubs = stub_dir(tool);
    let path_env: String = format!("{}:/usr/bin:/bin", stubs.path().display());
    let mut child = legion_cmd(fx.data.path())
        .current_dir(fx.repo.path())
        .env("PATH", path_env)
        .env("STUB_EXIT", exit)
        .arg(tool)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn legion");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("wait legion")
}

fn stub_args(out: &Output) -> Vec<String> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix("arg:"))
        .map(str::to_string)
        .collect()
}

#[test]
fn grep_fallback_hands_grep_the_argv_untouched() {
    let fx = fixture();
    let outside = tempfile::tempdir().expect("outside dir");
    let outside_path: String = outside.path().display().to_string();
    let cases: [Vec<&str>; 6] = [
        // -v is grep's invert-match, never legion's verbose.
        vec!["-v", "needle", "src"],
        // `--` reaches grep: clap would have eaten it.
        vec!["-rnv", "--", "-x", "src"],
        // A leading `--` too: clap drops it before a trailing positional.
        vec!["--", "-x", "src/a.rs"],
        // -h is grep's no-filename, not help.
        vec!["-h", "needle", "src/a.rs"],
        // A path outside every watched repo.
        vec!["-rn", "needle", outside_path.as_str()],
        vec!["--help"],
    ];
    for args in cases {
        let out: Output = stub_run(&fx, "grep", &args, "", "0");
        assert_eq!(out.status.code(), Some(0), "{args:?}");
        assert_eq!(stub_args(&out), args, "argv changed for {args:?}");
    }
}

#[test]
fn grep_fallback_passes_exit_code_and_stderr_through() {
    let fx = fixture();
    for code in ["1", "2"] {
        let out: Output = stub_run(&fx, "grep", &["-v", "x", "src"], "", code);
        assert_eq!(out.status.code(), Some(code.parse::<i32>().expect("code")));
        assert_eq!(String::from_utf8_lossy(&out.stderr), "stub-stderr\n");
    }
}

#[test]
fn grep_reads_standard_input_through_the_tool() {
    let fx = fixture();
    let out: Output = stub_run(&fx, "grep", &["needle"], "a needle\n", "0");
    let stdout: String = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(stdout, "arg:needle\nstdin:a needle\n");
}

#[test]
fn a_sym_error_on_an_answerable_form_runs_the_tool() {
    let fx = fixture();
    // `-n PATTERN DIR` is a form sym takes, but "(" is not a valid regex:
    // the sym error becomes the real rg's run, never legion's own error.
    let out: Output = stub_run(&fx, "rg", &["-n", "(", "src"], "", "2");
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(stub_args(&out), vec!["-n", "(", "src"]);
    assert_eq!(String::from_utf8_lossy(&out.stderr), "stub-stderr\n");
}

#[test]
fn a_scan_that_could_differ_from_the_tool_runs_the_tool() {
    let fx = fixture();
    // A binary file is one sym skips and grep reports: sym's answer could
    // differ, so the real grep runs with the argv untouched.
    std::fs::write(fx.repo.path().join("src/blob.bin"), b"needle\0\n").expect("write");
    let out: Output = stub_run(&fx, "grep", &["-rn", "needle", "src"], "", "0");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stub_args(&out), vec!["-rn", "needle", "src"]);
}

#[test]
fn rg_fallback_hands_rg_the_argv_untouched() {
    let fx = fixture();
    let cases: [&[&str]; 3] = [
        &["-V"],
        &["-S", "needle", "src"],
        &["-nv", "--", "-x", "src"],
    ];
    for args in cases {
        let out: Output = stub_run(&fx, "rg", args, "", "2");
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert_eq!(stub_args(&out), args.to_vec(), "argv changed for {args:?}");
        assert_eq!(String::from_utf8_lossy(&out.stderr), "stub-stderr\n");
    }
}
