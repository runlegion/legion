//! Integration tests for `legion git` (#1335): the proxy that takes git's
//! own arguments. Every test drives real `git` fixtures -- a bare `origin`
//! plus a local checkout -- because the command's job is choosing between
//! the audited push/commit paths, a refusal, and real git.
//!
//! The binary under test spawns real `git push` / `git commit`, so identity
//! and `commit.gpgsign=false` live in each fixture's LOCAL config and the
//! host's global/system config is swapped out on the command (the same two
//! rules `commit.rs` documents).

use crate::common::*;
use std::path::{Path, PathBuf};
use std::process::Command;

const AGENT: &str = "test-agent";

/// A bare `origin` that accepts push options, so `-o force=1` can succeed.
fn init_bare_remote() -> tempfile::TempDir {
    let remote = tempfile::tempdir().unwrap();
    let out = Command::new("git")
        .current_dir(remote.path())
        .args(["init", "--bare", "-q"])
        .output()
        .expect("git init --bare must spawn");
    assert!(out.status.success());
    let out = Command::new("git")
        .arg("--git-dir")
        .arg(remote.path())
        .args(["config", "receive.advertisePushOptions", "true"])
        .output()
        .expect("git config must spawn");
    assert!(out.status.success());
    remote
}

/// Configure identity and signing in `dir`'s local config.
fn configure_identity(dir: &Path) {
    run_git_fixture(dir, &["config", "user.name", "Legion Test Fixture"]);
    run_git_fixture(dir, &["config", "user.email", "fixture@example.invalid"]);
    run_git_fixture(dir, &["config", "commit.gpgsign", "false"]);
}

/// A checkout at `lp` with `main` seeded and pushed to `origin`, then `feat`
/// checked out carrying one more commit, and a `pre-push` hook that leaves a
/// marker so a test can tell whether any `git push` ran at all.
fn init_local_at(lp: &Path, remote: &Path) {
    run_git_fixture(lp, &["init", "-q", "-b", "main"]);
    configure_identity(lp);
    run_git_fixture(lp, &["remote", "add", "origin", remote.to_str().unwrap()]);
    std::fs::write(lp.join("README.md"), "seed\n").unwrap();
    run_git_fixture(lp, &["add", "README.md"]);
    run_git_fixture(lp, &["commit", "-q", "-m", "seed"]);
    run_git_fixture(lp, &["push", "-q", "origin", "main"]);
    run_git_fixture(lp, &["checkout", "-q", "-b", "feat"]);
    std::fs::write(lp.join("feature.txt"), "change\n").unwrap();
    run_git_fixture(lp, &["add", "feature.txt"]);
    run_git_fixture(lp, &["commit", "-q", "-m", "add feature"]);

    let hook = lp.join(".git").join("hooks").join("pre-push");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    let marker = lp.join(".git").join("pushed-marker");
    std::fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// A bare remote plus a local checkout on `feat`.
struct Fixture {
    remote: tempfile::TempDir,
    local: tempfile::TempDir,
    data: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Fixture {
        let remote = init_bare_remote();
        let local = tempfile::tempdir().unwrap();
        init_local_at(local.path(), remote.path());
        Fixture {
            remote,
            local,
            data: tempfile::tempdir().unwrap(),
        }
    }

    fn lp(&self) -> &Path {
        self.local.path()
    }

    /// `legion git <args>` standing in the local checkout.
    fn legion_git(&self, args: &[&str]) -> Command {
        let mut cmd = legion_git_in(self.data.path(), self.lp());
        cmd.args(args);
        cmd
    }

    fn remote_refs(&self) -> String {
        bare_refs(self.remote.path())
    }

    fn pushed(&self) -> bool {
        self.lp().join(".git").join("pushed-marker").exists()
    }

    fn rows(&self) -> Vec<serde_json::Value> {
        audit_rows(self.data.path())
    }

    fn stage_change(&self, name: &str) {
        std::fs::write(self.lp().join(name), format!("{name}\n")).unwrap();
        run_git_fixture(self.lp(), &["add", name]);
    }
}

/// `legion git` with the host's git config swapped out, a fixed audit agent,
/// and every pager and editor pinned so nothing waits on a terminal.
fn legion_git_in(data_dir: &Path, cwd: &Path) -> Command {
    let mut cmd = legion_cmd(data_dir);
    isolate(&mut cmd, cwd);
    cmd.env("LEGION_REPO", AGENT).arg("git");
    cmd
}

/// Real `git` with the same environment `legion_git_in` gives legion.
fn real_git_in(cwd: &Path) -> Command {
    let mut cmd = Command::new("git");
    isolate(&mut cmd, cwd);
    cmd
}

fn isolate(cmd: &mut Command, cwd: &Path) {
    let (global, system) = isolated_git_config_paths();
    cmd.current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", global)
        .env("GIT_CONFIG_SYSTEM", system)
        .env("GIT_EDITOR", "true")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .env("MANPAGER", "cat")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE");
}

fn bare_refs(remote: &Path) -> String {
    let out = Command::new("git")
        .arg("--git-dir")
        .arg(remote)
        .args(["for-each-ref", "--format=%(refname) %(objectname)"])
        .output()
        .expect("git for-each-ref must spawn");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn bare_ref(remote: &Path, name: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("--git-dir")
        .arg(remote)
        .args(["rev-parse", "--verify", "--quiet", name])
        .output()
        .expect("git rev-parse must spawn");
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn local_rev(dir: &Path, rev: &str) -> String {
    run_git_fixture_output(dir, &["rev-parse", rev])
}

fn head_message(dir: &Path) -> String {
    run_git_fixture_output(dir, &["log", "-1", "--format=%B"])
}

/// Every audit row, oldest first, with `details` parsed.
fn audit_rows(data_dir: &Path) -> Vec<serde_json::Value> {
    let out = run_ok(legion_cmd(data_dir).args(["audit", "--json", "--limit", "1000"]));
    let mut rows: Vec<serde_json::Value> =
        serde_json::from_str(&out).expect("audit --json must produce a JSON array");
    for row in &mut rows {
        let details: serde_json::Value = row["details"]
            .as_str()
            .map(|s| serde_json::from_str(s).expect("details must be JSON"))
            .unwrap_or(serde_json::Value::Null);
        row["details"] = details;
    }
    rows.sort_by(|a, b| {
        a["timestamp"]
            .as_str()
            .unwrap_or_default()
            .cmp(b["timestamp"].as_str().unwrap_or_default())
    });
    rows
}

fn one_row(rows: &[serde_json::Value]) -> &serde_json::Value {
    assert_eq!(
        rows.len(),
        1,
        "expected exactly one audit row, got {rows:#?}"
    );
    &rows[0]
}

/// The shape of an audit row with the values that differ between two runs
/// of the same command (shas, paths, timestamps, ids) left out.
fn row_shape(row: &serde_json::Value) -> serde_json::Value {
    let mut keys: Vec<String> = row["details"]
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    serde_json::json!({
        "agent": row["agent"],
        "action": row["action"],
        "target_type": row["target_type"],
        "target_ref": row["target_ref"],
        "source_type": row["source_type"],
        "outcome": row["outcome"],
        "task_id": row["task_id"],
        "detail_keys": keys,
        "signing": row["details"]["signing"],
        "gates": row["details"]["gates"],
    })
}

fn assert_real_git_row(row: &serde_json::Value, action: &str, args: &[&str], code: i32) {
    assert_eq!(row["action"], action, "{row:#}");
    assert_eq!(row["agent"], AGENT, "{row:#}");
    assert_eq!(row["details"]["real_git"], true, "{row:#}");
    assert_eq!(row["details"]["args"], serde_json::json!(args), "{row:#}");
    assert_eq!(row["details"]["exit_code"], code, "{row:#}");
    let outcome = if code == 0 { "success" } else { "failure" };
    assert_eq!(row["outcome"], outcome, "{row:#}");
}

fn code(out: &std::process::Output) -> i32 {
    out.status.code().expect("exit code")
}

// ---------------------------------------------------------------------------
// Finding the subcommand
// ---------------------------------------------------------------------------

/// The first `-C` is the directory; the second is `commit`'s reuse-message
/// option, which the audited path cannot express, so real git runs.
#[cfg(unix)]
#[test]
fn git_dir_then_commit_reuse_message_runs_real_git_in_the_dir() {
    let _guard = RealRepoConfigGuard::new();
    let fx = Fixture::new();
    fx.stage_change("more.txt");
    let elsewhere = tempfile::tempdir().unwrap();
    let before_msg = head_message(fx.lp());
    let before = local_rev(fx.lp(), "HEAD");
    let dir = fx.lp().to_str().unwrap();

    let args = ["-C", dir, "commit", "-C", "HEAD"];
    run_ok(legion_git_in(fx.data.path(), elsewhere.path()).args(args));

    assert_ne!(local_rev(fx.lp(), "HEAD"), before);
    assert_eq!(head_message(fx.lp()), before_msg);
    let rows = fx.rows();
    let row = one_row(&rows);
    assert_real_git_row(row, "commit", &args, 0);
    assert_eq!(row["target_ref"], "feat");
}

/// `-C <dir> commit -m` runs the audited commit in `<dir>`, and its row
/// matches the one `legion commit -C <dir> -m` writes. The process's own
/// directory -- itself a repository -- does not change.
#[cfg(unix)]
#[test]
fn git_dir_commit_message_matches_legion_commit_row() {
    let _guard = RealRepoConfigGuard::new();
    let fx = Fixture::new();
    fx.stage_change("more.txt");
    let own = Fixture::new();
    own.stage_change("own.txt");
    let own_head = local_rev(own.lp(), "HEAD");
    let own_status = run_git_fixture_output(own.lp(), &["status", "--porcelain"]);
    let before = local_rev(fx.lp(), "HEAD");
    let dir = fx.lp().to_str().unwrap();

    run_ok(legion_git_in(fx.data.path(), own.lp()).args(["-C", dir, "commit", "-m", "feat(x): y"]));

    assert_ne!(local_rev(fx.lp(), "HEAD"), before);
    assert_eq!(head_message(fx.lp()), "feat(x): y");
    assert_eq!(local_rev(own.lp(), "HEAD"), own_head);
    assert_eq!(
        run_git_fixture_output(own.lp(), &["status", "--porcelain"]),
        own_status
    );

    let twin = Fixture::new();
    twin.stage_change("more.txt");
    let mut cmd = legion_cmd(twin.data.path());
    isolate(&mut cmd, own.lp());
    run_ok(cmd.args([
        "commit",
        "--repo",
        AGENT,
        "-C",
        twin.lp().to_str().unwrap(),
        "-m",
        "feat(x): y",
    ]));

    let ours = fx.rows();
    let theirs = twin.rows();
    assert_eq!(row_shape(one_row(&ours)), row_shape(one_row(&theirs)));
    assert!(ours[0]["details"].get("real_git").is_none());
}

/// Several `-C` options compose: `-C a -C b` stands in `a/b`.
#[cfg(unix)]
#[test]
fn git_several_dash_c_compose() {
    let _guard = RealRepoConfigGuard::new();
    let root = tempfile::tempdir().unwrap();
    let inner = root.path().join("a").join("b");
    std::fs::create_dir_all(&inner).unwrap();
    run_git_fixture(&inner, &["init", "-q", "-b", "zed"]);
    let data = tempfile::tempdir().unwrap();

    let stdout =
        run_ok(legion_git_in(data.path(), root.path()).args(["-C", "a", "-C", "b", "status"]));
    assert!(
        stdout.contains("zed"),
        "expected git status of a/b, got: {stdout}"
    );
}

/// Each value-taking global option, in each form git accepts, is skipped to
/// find `push`: `push --force` after it is refused on the force rule.
#[cfg(unix)]
#[test]
fn git_value_global_options_still_find_push() {
    let fx = Fixture::new();
    let dir = fx.lp().to_str().unwrap();
    let git_dir = fx.lp().join(".git");
    let git_dir = git_dir.to_str().unwrap();
    let with_eq_git_dir = format!("--git-dir={git_dir}");
    let with_eq_work_tree = format!("--work-tree={dir}");
    let cases: Vec<Vec<&str>> = vec![
        vec!["-C", dir],
        vec!["-c", "user.name=x"],
        vec!["--git-dir", git_dir],
        vec![with_eq_git_dir.as_str()],
        vec!["--work-tree", dir],
        vec![with_eq_work_tree.as_str()],
        vec!["--namespace", "ns"],
        vec!["--namespace=ns"],
        vec!["--config-env", "user.name=HOME"],
        vec!["--config-env=user.name=HOME"],
        vec!["--exec-path=/nonexistent"],
        vec!["--attr-source", "HEAD"],
        vec!["--attr-source=HEAD"],
        vec!["--list-cmds=main"],
    ];
    let before = fx.remote_refs();
    for mut case in cases {
        case.extend(["push", "--force", "origin", "feat"]);
        let (_out, stderr) = run_fail(&mut fx.legion_git(&case));
        assert!(stderr.contains("force rule"), "{case:?}: {stderr}");
    }
    assert_eq!(fx.remote_refs(), before);
    assert!(!fx.pushed());
}

/// `--git-dir <d> --work-tree <w> commit -m` is a commit with a global
/// option other than `-C`: real git, with an audit row.
#[cfg(unix)]
#[test]
fn git_dir_and_work_tree_commit_runs_real_git_audited() {
    let _guard = RealRepoConfigGuard::new();
    let fx = Fixture::new();
    fx.stage_change("more.txt");
    let elsewhere = tempfile::tempdir().unwrap();
    let git_dir = fx.lp().join(".git");
    let args = [
        "--git-dir",
        git_dir.to_str().unwrap(),
        "--work-tree",
        fx.lp().to_str().unwrap(),
        "commit",
        "-m",
        "feat(x): y",
    ];
    run_ok(legion_git_in(fx.data.path(), elsewhere.path()).args(args));
    assert_eq!(head_message(fx.lp()), "feat(x): y");
    let rows = fx.rows();
    let row = one_row(&rows);
    assert_real_git_row(row, "commit", &args, 0);
    assert_eq!(row["target_ref"], "feat");
}

/// `--version`, `-h`, `--help`, `-v`, and `status -- file` reach git
/// unchanged: same stdout and exit code as git itself, no audit row.
#[cfg(unix)]
#[test]
fn git_help_version_and_dashdash_reach_git_unchanged() {
    let fx = Fixture::new();
    let cases: [&[&str]; 6] = [
        &["--version"],
        &["-v"],
        &["-h"],
        &["--help"],
        &["status", "--", "feature.txt"],
        &["--", "status"],
    ];
    for case in cases {
        let ours = fx.legion_git(case).output().unwrap();
        let theirs = real_git_in(fx.lp()).args(case).output().unwrap();
        assert_eq!(code(&ours), code(&theirs), "{case:?}");
        assert_eq!(
            String::from_utf8_lossy(&ours.stdout),
            String::from_utf8_lossy(&theirs.stdout),
            "{case:?}"
        );
    }
    let version = fx.legion_git(&["-v"]).output().unwrap();
    assert!(String::from_utf8_lossy(&version.stdout).starts_with("git version"));
    assert!(fx.rows().is_empty());
}

// ---------------------------------------------------------------------------
// Push through the audited path
// ---------------------------------------------------------------------------

/// The audited branch push's row for `feat`, from `legion push --branch`.
fn legion_push_row_shape() -> serde_json::Value {
    let twin = Fixture::new();
    let mut cmd = legion_cmd(twin.data.path());
    isolate(&mut cmd, twin.lp());
    run_ok(cmd.args(["push", "--repo", AGENT, "--branch", "feat"]));
    row_shape(one_row(&twin.rows()))
}

/// Every translated form runs the audited branch push for `feat`: the same
/// row `legion push --branch feat` writes, and the remote ref `git push`
/// would have updated -- `refs/heads/feat` at the local commit, and nothing
/// else.
#[cfg(unix)]
#[test]
fn git_push_forms_run_the_audited_branch_push() {
    let _guard = RealRepoConfigGuard::new();
    let want = legion_push_row_shape();
    let cases: [&[&str]; 8] = [
        &["push"],
        &["push", "origin"],
        &["push", "-u", "origin", "feat"],
        &["push", "--set-upstream", "origin", "feat"],
        &["push", "origin", "HEAD"],
        &["push", "origin", "feat:feat"],
        &["push", "origin", "refs/heads/feat"],
        &["push", "origin", "feat:refs/heads/feat"],
    ];
    for case in cases {
        let fx = Fixture::new();
        let main_before = bare_ref(fx.remote.path(), "refs/heads/main");
        let stdout = run_ok(&mut fx.legion_git(case));
        assert!(
            stdout.contains("pushed feat to origin"),
            "{case:?}: {stdout}"
        );
        assert_eq!(row_shape(one_row(&fx.rows())), want, "{case:?}");
        assert_eq!(
            bare_ref(fx.remote.path(), "refs/heads/feat"),
            Some(local_rev(fx.lp(), "feat")),
            "{case:?}"
        );
        assert_eq!(bare_ref(fx.remote.path(), "refs/heads/main"), main_before);
        assert_eq!(fx.remote_refs().lines().count(), 2, "{case:?}");
    }
}

/// `tag <name>` and `refs/tags/<name>` run the audited tag push.
#[cfg(unix)]
#[test]
fn git_push_tag_forms_run_the_audited_tag_push() {
    let _guard = RealRepoConfigGuard::new();
    for case in [
        &["push", "origin", "tag", "v1"][..],
        &["push", "origin", "refs/tags/v1"],
    ] {
        let fx = Fixture::new();
        run_git_fixture(fx.lp(), &["tag", "v1", "main"]);
        let stdout = run_ok(&mut fx.legion_git(case));
        assert!(stdout.contains("pushed tag v1"), "{case:?}: {stdout}");
        let rows = fx.rows();
        let row = one_row(&rows);
        assert_eq!(row["target_type"], "tag");
        assert_eq!(row["target_ref"], "v1");
        assert!(row["details"].get("real_git").is_none());
        assert_eq!(
            bare_ref(fx.remote.path(), "refs/tags/v1"),
            Some(local_rev(fx.lp(), "v1"))
        );
    }
}

/// A no-refspec push whose upstream is not `origin/<same name>` runs as real
/// git, and the remotes end up exactly as `git push` leaves them.
#[cfg(unix)]
#[test]
fn git_push_with_foreign_upstream_runs_real_git() {
    let _guard = RealRepoConfigGuard::new();
    // Upstream on another remote; upstream under another name on origin.
    let setups: [fn(&Fixture, &Path); 2] = [
        |fx, fork| {
            run_git_fixture(fx.lp(), &["remote", "add", "fork", fork.to_str().unwrap()]);
            run_git_fixture(fx.lp(), &["push", "-q", "-u", "fork", "feat"]);
        },
        |fx, _| run_git_fixture(fx.lp(), &["push", "-q", "-u", "origin", "feat:elsewhere"]),
    ];
    for setup in setups {
        let mut outcomes: Vec<(i32, String, String)> = Vec::new();
        for use_legion in [true, false] {
            let fx = Fixture::new();
            let fork = init_bare_remote();
            setup(&fx, fork.path());
            fx.stage_change("next.txt");
            run_git_fixture(fx.lp(), &["commit", "-q", "-m", "next"]);
            let out = if use_legion {
                fx.legion_git(&["push"]).output().unwrap()
            } else {
                real_git_in(fx.lp()).arg("push").output().unwrap()
            };
            let head = local_rev(fx.lp(), "HEAD");
            let where_head = |refs: String| {
                refs.lines()
                    .filter(|l| l.ends_with(&head))
                    .map(|l| l.split(' ').next().unwrap_or_default().to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            };
            let origin = where_head(fx.remote_refs());
            let forked = where_head(bare_refs(fork.path()));
            if use_legion {
                let rows = fx.rows();
                assert_real_git_row(one_row(&rows), "push", &["push"], code(&out));
            }
            outcomes.push((code(&out), origin, forked));
        }
        assert_eq!(outcomes[0], outcomes[1]);
    }
}

// ---------------------------------------------------------------------------
// Commit through the audited path
// ---------------------------------------------------------------------------

/// Several `-m` are joined as separate paragraphs, as git joins them.
#[cfg(unix)]
#[test]
fn git_commit_joins_several_messages() {
    let _guard = RealRepoConfigGuard::new();
    let fx = Fixture::new();
    fx.stage_change("more.txt");
    run_ok(&mut fx.legion_git(&["commit", "-m", "feat(x): a", "-m", "b"]));
    assert_eq!(head_message(fx.lp()), "feat(x): a\n\nb");
    let rows = fx.rows();
    assert!(one_row(&rows)["details"].get("pre_sha").is_some());
}

/// Each message and file spelling commits through the audited path.
#[cfg(unix)]
#[test]
fn git_commit_message_and_file_forms() {
    let _guard = RealRepoConfigGuard::new();
    let cases: [&[&str]; 8] = [
        &["-mfeat(x): foo"],
        &["--message", "feat(x): foo"],
        &["--message=feat(x): foo"],
        &["-F", "msg.txt"],
        &["-Fmsg.txt"],
        &["--file", "msg.txt"],
        &["--file=msg.txt"],
        &["-F", "-"],
    ];
    for case in cases {
        let fx = Fixture::new();
        std::fs::write(fx.lp().join("msg.txt"), "feat(x): foo\n").unwrap();
        fx.stage_change("more.txt");
        let mut args: Vec<&str> = vec!["commit"];
        args.extend_from_slice(case);
        let mut cmd = fx.legion_git(&args);
        let mut child = cmd
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        {
            use std::io::Write;
            let mut stdin = child.stdin.take().unwrap();
            stdin.write_all(b"feat(x): foo\n").unwrap();
        }
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{case:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(head_message(fx.lp()), "feat(x): foo", "{case:?}");
        let rows = fx.rows();
        let row = one_row(&rows);
        assert_eq!(row["outcome"], "success");
        assert!(row["details"].get("real_git").is_none(), "{case:?}");
    }
}

/// A message the audited validation refuses is refused as `legion commit`
/// refuses it, with the same row.
#[cfg(unix)]
#[test]
fn git_commit_bad_message_is_refused_like_legion_commit() {
    let _guard = RealRepoConfigGuard::new();
    let fx = Fixture::new();
    fx.stage_change("more.txt");
    let before = local_rev(fx.lp(), "HEAD");
    let (_o, ours) = run_fail(&mut fx.legion_git(&["commit", "-m", "no scope here"]));
    assert_eq!(local_rev(fx.lp(), "HEAD"), before);

    let twin = Fixture::new();
    twin.stage_change("more.txt");
    let mut cmd = legion_cmd(twin.data.path());
    isolate(&mut cmd, twin.lp());
    let (_o, theirs) = run_fail(cmd.args(["commit", "--repo", AGENT, "-m", "no scope here"]));

    assert_eq!(ours, theirs);
    assert_eq!(
        row_shape(one_row(&fx.rows())),
        row_shape(one_row(&twin.rows()))
    );
    assert_eq!(fx.rows()[0]["outcome"], "failure");
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn git_push_force_variants_are_refused() {
    let fx = Fixture::new();
    let sha = local_rev(fx.lp(), "main");
    let lease_expect = format!("--force-with-lease=feat:{sha}");
    let cases: Vec<Vec<&str>> = vec![
        vec!["push", "-f", "origin", "feat"],
        vec!["push", "--force", "origin", "feat"],
        vec!["push", "--force-with-lease", "origin", "feat"],
        vec!["push", "--force-with-lease=feat", "origin", "feat"],
        vec!["push", &lease_expect, "origin", "feat"],
        vec!["push", "--force-if-includes", "origin", "feat"],
        vec!["push", "-uf", "origin", "feat"],
        vec!["push", "origin", "+feat"],
    ];
    let before = fx.remote_refs();
    for case in cases {
        let (_o, stderr) = run_fail(&mut fx.legion_git(&case));
        assert!(stderr.contains("force rule"), "{case:?}: {stderr}");
        assert_eq!(fx.remote_refs(), before, "{case:?}");
        assert!(!fx.pushed(), "{case:?}: a git push ran");
    }
    assert!(fx.rows().is_empty());
}

/// `--no-force-with-lease` and `-o force=1` are not force variants: they run
/// as real git and push.
#[cfg(unix)]
#[test]
fn git_push_non_force_spellings_are_not_refused() {
    let _guard = RealRepoConfigGuard::new();
    for case in [
        &["push", "--no-force-with-lease", "origin", "feat"][..],
        &["push", "-o", "force=1", "origin", "feat"],
    ] {
        let fx = Fixture::new();
        run_ok(&mut fx.legion_git(case));
        assert_eq!(
            bare_ref(fx.remote.path(), "refs/heads/feat"),
            Some(local_rev(fx.lp(), "feat")),
            "{case:?}"
        );
        let rows = fx.rows();
        assert_real_git_row(one_row(&rows), "push", case, 0);
    }
}

#[cfg(unix)]
#[test]
fn git_push_to_main_or_master_is_refused() {
    let fx = Fixture::new();
    // Local main ahead of origin/main, so any push of it would change origin.
    run_git_fixture(fx.lp(), &["branch", "-f", "main", "feat"]);
    let cases: [&[&str]; 8] = [
        &["push", "origin", "main"],
        &["push", "origin", "master"],
        &["push", "origin", "feat:main"],
        &["push", "origin", "HEAD:refs/heads/master"],
        &["push", "origin", ":main"],
        &["push", "origin", "--delete", "main"],
        &["push", "--all", "origin"],
        &["push", "--mirror", "origin"],
    ];
    let before = fx.remote_refs();
    for case in cases {
        let (_o, stderr) = run_fail(&mut fx.legion_git(case));
        assert!(stderr.contains("main/master rule"), "{case:?}: {stderr}");
        assert_eq!(fx.remote_refs(), before, "{case:?}");
    }
    run_git_fixture(fx.lp(), &["checkout", "-q", "main"]);
    let (_o, stderr) = run_fail(&mut fx.legion_git(&["push"]));
    assert!(stderr.contains("main/master rule"), "{stderr}");
    assert_eq!(fx.remote_refs(), before);
    assert!(!fx.pushed());
}

/// The refusals are judged against the repository git would act on: the
/// process stands in a checkout on `feat`, the target is on `main`.
#[cfg(unix)]
#[test]
fn git_push_refusals_hold_under_dir_and_git_dir() {
    let target = Fixture::new();
    run_git_fixture(target.lp(), &["checkout", "-q", "main"]);
    let cwd = Fixture::new();
    let dir = target.lp().to_str().unwrap();
    let git_dir = target.lp().join(".git");
    let git_dir = git_dir.to_str().unwrap();
    let before = target.remote_refs();
    let cases: [&[&str]; 5] = [
        &["-C", dir, "push"],
        &["-C", dir, "push", "origin", "HEAD"],
        &["--git-dir", git_dir, "push"],
        &["--git-dir", git_dir, "push", "--force", "origin", "feat"],
        &["-C", dir, "push", "origin", "+feat"],
    ];
    for case in cases {
        let (_o, stderr) = run_fail(legion_git_in(cwd.data.path(), cwd.lp()).args(case));
        assert!(stderr.contains("rule"), "{case:?}: {stderr}");
        assert_eq!(target.remote_refs(), before, "{case:?}");
    }
    assert!(!target.pushed());
    assert!(!cwd.pushed());
}

// ---------------------------------------------------------------------------
// Real git
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn git_inexpressible_commits_run_real_git_audited() {
    let _guard = RealRepoConfigGuard::new();
    let cases: [(&[&str], i32); 3] = [
        (&["commit", "-am", "feat(x): y"], 0),
        (&["commit", "--amend", "--no-edit"], 0),
        (&["commit"], 1),
    ];
    for (case, want) in cases {
        let fx = Fixture::new();
        std::fs::write(fx.lp().join("feature.txt"), "edited\n").unwrap();
        fx.stage_change("staged.txt");
        let out = fx.legion_git(case).output().unwrap();
        assert_eq!(code(&out), want, "{case:?}");
        let rows = fx.rows();
        assert_real_git_row(one_row(&rows), "commit", case, want);
    }
}

/// A branch no worktree has checked out: the audited path would fail in
/// `resolve_checkout`, so real git pushes it. Not refused.
#[cfg(unix)]
#[test]
fn git_push_of_unchecked_out_branch_runs_real_git() {
    let _guard = RealRepoConfigGuard::new();
    let fx = Fixture::new();
    run_git_fixture(fx.lp(), &["checkout", "-q", "main"]);
    let args = ["push", "origin", "feat"];
    run_ok(&mut fx.legion_git(&args));
    assert_eq!(
        bare_ref(fx.remote.path(), "refs/heads/feat"),
        Some(local_rev(fx.lp(), "feat"))
    );
    let rows = fx.rows();
    let row = one_row(&rows);
    assert_real_git_row(row, "push", &args, 0);
    assert_eq!(row["target_ref"], "main");
}

#[cfg(unix)]
#[test]
fn git_push_tags_runs_real_git() {
    let _guard = RealRepoConfigGuard::new();
    let fx = Fixture::new();
    run_git_fixture(fx.lp(), &["tag", "v9"]);
    run_ok(&mut fx.legion_git(&["push", "--tags"]));
    assert!(bare_ref(fx.remote.path(), "refs/tags/v9").is_some());
    let rows = fx.rows();
    assert_real_git_row(one_row(&rows), "push", &["push", "--tags"], 0);
}

/// Other subcommands produce git's output and exit code, with no row.
#[cfg(unix)]
#[test]
fn git_other_subcommands_run_git_unaudited() {
    let fx = Fixture::new();
    std::fs::write(fx.lp().join("feature.txt"), "edited\n").unwrap();
    let cases: [&[&str]; 4] = [
        &["status"],
        &["log", "--oneline", "-3"],
        &["diff", "--stat"],
        &["rev-parse", "--verify", "nosuchref"],
    ];
    for case in cases {
        let ours = fx.legion_git(case).output().unwrap();
        let theirs = real_git_in(fx.lp()).args(case).output().unwrap();
        assert_eq!(code(&ours), code(&theirs), "{case:?}");
        assert_eq!(ours.stdout, theirs.stdout, "{case:?}");
    }
    let bad = fx
        .legion_git(&["rev-parse", "--verify", "nosuchref"])
        .output()
        .unwrap();
    assert_ne!(code(&bad), 0);
    assert!(fx.rows().is_empty());
}

// ---------------------------------------------------------------------------
// Audit identity
// ---------------------------------------------------------------------------

/// `LEGION_REPO` names the agent on both the audited and the real-git rows.
#[cfg(unix)]
#[test]
fn git_audit_agent_comes_from_legion_repo() {
    let _guard = RealRepoConfigGuard::new();
    let fx = Fixture::new();
    fx.stage_change("more.txt");
    let mut cmd = fx.legion_git(&["commit", "-m", "feat(x): y"]);
    run_ok(cmd.env("LEGION_REPO", "x"));
    let mut cmd = fx.legion_git(&["commit", "--amend", "--no-edit"]);
    run_ok(cmd.env("LEGION_REPO", "x"));
    let mut cmd = fx.legion_git(&["push", "origin", "feat"]);
    run_ok(cmd.env("LEGION_REPO", "x"));
    let rows = fx.rows();
    assert_eq!(rows.len(), 3);
    for row in &rows {
        assert_eq!(row["agent"], "x", "{row:#}");
    }
}

/// Without `LEGION_REPO`, run from a linked worktree, rows name the main
/// repository, not the worktree's directory.
#[cfg(unix)]
#[test]
fn git_audit_agent_from_a_worktree_names_the_main_repository() {
    let _guard = RealRepoConfigGuard::new();
    let remote = init_bare_remote();
    let root = tempfile::tempdir().unwrap();
    let main_repo: PathBuf = root.path().join("mainrepo");
    std::fs::create_dir_all(&main_repo).unwrap();
    init_local_at(&main_repo, remote.path());
    run_git_fixture(&main_repo, &["checkout", "-q", "main"]);
    let linked: PathBuf = root.path().join("linked-dir");
    run_git_fixture(
        &main_repo,
        &["worktree", "add", linked.to_str().unwrap(), "feat"],
    );
    std::fs::write(linked.join("more.txt"), "more\n").unwrap();
    let data = tempfile::tempdir().unwrap();

    let git_in_linked = |args: &[&str]| {
        let mut cmd = legion_git_in(data.path(), &linked);
        cmd.env_remove("LEGION_REPO").args(args);
        cmd
    };
    run_ok(&mut git_in_linked(&["add", "more.txt"]));
    run_ok(&mut git_in_linked(&["commit", "-m", "feat(x): y"]));
    run_ok(&mut git_in_linked(&["commit", "--amend", "--no-edit"]));
    run_ok(&mut git_in_linked(&["push", "origin", "feat"]));
    let rows = audit_rows(data.path());
    assert_eq!(rows.len(), 3, "{rows:#?}");
    for row in &rows {
        assert_eq!(row["agent"], "mainrepo", "{row:#}");
    }
}
