//! Integration tests: repo identity is registered, never taken from the
//! working directory (#1479).

use std::path::Path;

use crate::common::*;

/// A data dir whose watch.toml registers `name` at a fresh checkout dir.
/// The checkout's basename differs from `name`, so a pass proves the name
/// came from watch.toml, not the directory.
fn registered_checkout(name: &str) -> (tempfile::TempDir, tempfile::TempDir) {
    let data = tempfile::tempdir().unwrap();
    let checkout = tempfile::tempdir().unwrap();
    seed_watch_toml(data.path(), &[(name, checkout.path())]);
    (data, checkout)
}

fn recall_finds(data: &Path, repo: &str, text: &str) -> bool {
    run_ok(legion_cmd(data).args(["recall", "--repo", repo, "--context", text])).contains(text)
}

#[test]
fn a_command_inside_a_registered_repo_files_under_its_registered_name() {
    let (data, checkout) = registered_checkout("widget");
    let subdir = checkout.path().join("src");
    std::fs::create_dir_all(&subdir).unwrap();
    run_ok(legion_cmd(data.path()).current_dir(&subdir).args([
        "reflect",
        "--text",
        "registered names travel",
    ]));
    assert!(recall_finds(
        data.path(),
        "widget",
        "registered names travel"
    ));
}

#[test]
fn a_command_outside_a_registered_repo_stops_and_says_how_to_name_one() {
    let (data, _checkout) = registered_checkout("widget");
    let elsewhere = tempfile::tempdir().unwrap();
    let (_, stderr) = run_fail(legion_cmd(data.path()).current_dir(elsewhere.path()).args([
        "reflect",
        "--text",
        "must not land anywhere",
    ]));
    assert!(stderr.contains("--repo"), "{stderr}");
    assert!(stderr.contains("not in a registered repo"), "{stderr}");
    assert!(stderr.contains("legion watch add"), "{stderr}");
    let basename = elsewhere.path().file_name().unwrap().to_str().unwrap();
    assert!(!recall_finds(
        data.path(),
        basename,
        "must not land anywhere"
    ));
}

#[test]
fn signal_without_repo_stops_outside_and_defaults_inside_a_registered_repo() {
    let (data, checkout) = registered_checkout("widget");
    let elsewhere = tempfile::tempdir().unwrap();
    let signal = [
        "signal", "--to", "gadget", "--verb", "info", "--note", "hello",
    ];
    let (_, stderr) = run_fail(
        legion_cmd(data.path())
            .current_dir(elsewhere.path())
            .args(signal),
    );
    assert!(stderr.contains("not in a registered repo"), "{stderr}");

    run_ok(
        legion_cmd(data.path())
            .current_dir(checkout.path())
            .args(signal),
    );
    let board = run_ok(legion_cmd(data.path()).args(["bullpen", "--repo", "platform"]));
    assert!(
        board.contains("@gadget"),
        "the signal was recorded: {board}"
    );
}

#[test]
fn every_repo_flag_accepts_owner_repo_through_the_github_field() {
    let data = tempfile::tempdir().unwrap();
    let checkout = tempfile::tempdir().unwrap();
    std::fs::write(
        data.path().join("watch.toml"),
        format!(
            "[[repos]]\nname = \"widget\"\nworkdir = \"{}\"\ngithub = \"acme/widget-repo\"\n",
            checkout.path().display()
        ),
    )
    .unwrap();
    run_ok(legion_cmd(data.path()).args([
        "reflect",
        "--repo",
        "acme/widget-repo",
        "--text",
        "owner slash repo maps",
    ]));
    assert!(recall_finds(
        data.path(),
        "acme/widget-repo",
        "owner slash repo maps"
    ));
    assert!(recall_finds(data.path(), "widget", "owner slash repo maps"));

    let (_, stderr) = run_fail(legion_cmd(data.path()).args([
        "reflect",
        "--repo",
        "acme/unknown",
        "--text",
        "x",
    ]));
    assert!(stderr.contains("acme/unknown"), "{stderr}");
}

#[test]
fn repo_resolve_answers_registered_then_session_lock_then_unregistered() {
    let (data, checkout) = registered_checkout("widget");
    let resolve = |cwd: &Path, pid: Option<&str>| {
        let mut cmd = legion_cmd(data.path());
        cmd.args(["repo", "resolve", "--cwd"]).arg(cwd);
        if let Some(pid) = pid {
            cmd.args(["--pid", pid]);
        }
        run_ok(&mut cmd).trim().to_string()
    };
    assert_eq!(resolve(checkout.path(), None), "widget");

    let elsewhere = tempfile::tempdir().unwrap();
    assert_eq!(resolve(elsewhere.path(), None), "unregistered");

    let sessions = data.path().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(sessions.join("widget.session"), "424242").unwrap();
    assert_eq!(resolve(elsewhere.path(), Some("424242")), "widget");
    assert_eq!(resolve(elsewhere.path(), Some("1")), "unregistered");
}

#[test]
fn repo_resolve_maps_a_linked_worktree_to_its_main_checkouts_repo() {
    let data = tempfile::tempdir().unwrap();
    let repo = gate_fixture_repo();
    seed_watch_toml(data.path(), &[("widget", repo.path())]);
    let parent = tempfile::tempdir().unwrap();
    let worktree = parent.path().join("agent-wt");
    run_git_fixture(
        repo.path(),
        &[
            "worktree",
            "add",
            "-q",
            worktree.to_str().unwrap(),
            "-b",
            "agent",
        ],
    );
    let out = run_ok(
        legion_cmd(data.path())
            .args(["repo", "resolve", "--cwd"])
            .arg(&worktree),
    );
    assert_eq!(out.trim(), "widget");
}
