//! `legion find` (#1472): answers from sym's inventory in find's own shape,
//! and never runs find.
//!
//! Three kinds of test:
//! - parity: legion and the real find, same argv, same cwd, over a tree with
//!   nothing gitignored in it, where the two must agree;
//! - sym-only: `PATH` holds no find at all, so only the inventory can answer;
//! - stub: a fake find first on `PATH` leaves a marker when run. A form sym
//!   declines must name a command, exit 1, and leave no marker.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use crate::common::{legion_cmd, run_ok};

/// A watched, indexed git checkout, plus the data dir whose watch.toml names
/// it. `src/` holds only indexed files; `dist/` and `node_modules/` are
/// gitignored, and on disk.
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

fn git(cwd: &Path, args: &[&str]) {
    let out: Output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn fixture() -> Fixture {
    let data = tempfile::tempdir().expect("data dir");
    let repo = tempfile::tempdir().expect("repo dir");
    git(repo.path(), &["init", "-q"]);
    write(repo.path(), ".gitignore", "dist/\nnode_modules/\n");
    write(repo.path(), "config.toml", "z = 3\n");
    write(repo.path(), "src/a.rs", "fn main() {}\n");
    write(repo.path(), "src/sub/b.toml", "x = 1\n");
    write(repo.path(), "src/sub/deep/C.md", "# c\n");
    write(repo.path(), "src/.hidden.toml", "y = 2\n");
    write(repo.path(), "dist/site.css", "a {}\n");
    write(repo.path(), "node_modules/zod/package.json", "{}\n");
    std::fs::write(
        data.path().join("watch.toml"),
        format!(
            "[[repos]]\nname = \"fixture\"\nworkdir = \"{}\"\n",
            repo.path().display()
        ),
    )
    .expect("seed watch.toml");
    run_ok(legion_cmd(data.path()).args(["index", "fixture"]));
    Fixture { data, repo }
}

fn legion_find_in(fx: &Fixture, cwd: &Path, path_env: &str, args: &[&str]) -> Output {
    legion_cmd(fx.data.path())
        .current_dir(cwd)
        .env("PATH", path_env)
        .arg("find")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("spawn legion")
}

/// `legion find` from the repo root with no find on PATH.
fn sym_only(fx: &Fixture, args: &[&str]) -> Output {
    let no_tools = tempfile::tempdir().expect("empty PATH dir");
    legion_find_in(
        fx,
        fx.repo.path(),
        &no_tools.path().display().to_string(),
        args,
    )
}

fn sorted_lines(out: &Output) -> Vec<String> {
    let mut lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    lines.sort();
    lines
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn a_name_answers_in_finds_own_shape_with_no_find_on_path() {
    let fx = fixture();
    let out: Output = sym_only(&fx, &[".", "-name", "*.toml"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        sorted_lines(&out),
        vec!["./config.toml", "./src/.hidden.toml", "./src/sub/b.toml"]
    );
}

/// Each form runs over `src`, which nothing gitignored touches: legion and the
/// real find must agree, and legion needs no find to do it. `src` holds no
/// empty directory, so the directories derived from file paths are all of them.
#[test]
fn answered_forms_match_real_find() {
    let forms: [&[&str]; 35] = [
        &["src"],
        &["src/"],
        &["./src"],
        &["src", "-type", "f"],
        &["src", "-type", "d"],
        &["src", "-name", "*.toml"],
        &["src", "-type", "f", "-name", "*.toml"],
        &["src", "-name", "*.toml", "-type", "f"],
        &["src", "-name", "sub"],
        &["src", "-name", "absent-*"],
        &["src", "-iname", "c.MD"],
        &["src", "-maxdepth", "1"],
        &["src", "-maxdepth", "2", "-type", "f"],
        &["src", "-mindepth", "2", "-type", "f"],
        &["src", "-maxdepth", "0"],
        &["src", "-path", "*/deep/*"],
        &["src", "-not", "-path", "*/sub/*"],
        &["src", "!", "-name", "*.rs", "-type", "f"],
        &["src", "-name", "*.rs", "-o", "-name", "*.md"],
        &[
            "src", "(", "-name", "*.rs", "-o", "-name", "*.md", ")", "-type", "f",
        ],
        &["src", "-type", "f", "-a", "-name", "a.rs", "-print"],
        &["src/sub", "src/sub/deep", "-type", "f"],
        &["src", "-name", "sub", "-prune"],
        &["src", "-name", "sub", "-prune", "-o", "-print"],
        &["src", "-name", "sub", "-prune", "-o", "-type", "f"],
        &[
            "src", "-name", "sub", "-prune", "-o", "-type", "f", "-print",
        ],
        &[
            "src",
            "-path",
            "src/sub/deep",
            "-prune",
            "-o",
            "-name",
            "*.md",
            "-print",
        ],
        &[
            "src", "-type", "d", "-name", "deep", "-prune", "-o", "-type", "f", "-print",
        ],
        &["src", "-type", "f", "-o", "-name", "sub", "-prune"],
        &["src", "-name", "sub", "-prune", "-print"],
        &[
            "src",
            "-maxdepth",
            "1",
            "-name",
            "sub",
            "-prune",
            "-o",
            "-print",
        ],
        &["src", "-name", "a.rs", "-prune"],
        &["src", "-prune"],
        &["src/a.rs"],
        &["src", "-print", "-name", "a.rs"],
    ];
    let fx = fixture();
    for args in forms {
        let ours: Output = sym_only(&fx, args);
        let theirs: Output = Command::new("find")
            .current_dir(fx.repo.path())
            .args(args)
            .output()
            .expect("spawn real find");
        assert_eq!(
            sorted_lines(&ours),
            sorted_lines(&theirs),
            "{args:?}\nlegion stderr: {}",
            stderr(&ours)
        );
        assert_eq!(ours.status.code(), theirs.status.code(), "{args:?}");
    }
}

#[test]
fn a_listing_has_no_gitignored_tree_and_no_git_dir() {
    let fx = fixture();
    let out: Output = sym_only(&fx, &[]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        sorted_lines(&out),
        vec![
            ".",
            "./.gitignore",
            "./config.toml",
            "./src",
            "./src/.hidden.toml",
            "./src/a.rs",
            "./src/sub",
            "./src/sub/b.toml",
            "./src/sub/deep",
            "./src/sub/deep/C.md",
        ]
    );
}

#[test]
fn a_covered_tree_with_no_match_is_empty_exit_zero_and_says_where_to_look() {
    let fx = fixture();
    let out: Output = sym_only(&fx, &[".", "-name", "*.css"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty());
    let said: String = stderr(&out);
    assert!(
        said.contains("gitignored") && said.contains("ls -R ."),
        "{said}"
    );
}

/// A stub `find` that leaves a marker file when run. Declined forms must not
/// touch it.
struct StubFind {
    dir: tempfile::TempDir,
    marker: PathBuf,
}

fn stub_find() -> StubFind {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("stub dir");
    let marker: PathBuf = dir.path().join("ran");
    let script: PathBuf = dir.path().join("find");
    std::fs::write(
        &script,
        format!("#!/bin/sh\necho ran > \"{}\"\n", marker.display()),
    )
    .expect("write stub");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    StubFind { dir, marker }
}

/// Declined with the stub first on PATH: exit 1, nothing on stdout, a message
/// naming `command`, and find never ran.
fn assert_declined(fx: &Fixture, args: &[&str], command: &str) {
    let stub: StubFind = stub_find();
    let path_env: String = format!("{}:/usr/bin:/bin", stub.dir.path().display());
    let out: Output = legion_find_in(fx, fx.repo.path(), &path_env, args);
    assert_eq!(out.status.code(), Some(1), "{args:?}: {}", stderr(&out));
    assert!(out.stdout.is_empty(), "{args:?}");
    let said: String = stderr(&out);
    assert!(said.contains(command), "{args:?}: {said}");
    assert!(!stub.marker.exists(), "{args:?} ran find");
}

#[test]
fn a_predicate_sym_cannot_answer_names_a_command_and_never_runs_find() {
    let fx = fixture();
    for (args, command) in [
        (&["src", "-exec", "cat", "{}", ";"][..], "legion find src"),
        (&["src", "-delete"], "| xargs"),
        (&["src", "-mtime", "-1"], "ls -lR src"),
        (&["src", "-newermt", "2026-01-01"], "ls -lR src"),
        (&["src", "-regex", ".*"], "legion sym tree"),
        (&["src", "-name", "*.[ch]"], "legion find src -name"),
    ] {
        assert_declined(&fx, args, command);
    }
}

#[test]
fn gitignored_trees_decline_instead_of_answering_empty() {
    let fx = fixture();
    assert_declined(&fx, &["dist", "-name", "*.css"], "ls -R dist");
    assert_declined(
        &fx,
        &["node_modules/zod", "-name", "package.json"],
        "ls -R node_modules/zod",
    );
}

#[test]
fn a_path_outside_every_watched_repo_declines_and_names_commands() {
    let fx = fixture();
    let outside = tempfile::tempdir().expect("outside dir");
    let path: String = outside.path().display().to_string();
    assert_declined(&fx, &[&path, "-name", "x"], &format!("ls -R {path}"));
    assert_declined(&fx, &[&path, "-name", "x"], "legion sym etc find-file");
}

#[test]
fn a_watched_repo_never_indexed_says_to_index_it() {
    let data = tempfile::tempdir().expect("data dir");
    let repo = tempfile::tempdir().expect("repo dir");
    std::fs::write(
        data.path().join("watch.toml"),
        format!(
            "[[repos]]\nname = \"fresh\"\nworkdir = \"{}\"\n",
            repo.path().display()
        ),
    )
    .expect("seed watch.toml");
    let fx = Fixture { data, repo };
    assert_declined(&fx, &["."], "legion index fresh");
}

#[test]
fn a_missing_operand_says_so_like_find() {
    let fx = fixture();
    assert_declined(&fx, &["no-such-dir"], "No such file or directory");
}

#[test]
fn one_declined_operand_still_answers_the_others_and_exits_one() {
    let fx = fixture();
    let out: Output = sym_only(&fx, &["src/sub/deep", "dist", "-type", "f"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(sorted_lines(&out), vec!["src/sub/deep/C.md"]);
    assert!(stderr(&out).contains("ls -R dist"), "{}", stderr(&out));
}

/// `legion find` with stderr discarded, as `2>/dev/null` does.
fn discarding_stderr(fx: &Fixture, args: &[&str]) -> Output {
    legion_cmd(fx.data.path())
        .current_dir(fx.repo.path())
        .arg("find")
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .expect("spawn legion")
}

/// In `find ... | xargs rm` a refusal's words on stdout would become arguments.
#[test]
fn a_refusal_never_goes_to_stdout_even_with_stderr_discarded() {
    let fx = fixture();
    for args in [
        &["dist", "-name", "*.css"][..],
        &["src", "-mtime", "-1"],
        &["/no/such/path"],
        &[".", "-path", "*/node_modules/*"],
    ] {
        let refused: Output = discarding_stderr(&fx, args);
        assert_eq!(refused.status.code(), Some(1), "{args:?}");
        assert!(refused.stdout.is_empty(), "{args:?}: {refused:?}");
    }
    let empty: Output = discarding_stderr(&fx, &[".", "-name", "*.css"]);
    assert_eq!(empty.status.code(), Some(0));
    assert!(empty.stdout.is_empty());
    let answered: Output = discarding_stderr(&fx, &["src", "-name", "a.rs"]);
    assert_eq!(sorted_lines(&answered), vec!["src/a.rs"]);
}

/// The operator's case: nothing a refusal says reaches xargs.
#[test]
fn a_refusal_gives_xargs_no_arguments() {
    let fx = fixture();
    let bin: String = env!("CARGO_BIN_EXE_legion").to_string();
    let out: Output = Command::new("sh")
        .current_dir(fx.repo.path())
        .env("LEGION_DATA_DIR", fx.data.path())
        .arg("-c")
        .arg(format!(
            "'{bin}' find dist -name '*.css' 2>/dev/null | xargs -n1 echo arg:"
        ))
        .output()
        .expect("spawn sh");
    let stdout: String = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(!stdout.contains("legion"), "{stdout}");
    assert!(!stdout.contains("ls"), "{stdout}");
}

#[test]
fn a_new_uncommitted_file_the_query_would_print_is_said_on_stderr() {
    let fx = fixture();
    write(fx.repo.path(), "src/new.toml", "n = 1\n");
    let out: Output = sym_only(&fx, &["src", "-name", "*.toml"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        sorted_lines(&out),
        vec!["src/.hidden.toml", "src/sub/b.toml"]
    );
    let said: String = stderr(&out);
    assert!(said.contains("behind the disk"), "{said}");
    assert!(said.contains("src/new.toml"), "{said}");
    assert!(said.contains("legion index fixture"), "{said}");
}

#[test]
fn a_short_list_is_refused_when_its_warning_would_go_nowhere() {
    let fx = fixture();
    write(fx.repo.path(), "src/new.toml", "n = 1\n");
    let out: Output = discarding_stderr(&fx, &["src", "-name", "*.toml"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty(), "{out:?}");
}

#[test]
fn a_new_directory_counts_and_so_does_a_listing() {
    let fx = fixture();
    write(fx.repo.path(), "src/fresh/x.rs", "\n");
    let out: Output = sym_only(&fx, &["src", "-type", "d", "-name", "fresh"]);
    assert!(out.stdout.is_empty());
    assert!(stderr(&out).contains("src/fresh"), "{}", stderr(&out));
    let listing: Output = sym_only(&fx, &["src", "-type", "f"]);
    assert!(
        stderr(&listing).contains("src/fresh/x.rs"),
        "{}",
        stderr(&listing)
    );
}

#[test]
fn a_change_the_answer_does_not_depend_on_says_nothing() {
    let fx = fixture();
    // The query would not print it.
    write(fx.repo.path(), "src/other.rs", "\n");
    // Pruned out of the question.
    write(fx.repo.path(), "src/sub/new.toml", "\n");
    // Gitignored, so the index would not hold it either.
    write(fx.repo.path(), "dist/later.toml", "\n");
    // Outside the operand.
    write(fx.repo.path(), "later.toml", "\n");
    let out: Output = sym_only(
        &fx,
        &[
            "src", "-name", "sub", "-prune", "-o", "-name", "*.toml", "-print",
        ],
    );
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(sorted_lines(&out), vec!["src/.hidden.toml"]);
    assert!(out.stderr.is_empty(), "{}", stderr(&out));
    // Discarding stderr does not turn it into a refusal.
    let quiet: Output = discarding_stderr(
        &fx,
        &[
            "src", "-name", "sub", "-prune", "-o", "-name", "*.toml", "-print",
        ],
    );
    assert_eq!(quiet.status.code(), Some(0));
}

#[test]
fn a_file_gone_since_the_index_is_not_printed_and_needs_no_warning() {
    let fx = fixture();
    std::fs::remove_file(fx.repo.path().join("src/a.rs")).expect("remove");
    let out: Output = sym_only(&fx, &["src", "-maxdepth", "1", "-type", "f"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(sorted_lines(&out), vec!["src/.hidden.toml"]);
    assert!(out.stderr.is_empty(), "{}", stderr(&out));
}

/// The registered checkout's inventory is the wrong answer for another branch:
/// a linked worktree is refused and pointed at a command that answers for it,
/// wherever the worktree lives.
#[test]
fn a_linked_worktree_is_refused_and_pointed_at_git_ls_files() {
    let fx = fixture();
    git(fx.repo.path(), &["add", "."]);
    git(fx.repo.path(), &["commit", "-q", "-m", "one"]);
    run_ok(legion_cmd(fx.data.path()).args(["index", "fixture"]));
    let parent = tempfile::tempdir().expect("worktree parent");
    let beside: PathBuf = parent.path().join("wt");
    let inside: PathBuf = fx.repo.path().join(".claude/worktrees/agent");
    for worktree in [&beside, &inside] {
        git(
            fx.repo.path(),
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                &worktree.display().to_string(),
            ],
        );
        let stub: StubFind = stub_find();
        let path_env: String = format!(
            "{}:{}",
            stub.dir.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let out: Output = legion_find_in(&fx, worktree, &path_env, &["src", "-type", "f"]);
        assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
        assert!(out.stdout.is_empty());
        assert!(
            stderr(&out).contains("git -C src ls-files"),
            "{}",
            stderr(&out)
        );
        assert!(!stub.marker.exists());
        // The command it names answers.
        let named: Output = Command::new("git")
            .current_dir(worktree)
            .args(["-C", "src", "ls-files"])
            .output()
            .expect("spawn git");
        assert!(String::from_utf8_lossy(&named.stdout).contains("a.rs"));
    }
}

#[test]
fn an_empty_answer_about_an_ignored_tree_is_a_refusal() {
    let fx = fixture();
    assert_declined(
        &fx,
        &[".", "-path", "*/node_modules/zod/package.json"],
        "ls -R .",
    );
    let out: Output = legion_cmd(fx.data.path())
        .current_dir(fx.repo.path())
        .args(["find", ".", "-name", "dist", "-type", "d"])
        .stderr(Stdio::null())
        .output()
        .expect("spawn legion");
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty(), "{out:?}");
}

/// The rest of the command runs: the rewritten form in a real shell.
#[test]
fn the_rest_of_a_pipeline_runs_on_the_answer() {
    let fx = fixture();
    let bin: String = env!("CARGO_BIN_EXE_legion").to_string();
    let out: Output = Command::new("sh")
        .current_dir(fx.repo.path())
        .env("LEGION_DATA_DIR", fx.data.path())
        .arg("-c")
        .arg(format!(
            "'{bin}' find src -name '*.toml' 2>/dev/null | wc -l && echo after && '{bin}' find src -type f | head -1"
        ))
        .output()
        .expect("spawn sh");
    let stdout: String = String::from_utf8_lossy(&out.stdout).into_owned();
    let lines: Vec<&str> = stdout.lines().map(str::trim).collect();
    assert_eq!(
        lines,
        vec!["2", "after", "src/.hidden.toml"],
        "{}",
        stderr(&out)
    );
}

#[test]
fn a_leading_legion_flag_reaches_the_same_answer_through_clap() {
    let fx = fixture();
    let out: Output = legion_cmd(fx.data.path())
        .current_dir(fx.repo.path())
        .args(["-v", "find", "src", "-name", "a.rs"])
        .output()
        .expect("spawn legion");
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(sorted_lines(&out), vec!["src/a.rs"]);
}
