//! CLI end-to-end tests for the #1010 worktree-divergence guard.
//!
//! `legion sym`/`legion index` resolve a repo to the single workdir path
//! `watch.toml` registers for it. Invoked from inside a linked git worktree
//! on a branch that workdir's last `legion index` run never saw, they used
//! to answer for the registered (primary) checkout silently -- #1003's
//! `sym refs` miss. The fix is a guard (Sean's decision on #1010, not
//! worktree-aware resolution): before answering, compare the invoking
//! checkout's toplevel + HEAD against the registered workdir + its last
//! indexed HEAD, and print a WARNING naming both HEADs and the worktree
//! path when they diverge. The pure decision logic (`worktree_divergence_
//! warning`, `inventory::git_common_dir`) is unit-tested in
//! `src/cli/index_cmd.rs` and `src/inventory.rs`; these tests exercise the
//! actual binary surface named in the issue: `sym refs`, `sym etc
//! find-content`, and `legion index --file`, plus the primary-checkout
//! regression case.

use crate::common::{RealRepoConfigGuard, legion_cmd, run_ok, run_ok_stderr};

/// Seed a watch.toml in the data dir pointing at `repos` (name, workdir).
/// Backslashes are TOML escape syntax, so Windows paths interpolated raw
/// into a basic string make the whole file unparseable -- normalize to
/// forward slashes, which Windows path APIs accept.
fn seed_watch_toml(data_dir: &std::path::Path, repos: &[(&str, &std::path::Path)]) {
    let mut toml = String::new();
    for (name, workdir) in repos {
        toml.push_str(&format!(
            "[[repos]]\nname = \"{}\"\nworkdir = \"{}\"\n\n",
            name,
            workdir.display().to_string().replace('\\', "/")
        ));
    }
    std::fs::write(data_dir.join("watch.toml"), toml).expect("seed watch.toml");
}

/// Run `git` in `dir` with an isolated global/system config (never a real
/// `git config` write) but WITHOUT `common::run_git_fixture`'s `GIT_DIR`/
/// `GIT_WORK_TREE` overrides: those are hardcoded to `<dir>/.git`, which is
/// a plain FILE (not a directory) inside a linked worktree, so forcing them
/// would break every fixture command run there -- plain directory-based
/// discovery (`current_dir` only) resolves both a primary checkout and a
/// linked worktree correctly.
fn git_in(dir: &std::path::Path, args: &[&str]) {
    let (global, system) = crate::common::isolated_git_config_paths();
    let mut full_args: Vec<&str> = vec![
        "-c",
        "user.name=Legion Test Fixture",
        "-c",
        "user.email=legion-test-fixture@example.invalid",
        "-c",
        "commit.gpgsign=false",
    ];
    full_args.extend_from_slice(args);
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", global)
        .env("GIT_CONFIG_SYSTEM", system)
        .args(&full_args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} failed to spawn in {dir:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} exited non-zero in {dir:?}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Read-only counterpart to `git_in`, for a query whose stdout the caller
/// needs (`rev-parse HEAD`).
fn git_head(dir: &std::path::Path) -> String {
    let (global, system) = crate::common::isolated_git_config_paths();
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", global)
        .env("GIT_CONFIG_SYSTEM", system)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap_or_else(|e| panic!("git rev-parse HEAD failed to spawn in {dir:?}: {e}"));
    assert!(
        out.status.success(),
        "git rev-parse HEAD exited non-zero in {dir:?}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Add a linked worktree at `<repo_dir>/wt` on a new branch. Deliberately
/// NESTED inside `repo_dir` rather than a sibling tempdir: git tolerates
/// this (verified empirically), and nesting is what keeps `legion index
/// --file`'s existing path-prefix owner-walk (unchanged by #1010 -- that
/// resolution logic is explicitly out of scope) able to find the fixture's
/// registered repo as the file's owner. A sibling worktree would make
/// `index_file_warns_when_invoked_from_worktree_owning_repo` fail with "no
/// watch.toml entry owns" before the guard ever runs.
fn add_worktree(repo_dir: &std::path::Path) -> std::path::PathBuf {
    git_in(repo_dir, &["worktree", "add", "wt", "-b", "feature-branch"]);
    repo_dir.join("wt")
}

/// Build a minimal but real SCIP index for `repo_name` at `repo_dir` via the
/// `scip-rust` PATH-shim technique from `index_telemetry.rs`'s
/// `index_and_sym_def_refs_roundtrip_against_fixture_repo` (a pre-built
/// protobuf blob copied into place, rather than depending on a real
/// `rust-analyzer`/`scip-rust` binary) -- hermetic and fast; the guard
/// itself does not care about SCIP content, only HEADs, but `sym
/// refs`/`sym list` need a real index row to answer at all. Commits a
/// `Cargo.toml` + `src/lib.rs` defining `Greeter`/`hello` to a real git repo
/// (caller must `seed_watch_toml` first), runs `legion index repo_name`,
/// and returns the repo's HEAD at index time. Shared by every test here
/// that needs a queryable SCIP index rather than just an inventory
/// snapshot.
#[cfg(unix)]
fn seed_scip_fixture(
    data_dir: &std::path::Path,
    repo_dir: &std::path::Path,
    repo_name: &str,
) -> String {
    use protobuf::Message;
    use scip::types::{Document, Index, Occurrence, SymbolRole};
    use std::os::unix::fs::PermissionsExt;

    std::fs::write(
        repo_dir.join("Cargo.toml"),
        format!("[package]\nname = \"{repo_name}\"\nversion = \"0.1.0\"\n"),
    )
    .expect("write fixture");
    std::fs::create_dir_all(repo_dir.join("src")).expect("mkdir");
    std::fs::write(
        repo_dir.join("src/lib.rs"),
        "pub struct Greeter;\npub fn hello() {}\n",
    )
    .expect("write fixture");

    git_in(repo_dir, &["init", "-q", "-b", "main"]);
    git_in(repo_dir, &["add", "."]);
    git_in(repo_dir, &["commit", "-q", "-m", "initial"]);
    let primary_head = git_head(repo_dir);

    let symbol = format!("rust-analyzer cargo {repo_name} 0.1.0 src/lib.rs/Greeter#");
    let occurrence = |range: Vec<i32>, is_def: bool| {
        let mut o = Occurrence::new();
        o.symbol = symbol.clone();
        o.range = range;
        if is_def {
            o.symbol_roles = SymbolRole::Definition as i32;
        }
        o
    };
    let mut document = Document::new();
    document.relative_path = "src/lib.rs".to_string();
    document.occurrences = vec![
        occurrence(vec![4, 0, 4, 7], true),
        occurrence(vec![10, 8, 10, 15], false),
    ];
    let mut scip_index = Index::new();
    scip_index.documents = vec![document];
    let blob = scip_index.write_to_bytes().expect("serialize scip index");
    let blob_path = data_dir.join(format!("{repo_name}-fixture-index.scip"));
    std::fs::write(&blob_path, &blob).expect("write blob");

    let shim_dir = tempfile::tempdir().expect("shim dir");
    let shim = shim_dir.path().join("scip-rust");
    std::fs::write(
        &shim,
        format!("#!/bin/sh\ncp '{}' index.scip\n", blob_path.display()),
    )
    .expect("write shim");
    let mut perm = std::fs::metadata(&shim).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&shim, perm).unwrap();
    let shim_path = format!("{}:/usr/bin:/bin", shim_dir.path().display());

    run_ok(
        legion_cmd(data_dir)
            .env("PATH", &shim_path)
            .args(["index", repo_name]),
    );

    primary_head
}

/// #1010 acceptance test 1: `sym refs` from a linked worktree whose branch
/// has moved past the indexed HEAD warns, naming both HEADs.
#[cfg(unix)]
#[test]
fn sym_refs_warns_when_invoked_from_divergent_worktree() {
    let _guard = RealRepoConfigGuard::new();
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");

    seed_watch_toml(data_dir.path(), &[("wtguard", repo_dir.path())]);
    let primary_head = seed_scip_fixture(data_dir.path(), repo_dir.path(), "wtguard");

    // A linked worktree, checked out on a branch that carries one commit
    // the indexed HEAD never saw -- exactly #1003's "a caller that exists
    // only on the worktree's branch" scenario.
    let worktree_dir = add_worktree(repo_dir.path());
    std::fs::write(
        worktree_dir.join("src/lib.rs"),
        "pub struct Greeter;\npub fn hello() {}\npub fn hello2() { hello(); }\n",
    )
    .expect("write fixture");
    git_in(&worktree_dir, &["add", "."]);
    git_in(
        &worktree_dir,
        &["commit", "-q", "-m", "add hello2 call site"],
    );
    let worktree_head = git_head(&worktree_dir);
    assert_ne!(primary_head, worktree_head, "fixture must actually diverge");

    let stderr = run_ok_stderr(
        legion_cmd(data_dir.path())
            .current_dir(&worktree_dir)
            .args(["sym", "refs", "Greeter", "--repo", "wtguard"]),
    );
    assert!(
        stderr.contains("WARNING"),
        "expected a worktree-divergence warning, got:\n{stderr}"
    );
    assert!(
        stderr.contains(&primary_head[..7]),
        "expected the indexed HEAD ({}) to be named, got:\n{stderr}",
        &primary_head[..7]
    );
    assert!(
        stderr.contains(&worktree_head[..7]),
        "expected the invoking worktree's HEAD ({}) to be named, got:\n{stderr}",
        &worktree_head[..7]
    );
    let canon_wt = worktree_dir.canonicalize().expect("canonicalize worktree");
    assert!(
        stderr.contains(canon_wt.to_str().expect("utf8 path")),
        "expected the worktree path to be named, got:\n{stderr}"
    );
}

/// #1010 review finding MED 2(a): `sym list` also warns -- a different
/// SCIP-reading dispatch path (`run_sym_list`) than `sym refs`/`sym def`/
/// `sym impl`/`sym hover` (`run_location_query`/`run_hover_query`), guarded
/// separately in `run_sym_list` because `sym list --lang css` diverts to
/// `run_css_sym_list` before ever reaching those.
#[cfg(unix)]
#[test]
fn sym_list_warns_when_invoked_from_divergent_worktree() {
    let _guard = RealRepoConfigGuard::new();
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");

    seed_watch_toml(data_dir.path(), &[("symlistguard", repo_dir.path())]);
    let primary_head = seed_scip_fixture(data_dir.path(), repo_dir.path(), "symlistguard");

    let worktree_dir = add_worktree(repo_dir.path());
    std::fs::write(
        worktree_dir.join("src/lib.rs"),
        "pub struct Greeter;\npub fn hello() {}\npub fn hello2() { hello(); }\n",
    )
    .expect("write fixture");
    git_in(&worktree_dir, &["add", "."]);
    git_in(
        &worktree_dir,
        &["commit", "-q", "-m", "add hello2 call site"],
    );
    let worktree_head = git_head(&worktree_dir);
    assert_ne!(primary_head, worktree_head, "fixture must actually diverge");

    let stderr = run_ok_stderr(
        legion_cmd(data_dir.path())
            .current_dir(&worktree_dir)
            .args(["sym", "list", "--repo", "symlistguard"]),
    );
    assert!(
        stderr.contains("WARNING"),
        "expected a worktree-divergence warning, got:\n{stderr}"
    );
    assert!(
        stderr.contains(&primary_head[..7]),
        "expected the indexed HEAD to be named, got:\n{stderr}"
    );
    assert!(
        stderr.contains(&worktree_head[..7]),
        "expected the invoking worktree's HEAD to be named, got:\n{stderr}"
    );
}

/// #1010 acceptance test 2: `sym etc find-content` from the same divergent
/// worktree also warns -- the live-scan/inventory read path, not the SCIP
/// blob path.
#[test]
fn find_content_warns_when_invoked_from_divergent_worktree() {
    let _guard = RealRepoConfigGuard::new();
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    let state_dir = tempfile::tempdir().expect("state dir");

    std::fs::write(repo_dir.path().join("NOTES.md"), "needle-token here\n").expect("write");
    git_in(repo_dir.path(), &["init", "-q", "-b", "main"]);
    git_in(repo_dir.path(), &["add", "."]);
    git_in(repo_dir.path(), &["commit", "-q", "-m", "initial"]);
    let primary_head = git_head(repo_dir.path());

    seed_watch_toml(data_dir.path(), &[("fcguard", repo_dir.path())]);
    run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args(["index", "fcguard"]),
    );

    let worktree_dir = add_worktree(repo_dir.path());
    std::fs::write(worktree_dir.join("EXTRA.md"), "more content\n").expect("write");
    git_in(&worktree_dir, &["add", "."]);
    git_in(&worktree_dir, &["commit", "-q", "-m", "add extra doc"]);
    let worktree_head = git_head(&worktree_dir);
    assert_ne!(primary_head, worktree_head, "fixture must actually diverge");

    let stderr = run_ok_stderr(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .current_dir(&worktree_dir)
            .args([
                "sym",
                "etc",
                "find-content",
                "needle-token",
                "--repo",
                "fcguard",
            ]),
    );
    assert!(
        stderr.contains("WARNING"),
        "expected a worktree-divergence warning, got:\n{stderr}"
    );
    assert!(
        stderr.contains(&primary_head[..7]),
        "expected the indexed HEAD to be named, got:\n{stderr}"
    );
    assert!(
        stderr.contains(&worktree_head[..7]),
        "expected the invoking worktree's HEAD to be named, got:\n{stderr}"
    );
}

/// #1010 review finding MED 1: `sym etc find-content` with NO `--repo` --
/// the default scope for every guarded surface, and the exact shape the
/// issue's own example uses (`sym refs update_prediction`, no `--repo`) --
/// still warns for the one repo whose common dir actually matches the
/// invoking worktree, and stays silent about a SECOND, unrelated repo
/// registered in the same watch.toml even though that repo is ALSO
/// indexed (so it has a real snapshot a naive, non-identity-gated loop
/// could wrongly compare against).
#[test]
fn find_content_with_no_repo_flag_warns_for_matching_repo_only() {
    let _guard = RealRepoConfigGuard::new();
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    let other_dir = tempfile::tempdir().expect("other repo dir");
    let state_dir = tempfile::tempdir().expect("state dir");

    std::fs::write(repo_dir.path().join("NOTES.md"), "needle-token here\n").expect("write");
    git_in(repo_dir.path(), &["init", "-q", "-b", "main"]);
    git_in(repo_dir.path(), &["add", "."]);
    git_in(repo_dir.path(), &["commit", "-q", "-m", "initial"]);
    let primary_head = git_head(repo_dir.path());

    std::fs::write(other_dir.path().join("OTHER.md"), "unrelated\n").expect("write");
    git_in(other_dir.path(), &["init", "-q", "-b", "main"]);
    git_in(other_dir.path(), &["add", "."]);
    git_in(other_dir.path(), &["commit", "-q", "-m", "other initial"]);

    seed_watch_toml(
        data_dir.path(),
        &[
            ("fcnorepoguard", repo_dir.path()),
            ("otherrepo", other_dir.path()),
        ],
    );
    run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args(["index", "fcnorepoguard"]),
    );
    run_ok(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .args(["index", "otherrepo"]),
    );

    let worktree_dir = add_worktree(repo_dir.path());
    std::fs::write(worktree_dir.join("EXTRA.md"), "more content\n").expect("write");
    git_in(&worktree_dir, &["add", "."]);
    git_in(&worktree_dir, &["commit", "-q", "-m", "add extra doc"]);
    let worktree_head = git_head(&worktree_dir);
    assert_ne!(primary_head, worktree_head, "fixture must actually diverge");

    let stderr = run_ok_stderr(
        legion_cmd(data_dir.path())
            .env("XDG_STATE_HOME", state_dir.path())
            .current_dir(&worktree_dir)
            .args(["sym", "etc", "find-content", "needle-token"]),
    );
    assert!(
        stderr.contains("WARNING"),
        "expected a worktree-divergence warning for the matching repo, got:\n{stderr}"
    );
    assert!(
        stderr.contains("fcnorepoguard"),
        "expected the warning to name the matching repo, got:\n{stderr}"
    );
    assert!(
        !stderr.contains("otherrepo"),
        "must not mention the unrelated repo at all, got:\n{stderr}"
    );
    assert!(
        stderr.contains(&primary_head[..7]),
        "expected the indexed HEAD to be named, got:\n{stderr}"
    );
    assert!(
        stderr.contains(&worktree_head[..7]),
        "expected the invoking worktree's HEAD to be named, got:\n{stderr}"
    );
}

/// #1010 review finding LOW 2: the bare `legion index <repo>` form (no
/// `--file`) also warns when invoked from a divergent worktree -- the guard
/// covers both call shapes of `legion index`, not just `--file`. The
/// seeding `legion index plainguard` run MUST happen first so an
/// `inventory_snapshots` row exists before the divergence-invoking second
/// run; without that seed, `warn_worktree_divergence` early-returns on a
/// missing snapshot and this test would pass vacuously (no warning either
/// way). That seeding run's own cwd -- wherever `cargo test` happens to run
/// from -- is itself an unrelated repo relative to the fixture, which the
/// identity gate keeps silent on its own.
#[test]
fn plain_index_warns_when_invoked_from_divergent_worktree() {
    let _guard = RealRepoConfigGuard::new();
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");

    std::fs::write(repo_dir.path().join("README.md"), "docs only\n").expect("write");
    git_in(repo_dir.path(), &["init", "-q", "-b", "main"]);
    git_in(repo_dir.path(), &["add", "."]);
    git_in(repo_dir.path(), &["commit", "-q", "-m", "initial"]);
    let primary_head = git_head(repo_dir.path());

    seed_watch_toml(data_dir.path(), &[("plainguard", repo_dir.path())]);
    run_ok(legion_cmd(data_dir.path()).args(["index", "plainguard"]));

    let worktree_dir = add_worktree(repo_dir.path());
    std::fs::write(
        worktree_dir.join("NOTES.md"),
        "new file only on the branch\n",
    )
    .expect("write");
    git_in(&worktree_dir, &["add", "."]);
    git_in(&worktree_dir, &["commit", "-q", "-m", "add notes"]);
    let worktree_head = git_head(&worktree_dir);
    assert_ne!(primary_head, worktree_head, "fixture must actually diverge");

    let stderr = run_ok_stderr(
        legion_cmd(data_dir.path())
            .current_dir(&worktree_dir)
            .args(["index", "plainguard"]),
    );
    assert!(
        stderr.contains("WARNING"),
        "expected a worktree-divergence warning, got:\n{stderr}"
    );
    assert!(
        stderr.contains(&primary_head[..7]),
        "expected the indexed HEAD to be named, got:\n{stderr}"
    );
    assert!(
        stderr.contains(&worktree_head[..7]),
        "expected the invoking worktree's HEAD to be named, got:\n{stderr}"
    );
    let canon_wt = worktree_dir.canonicalize().expect("canonicalize worktree");
    assert!(
        stderr.contains(canon_wt.to_str().expect("utf8 path")),
        "expected the worktree path to be named, got:\n{stderr}"
    );
}

/// #1010 acceptance test 3: `legion index --file <path-under-worktree>`
/// warns too. Unlike the previous two tests, this invocation passes no
/// `--repo` and is not run with `.current_dir` set to the worktree at all --
/// the guard must derive the invoking checkout from the `--file` argument's
/// own directory, exactly as the issue specifies, since the operator can
/// run `legion index --file <path>` from anywhere.
#[test]
fn index_file_warns_when_invoked_from_worktree_owning_repo() {
    let _guard = RealRepoConfigGuard::new();
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");

    std::fs::write(repo_dir.path().join("README.md"), "docs only\n").expect("write");
    git_in(repo_dir.path(), &["init", "-q", "-b", "main"]);
    git_in(repo_dir.path(), &["add", "."]);
    git_in(repo_dir.path(), &["commit", "-q", "-m", "initial"]);
    let primary_head = git_head(repo_dir.path());

    seed_watch_toml(data_dir.path(), &[("ifguard", repo_dir.path())]);
    run_ok(legion_cmd(data_dir.path()).args(["index", "ifguard"]));

    let worktree_dir = add_worktree(repo_dir.path());
    std::fs::write(
        worktree_dir.join("NOTES.md"),
        "new file only on the branch\n",
    )
    .expect("write");
    git_in(&worktree_dir, &["add", "."]);
    git_in(&worktree_dir, &["commit", "-q", "-m", "add notes"]);
    let worktree_head = git_head(&worktree_dir);
    assert_ne!(primary_head, worktree_head, "fixture must actually diverge");

    let file_path = worktree_dir.join("NOTES.md");
    let stderr = run_ok_stderr(legion_cmd(data_dir.path()).args([
        "index",
        "--file",
        file_path.to_str().expect("utf8 path"),
    ]));
    assert!(
        stderr.contains("WARNING"),
        "expected a worktree-divergence warning, got:\n{stderr}"
    );
    assert!(
        stderr.contains(&primary_head[..7]),
        "expected the indexed HEAD to be named, got:\n{stderr}"
    );
    assert!(
        stderr.contains(&worktree_head[..7]),
        "expected the invoking worktree's HEAD to be named, got:\n{stderr}"
    );
    // `--file` still indexes the OWNING (registered/primary) repo, not the
    // worktree -- the guard only warns, it never changes what is indexed
    // (out of scope: worktree-aware resolution). This fixture is docs-only
    // (no Cargo.toml), so the normal confirmation is the inventory summary,
    // not a SCIP "indexed" line.
    assert!(
        stderr.contains("inventoried") && stderr.contains("ifguard"),
        "expected the normal index confirmation to still print, got:\n{stderr}"
    );
}

/// #1010 acceptance test 4 (regression): invoked from the PRIMARY checkout
/// with a matching HEAD, behavior is unchanged -- no WARNING, and the
/// command's output/exit code are exactly what they were before this guard
/// existed. Extends `find_file.rs`'s
/// `find_file_regression_head_drift_surfaced_instead_of_silent` pattern:
/// same real-git-repo + `sym etc find-file` setup as that test's "matching
/// heads" sibling (`find_file_human_output_prints_up_to_date_when_head_
/// matches`), but additionally invoked with cwd pinned to the registered
/// workdir itself -- the one case this guard must leave completely alone.
#[test]
fn find_file_from_primary_checkout_with_matching_head_is_unchanged() {
    let _guard = RealRepoConfigGuard::new();
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");
    let state_dir = tempfile::tempdir().expect("state dir");

    git_in(repo_dir.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(repo_dir.path().join("a.rs"), "fn a() {}\n").expect("write fixture");
    git_in(repo_dir.path(), &["add", "a.rs"]);
    git_in(repo_dir.path(), &["commit", "-q", "-m", "initial"]);

    seed_watch_toml(data_dir.path(), &[("primaryguard", repo_dir.path())]);
    // This seeding run's own cwd is wherever `cargo test` happens to run
    // from -- an unrelated repo relative to `primaryguard`'s fixture, which
    // the identity gate must keep silent on its own; asserted here to close
    // the negative direction for the plain `legion index <repo>` form too.
    let seed_stderr = run_ok_stderr(legion_cmd(data_dir.path()).args(["index", "primaryguard"]));
    assert!(
        !seed_stderr.contains("WARNING"),
        "an unrelated repo at cwd must never warn, got:\n{seed_stderr}"
    );

    let out = legion_cmd(data_dir.path())
        .env("XDG_STATE_HOME", state_dir.path())
        .current_dir(repo_dir.path())
        .args(["sym", "etc", "find-file", "a.rs", "--repo", "primaryguard"])
        .output()
        .expect("legion must spawn");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Unchanged from before #1010: the existing #746 freshness line still
    // reports "up to date" (HEAD matches), the entry still prints, and
    // nothing new appears.
    assert!(
        stderr.contains("primaryguard: indexed") && stderr.contains("up to date"),
        "expected the ordinary up-to-date freshness line, unchanged, got:\n{stderr}"
    );
    assert!(
        !stderr.contains("WARNING"),
        "invoking from the registered (primary) workdir must never warn, got:\n{stderr}"
    );
    assert!(!stderr.contains("invoked from worktree"), "got:\n{stderr}");
    assert!(
        stdout.contains("a.rs"),
        "expected the match to still print, got:\n{stdout}"
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "exit code must be exactly what it was before this guard existed"
    );
}

/// #1010 review finding MED 2(b): `sym impact` (`run_sym_impact`) is a
/// third, separate SCIP-reading dispatch path from `run_location_query`
/// and `run_sym_list`, and needs its own guard call.
#[cfg(unix)]
#[test]
fn sym_impact_warns_when_invoked_from_divergent_worktree() {
    let _guard = RealRepoConfigGuard::new();
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");

    seed_watch_toml(data_dir.path(), &[("impactguard", repo_dir.path())]);
    let primary_head = seed_scip_fixture(data_dir.path(), repo_dir.path(), "impactguard");

    let worktree_dir = add_worktree(repo_dir.path());
    std::fs::write(
        worktree_dir.join("src/lib.rs"),
        "pub struct Greeter;\npub fn hello() {}\npub fn hello2() { hello(); }\n",
    )
    .expect("write fixture");
    git_in(&worktree_dir, &["add", "."]);
    git_in(
        &worktree_dir,
        &["commit", "-q", "-m", "add hello2 call site"],
    );
    let worktree_head = git_head(&worktree_dir);
    assert_ne!(primary_head, worktree_head, "fixture must actually diverge");

    // A trivial unified diff naming a line inside src/lib.rs -- `sym
    // impact` only needs the diff to parse; the impact-radius result
    // itself is not what this test is checking.
    let diff_path = worktree_dir.join("change.diff");
    std::fs::write(
        &diff_path,
        "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,2 @@\n pub struct Greeter;\n-pub fn hello() {}\n+pub fn hello() { }\n",
    )
    .expect("write diff");

    let stderr = run_ok_stderr(
        legion_cmd(data_dir.path())
            .current_dir(&worktree_dir)
            .args([
                "sym",
                "impact",
                "--repo",
                "impactguard",
                "--diff",
                diff_path.to_str().expect("utf8 path"),
            ]),
    );
    assert!(
        stderr.contains("WARNING"),
        "expected a worktree-divergence warning, got:\n{stderr}"
    );
    assert!(
        stderr.contains(&primary_head[..7]),
        "expected the indexed HEAD to be named, got:\n{stderr}"
    );
    assert!(
        stderr.contains(&worktree_head[..7]),
        "expected the invoking worktree's HEAD to be named, got:\n{stderr}"
    );
}

/// #1010 review finding MED 2(c): `sym def --lang css` dispatches to
/// `run_css_sym_def`, a code path `run_location_query` never touches (css
/// is not a SCIP language) -- guarded directly in `run_sym_action`'s
/// `SymAction::Def` arm before the css/non-css split. No SCIP shim needed:
/// css symbols come straight from the file-inventory walk.
#[test]
fn sym_def_css_warns_when_invoked_from_divergent_worktree() {
    let _guard = RealRepoConfigGuard::new();
    let data_dir = tempfile::tempdir().expect("data dir");
    let repo_dir = tempfile::tempdir().expect("repo dir");

    std::fs::write(repo_dir.path().join("style.css"), ".foo { color: red; }\n")
        .expect("write fixture");
    git_in(repo_dir.path(), &["init", "-q", "-b", "main"]);
    git_in(repo_dir.path(), &["add", "."]);
    git_in(repo_dir.path(), &["commit", "-q", "-m", "initial"]);
    let primary_head = git_head(repo_dir.path());

    seed_watch_toml(data_dir.path(), &[("cssguard", repo_dir.path())]);
    run_ok(legion_cmd(data_dir.path()).args(["index", "cssguard"]));

    let worktree_dir = add_worktree(repo_dir.path());
    std::fs::write(worktree_dir.join("EXTRA.css"), ".bar { color: blue; }\n")
        .expect("write fixture");
    git_in(&worktree_dir, &["add", "."]);
    git_in(&worktree_dir, &["commit", "-q", "-m", "add extra css"]);
    let worktree_head = git_head(&worktree_dir);
    assert_ne!(primary_head, worktree_head, "fixture must actually diverge");

    let stderr = run_ok_stderr(
        legion_cmd(data_dir.path())
            .current_dir(&worktree_dir)
            .args(["sym", "def", ".foo", "--lang", "css", "--repo", "cssguard"]),
    );
    assert!(
        stderr.contains("WARNING"),
        "expected a worktree-divergence warning, got:\n{stderr}"
    );
    assert!(
        stderr.contains(&primary_head[..7]),
        "expected the indexed HEAD to be named, got:\n{stderr}"
    );
    assert!(
        stderr.contains(&worktree_head[..7]),
        "expected the invoking worktree's HEAD to be named, got:\n{stderr}"
    );
}
