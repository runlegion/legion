//! CLI end-to-end tests for sym in a git worktree (#1503).
//!
//! Run inside a linked worktree of a registered repo, `legion sym` and
//! `legion sym etc` answer from the worktree's own index, stored under the
//! worktree's canonical path, and reindex it first when it is missing or
//! not current. `legion index <repo>` run there indexes the worktree. The
//! registered checkout's stored index is never written from a worktree.

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
/// a plain FILE (not a directory) inside a linked worktree.
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

/// A SCIP index whose `src/lib.rs` defines each of `names`.
#[cfg(unix)]
fn scip_blob(repo_name: &str, names: &[&str]) -> Vec<u8> {
    use protobuf::Message;
    use scip::types::{Document, Index, Occurrence, SymbolRole};

    let mut document = Document::new();
    document.relative_path = "src/lib.rs".to_string();
    document.occurrences = names
        .iter()
        .zip(0..)
        .map(|(name, line)| {
            let mut o = Occurrence::new();
            o.symbol = format!("rust-analyzer cargo {repo_name} 0.1.0 src/lib.rs/{name}#");
            o.range = vec![line, 0, line, 7];
            o.symbol_roles = SymbolRole::Definition as i32;
            o
        })
        .collect();
    let mut index = Index::new();
    index.documents = vec![document];
    index.write_to_bytes().expect("serialize scip index")
}

/// A registered repo `name` with a linked worktree, and a `scip-rust` shim
/// on PATH that copies the indexed checkout's own `fixture.scip`, so each
/// checkout, and each edit to one, indexes to its own blob.
#[cfg(unix)]
struct Fixture {
    _guard: RealRepoConfigGuard,
    data_dir: tempfile::TempDir,
    state_dir: tempfile::TempDir,
    _shim_dir: tempfile::TempDir,
    shim_path: String,
    repo_dir: tempfile::TempDir,
    _trees: tempfile::TempDir,
    tree: std::path::PathBuf,
    name: &'static str,
}

#[cfg(unix)]
impl Fixture {
    fn new(name: &'static str) -> Self {
        use std::os::unix::fs::PermissionsExt;

        let guard = RealRepoConfigGuard::new();
        let data_dir = tempfile::tempdir().expect("data dir");
        let state_dir = tempfile::tempdir().expect("state dir");
        let repo_dir = tempfile::tempdir().expect("repo dir");
        let trees = tempfile::tempdir().expect("trees dir");

        std::fs::write(
            repo_dir.path().join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n"),
        )
        .expect("write fixture");
        std::fs::create_dir_all(repo_dir.path().join("src")).expect("mkdir");
        std::fs::write(repo_dir.path().join("src/lib.rs"), "pub struct Greeter;\n")
            .expect("write fixture");
        std::fs::write(repo_dir.path().join(".gitignore"), "fixture.scip\n").expect("write");
        git_in(repo_dir.path(), &["init", "-q", "-b", "main"]);
        git_in(repo_dir.path(), &["add", "."]);
        git_in(repo_dir.path(), &["commit", "-q", "-m", "initial"]);
        std::fs::write(
            repo_dir.path().join("fixture.scip"),
            scip_blob(name, &["Greeter"]),
        )
        .expect("write blob");

        // A sibling of the registered checkout, outside every workdir.
        let tree = trees
            .path()
            .canonicalize()
            .expect("canonical trees dir")
            .join("wt");
        git_in(
            repo_dir.path(),
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feature",
                tree.to_str().expect("utf-8"),
            ],
        );

        let shim_dir = tempfile::tempdir().expect("shim dir");
        let shim = shim_dir.path().join("scip-rust");
        std::fs::write(&shim, "#!/bin/sh\ncp fixture.scip index.scip\n").expect("write shim");
        let mut perm = std::fs::metadata(&shim).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&shim, perm).unwrap();
        let shim_path = format!("{}:/usr/bin:/bin", shim_dir.path().display());

        seed_watch_toml(data_dir.path(), &[(name, repo_dir.path())]);
        let fixture = Self {
            _guard: guard,
            data_dir,
            state_dir,
            _shim_dir: shim_dir,
            shim_path,
            repo_dir,
            _trees: trees,
            tree,
            name,
        };
        run_ok(
            fixture
                .legion(fixture.repo_dir.path())
                .args(["index", name]),
        );
        fixture
    }

    fn legion(&self, cwd: &std::path::Path) -> std::process::Command {
        let mut cmd = legion_cmd(self.data_dir.path());
        cmd.env("PATH", &self.shim_path)
            .env("XDG_STATE_HOME", self.state_dir.path())
            .current_dir(cwd);
        cmd
    }

    /// The tree's `src/lib.rs` and index blob now define `names`.
    fn define_in_tree(&self, names: &[&str]) {
        let source: String = names.iter().map(|n| format!("pub struct {n};\n")).collect();
        std::fs::write(self.tree.join("src/lib.rs"), source).expect("write source");
        std::fs::write(self.tree.join("fixture.scip"), scip_blob(self.name, names))
            .expect("write blob");
    }

    fn tree_key(&self) -> String {
        self.tree.display().to_string()
    }

    /// `legion index <key> --status --json`'s `(repo, updated_at)` rows.
    fn stored(&self, key: &str) -> Vec<(String, String)> {
        let out = run_ok(
            self.legion(self.repo_dir.path())
                .args(["index", key, "--status", "--json"]),
        );
        let rows: Vec<serde_json::Value> = serde_json::from_str(&out).expect("status json");
        rows.iter()
            .map(|r| {
                (
                    r["repo"].as_str().unwrap_or_default().to_string(),
                    r["updated_at"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect()
    }

    fn main_row(&self) -> Option<(String, String)> {
        self.stored(self.name).into_iter().next()
    }
}

#[cfg(unix)]
#[test]
fn sym_in_a_tree_finds_a_branch_only_symbol_and_the_main_checkout_does_not() {
    let fx = Fixture::new("treesym");
    fx.define_in_tree(&["Greeter", "BranchOnly"]);
    let main_before = fx.main_row().expect("main checkout indexed");

    // No flag, inside the tree: the tree is indexed under its own key and
    // answers.
    let (out, stderr) = {
        let output = fx
            .legion(&fx.tree)
            .args(["sym", "def", "BranchOnly"])
            .output()
            .expect("run legion");
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        (
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        )
    };
    assert!(out.contains("src/lib.rs"), "got:\n{out}\nstderr:\n{stderr}");
    assert!(out.contains(&fx.tree_key()), "got:\n{out}");
    assert!(stderr.contains("indexing worktree"), "got:\n{stderr}");

    // Naming the tree's own repo answers from the tree too.
    let named = run_ok(
        fx.legion(&fx.tree)
            .args(["sym", "def", "BranchOnly", "--repo", fx.name]),
    );
    assert!(named.contains(&fx.tree_key()), "got:\n{named}");

    // The main checkout does not see the branch's symbol, with or without
    // --repo.
    for args in [
        vec!["sym", "def", "BranchOnly", "--repo", fx.name],
        vec!["sym", "def", "BranchOnly"],
    ] {
        let output = fx
            .legion(fx.repo_dir.path())
            .args(&args)
            .output()
            .expect("run legion");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(!stdout.contains("BranchOnly") && !stdout.contains("src/lib.rs"));
    }

    // The tree's index is stored under its key; main's is unchanged.
    assert!(!fx.stored(&fx.tree_key()).is_empty());
    assert_eq!(fx.main_row(), Some(main_before));
}

#[cfg(unix)]
#[test]
fn a_change_in_the_tree_is_reindexed_on_the_next_sym_call_and_only_then() {
    let fx = Fixture::new("treeedit");
    fx.define_in_tree(&["Greeter"]);
    let main_before = fx.main_row().expect("main checkout indexed");
    run_ok(fx.legion(&fx.tree).args(["sym", "list"]));

    // Nothing changed: the stored index is current and answers as is.
    let stderr = run_ok_stderr(fx.legion(&fx.tree).args(["sym", "list"]));
    assert!(!stderr.contains("indexing worktree"), "got:\n{stderr}");

    fx.define_in_tree(&["Greeter", "AddedLater"]);
    let output = fx
        .legion(&fx.tree)
        .args(["sym", "def", "AddedLater"])
        .output()
        .expect("run legion");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");
    assert!(stderr.contains("indexing worktree"), "got:\n{stderr}");
    assert!(stdout.contains("src/lib.rs"), "got:\n{stdout}");
    assert_eq!(fx.main_row(), Some(main_before));
}

#[cfg(unix)]
#[test]
fn find_content_in_a_tree_scans_the_tree() {
    let fx = Fixture::new("treegrep");
    std::fs::write(fx.tree.join("NOTES.md"), "needle-on-the-branch\n").expect("write");

    let in_tree =
        run_ok(
            fx.legion(&fx.tree)
                .args(["sym", "etc", "find-content", "needle-on-the-branch"]),
        );
    assert!(in_tree.contains("NOTES.md:1"), "got:\n{in_tree}");

    let in_main = run_ok(fx.legion(fx.repo_dir.path()).args([
        "sym",
        "etc",
        "find-content",
        "needle-on-the-branch",
        "--repo",
        fx.name,
    ]));
    assert!(!in_main.contains("NOTES.md"), "got:\n{in_main}");
}

#[cfg(unix)]
#[test]
fn index_run_in_a_tree_indexes_the_tree_and_never_the_main_checkout() {
    let fx = Fixture::new("treeindex");
    fx.define_in_tree(&["Greeter", "BranchOnly"]);
    let main_before = fx.main_row().expect("main checkout indexed");

    run_ok(fx.legion(&fx.tree).args(["index", fx.name]));
    run_ok(fx.legion(&fx.tree).args([
        "index",
        "--file",
        fx.tree.join("src/lib.rs").to_str().expect("utf-8"),
    ]));

    assert!(!fx.stored(&fx.tree_key()).is_empty());
    assert_eq!(fx.main_row(), Some(main_before));
}
