//! Integration tests: `legion gh`, the gh proxy (#1336).
//!
//! A stub `gh` sits first on PATH. It appends each invocation's argv to a
//! log (NUL between arguments, 0x1e after each invocation), prints a fixed
//! line to stdout and to stderr, and exits 3. The verb path uses the same
//! stub-plugin pattern as `worksource_pr.rs`: a bash worksource that logs
//! each call and answers from fixtures, with a watch.toml entry
//! `name = "stub"`, `github = "owner/stub"`.
//!
//! Unix-only: the stubs are bash scripts with an exec bit.

#![cfg(unix)]

use crate::common::*;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const STUB_OUT: &str = "stub-gh stdout line";
const STUB_ERR: &str = "stub-gh stderr line";

/// Write an executable script.
fn write_exec(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    let mut perm = std::fs::metadata(path).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(path, perm).unwrap();
}

/// One test's world: a data dir, a PATH dir holding the stub gh, a plugin
/// root holding the stub worksource, and the logs both stubs write.
struct World {
    data: tempfile::TempDir,
    bin: tempfile::TempDir,
    plugin_root: tempfile::TempDir,
}

impl World {
    /// A world whose stub worksource answers `pr-checks` with `checks_json`,
    /// `pr-list` with `pr_list_json`, and `view-issue` with an issue whose
    /// body carries acceptance criteria.
    fn new(checks_json: &str, pr_list_json: &str) -> World {
        let world = World {
            data: tempfile::tempdir().unwrap(),
            bin: tempfile::tempdir().unwrap(),
            plugin_root: tempfile::tempdir().unwrap(),
        };
        write_exec(
            &world.bin.path().join("gh"),
            &format!(
                "#!/bin/bash\n\
                 {{ for a in \"$@\"; do printf '%s\\0' \"$a\"; done; printf '\\036'; }} >> \"{}\"\n\
                 echo '{STUB_OUT}'\n\
                 echo '{STUB_ERR}' >&2\n\
                 exit 3\n",
                world.gh_log().display()
            ),
        );
        let worksources = world.plugin_root.path().join("worksources");
        std::fs::create_dir_all(&worksources).unwrap();
        write_exec(
            &worksources.join("github"),
            &format!(
                r###"#!/bin/bash
echo "$1 repo=${{LEGION_WS_REPO:-}} pr=${{LEGION_WS_PR_NUMBER:-}} number=${{LEGION_WS_NUMBER:-}} strategy=${{LEGION_WS_STRATEGY:-}} delete=${{LEGION_WS_DELETE_BRANCH:-}} body=${{LEGION_WS_BODY:-}}" >> "{log}"
case "$1" in
  pr-checks)
    cat <<'BODY'
{checks_json}
BODY
    ;;
  pr-list)
    cat <<'BODY'
{pr_list_json}
BODY
    ;;
  view-issue)
    cat <<'BODY'
{{"url":"https://github.com/owner/stub/issues/7","number":7,"title":"t","body":"## Acceptance criteria\n- [ ] it works","labels":[],"assignees":[],"state":"OPEN"}}
BODY
    ;;
  view-pr)
    cat <<'BODY'
{{"number":5,"title":"t","state":"OPEN","author":"a","createdAt":"t","updatedAt":"t","body":"b","headRefName":"feat-x","headSha":"abc","baseRefName":"main","isDraft":false,"reviewDecision":"APPROVED","mergeable":"MERGEABLE"}}
BODY
    ;;
  merge)
    echo '{{"queued":false}}'
    ;;
  comment|close)
    ;;
  *)
    echo "stub: unknown subcommand $1" >&2
    exit 2
    ;;
esac
"###,
                log = world.plugin_log().display()
            ),
        );
        std::fs::write(
            world.data.path().join("watch.toml"),
            format!(
                "poll_interval_secs = 30\ncooldown_secs = 300\n\n[[repos]]\nname = \"stub\"\n\
                 github = \"owner/stub\"\nworkdir = \"{}\"\nworksource = \"github\"\n",
                world.data.path().display()
            ),
        )
        .unwrap();
        world
    }

    fn gh_log(&self) -> PathBuf {
        self.bin.path().join("gh.log")
    }

    fn plugin_log(&self) -> PathBuf {
        self.bin.path().join("plugin.log")
    }

    /// `legion` with the stub gh first on PATH and the stub plugin root.
    fn legion(&self, data_dir: &Path) -> Command {
        let mut cmd = legion_cmd(data_dir);
        let path = format!(
            "{}:{}",
            self.bin.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        cmd.env("PATH", path)
            .env("CLAUDE_PLUGIN_ROOT", self.plugin_root.path())
            .env_remove("GH_REPO");
        cmd
    }

    fn gh(&self, args: &[&str]) -> Output {
        self.legion(self.data.path())
            .arg("gh")
            .args(args)
            .output()
            .expect("run legion gh")
    }

    /// Every stub-gh invocation's argv, in order. Empty when never invoked.
    fn gh_calls(&self) -> Vec<Vec<String>> {
        let Ok(raw) = std::fs::read(self.gh_log()) else {
            return Vec::new();
        };
        let text = String::from_utf8(raw).unwrap();
        text.split_terminator('\u{1e}')
            .map(|rec| rec.split_terminator('\0').map(str::to_owned).collect())
            .collect()
    }

    fn plugin_calls(&self) -> String {
        std::fs::read_to_string(self.plugin_log()).unwrap_or_default()
    }

    /// Audit rows for `action`, newest first.
    fn audit_rows(&self, action: &str) -> Vec<serde_json::Value> {
        let out = run_ok(
            self.legion(self.data.path())
                .args(["audit", "--action", action, "--json"]),
        );
        serde_json::from_str::<Vec<serde_json::Value>>(&out).unwrap()
    }
}

const CHECKS_PASSING: &str =
    r#"{"headSha":"abc","checks":[{"name":"Tests","state":"SUCCESS","workflow":"CI","link":"l"}]}"#;
const CHECKS_FAILING: &str = r#"{"headSha":"abc","checks":[{"name":"Clippy","state":"FAILURE","workflow":"CI","link":"l"}]}"#;

fn argv(s: &[&str]) -> Vec<String> {
    s.iter().map(|a| (*a).to_owned()).collect()
}

#[test]
fn read_runs_gh_untouched_and_writes_no_audit_row() {
    let w = World::new(CHECKS_PASSING, "[]");
    let args = ["pr", "view", "5", "--json", "a,b", "-R", "owner/stub"];
    let out = w.gh(&args);
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{STUB_OUT}\n")
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        format!("{STUB_ERR}\n")
    );
    assert_eq!(w.gh_calls(), vec![argv(&args)]);
    assert!(w.audit_rows("gh-passthrough").is_empty());
}

#[test]
fn read_needs_no_data_dir() {
    let w = World::new(CHECKS_PASSING, "[]");
    let missing = w.data.path().join("does-not-exist");
    let args = ["pr", "view", "5", "--json", "a,b", "-R", "owner/stub"];
    let out = w
        .legion(&missing)
        .arg("gh")
        .args(args)
        .output()
        .expect("run legion gh");
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{STUB_OUT}\n")
    );
    assert_eq!(w.gh_calls(), vec![argv(&args)]);
    assert!(!missing.exists(), "a read must not create the data dir");
}

#[test]
fn argv_survives_byte_for_byte() {
    let w = World::new(CHECKS_PASSING, "[]");
    let args = [
        "api",
        "repos/o/n/issues?q=a b",
        "--help",
        "-v",
        "--",
        "--json",
        "a,b",
    ];
    let out = w.gh(&args);
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(w.gh_calls(), vec![argv(&args)]);
}

#[test]
fn untranslatable_write_runs_gh_and_writes_one_audit_row() {
    let w = World::new(CHECKS_PASSING, "[]");
    let args = ["pr", "merge", "5", "-s", "--auto", "-R", "owner/stub"];
    let out = w.gh(&args);
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{STUB_OUT}\n")
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains(STUB_ERR));
    assert_eq!(w.gh_calls(), vec![argv(&args)]);

    let rows = w.audit_rows("gh-passthrough");
    assert_eq!(rows.len(), 1, "rows: {rows:?}");
    let row = &rows[0];
    assert_eq!(row["agent"], "stub");
    assert_eq!(row["target_type"], "pr");
    assert_eq!(row["target_ref"], "5");
    assert_eq!(row["outcome"], "exit 3");
    assert_eq!(row["source_type"], "github");
    let details: Vec<String> = serde_json::from_str(row["details"].as_str().unwrap()).unwrap();
    assert_eq!(details, argv(&args));
}

#[test]
fn api_post_is_audited() {
    let w = World::new(CHECKS_PASSING, "[]");
    let args = [
        "api",
        "repos/o/n/issues",
        "-f",
        "title=x",
        "-R",
        "owner/stub",
    ];
    let out = w.gh(&args);
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(w.gh_calls(), vec![argv(&args)]);
    let rows = w.audit_rows("gh-passthrough");
    assert_eq!(rows.len(), 1, "rows: {rows:?}");
    assert_eq!(rows[0]["target_type"], "api");
    assert_eq!(rows[0]["target_ref"], "repos/o/n/issues");
}

#[test]
fn merge_translates_keeping_the_branch() {
    let w = World::new(CHECKS_PASSING, "[]");
    let out = w.gh(&["pr", "merge", "5", "-s", "-R", "owner/stub"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(w.gh_calls().is_empty(), "stub gh ran: {:?}", w.gh_calls());
    let calls = w.plugin_calls();
    assert!(
        calls.contains("merge repo=owner/stub pr=5 number= strategy=squash delete=false"),
        "plugin calls: {calls}"
    );
    let merges = w.audit_rows("merge");
    assert_eq!(merges.len(), 1, "rows: {merges:?}");
    assert_eq!(merges[0]["agent"], "stub");
    assert_eq!(merges[0]["target_ref"], "5");
    assert!(w.audit_rows("gh-passthrough").is_empty());
}

#[test]
fn verb_refusal_is_final() {
    let w = World::new(CHECKS_FAILING, "[]");
    let out = w.gh(&["pr", "merge", "5", "-s", "-R", "owner/stub"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("1 check(s) failed on PR #5: Clippy"),
        "stderr: {stderr}"
    );
    assert!(w.gh_calls().is_empty(), "stub gh ran: {:?}", w.gh_calls());
    assert!(!w.plugin_calls().lines().any(|l| l.starts_with("merge ")));
}

#[test]
fn current_branch_pr_resolves() {
    let pr_list = r#"[
  {"number":11,"title":"x","createdAt":"t","updatedAt":"t","headRefName":"feat-x","reviewDecision":null,"isDraft":false},
  {"number":12,"title":"y","createdAt":"t","updatedAt":"t","headRefName":"other","reviewDecision":null,"isDraft":false}
]"#;
    let w = World::new(CHECKS_PASSING, pr_list);
    let repo = tempfile::tempdir().unwrap();
    let init = Command::new("git")
        .args(["init", "-q", "-b", "feat-x"])
        .current_dir(repo.path())
        .output()
        .unwrap();
    assert!(init.status.success());

    let out = w
        .legion(w.data.path())
        .current_dir(repo.path())
        .args(["gh", "pr", "comment", "-b", "hi", "-R", "owner/stub"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(w.gh_calls().is_empty(), "stub gh ran: {:?}", w.gh_calls());
    let calls = w.plugin_calls();
    assert!(
        calls.contains("comment repo=owner/stub pr= number=11 strategy= delete= body=hi"),
        "plugin calls: {calls}"
    );
}

#[test]
fn gated_passthrough_is_refused() {
    let w = World::new(CHECKS_FAILING, "[]");
    let out = w.gh(&["pr", "merge", "5", "-s", "--auto", "-R", "owner/stub"]);
    assert!(!out.status.success());
    let gh_stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(w.gh_calls().is_empty(), "stub gh ran: {:?}", w.gh_calls());

    // The refusal is `legion pr merge`'s own, word for word.
    let (_stdout, verb_stderr) = run_fail(
        w.legion(w.data.path())
            .args(["pr", "merge", "--repo", "stub", "--number", "5"]),
    );
    assert_eq!(gh_stderr, verb_stderr);
    assert!(gh_stderr.contains("check(s) failed on PR #5"));

    let out = w.gh(&[
        "issue",
        "close",
        "7",
        "-r",
        "not planned",
        "-R",
        "owner/stub",
    ]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no verify verdict exists"),
        "stderr: {stderr}"
    );
    assert!(w.gh_calls().is_empty(), "stub gh ran: {:?}", w.gh_calls());
    assert!(w.audit_rows("gh-passthrough").is_empty());
}

#[test]
fn unmatched_repo_passes_through() {
    let w = World::new(CHECKS_PASSING, "[]");
    let args = ["issue", "close", "7", "-R", "other/repo"];
    let out = w.gh(&args);
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(w.gh_calls(), vec![argv(&args)]);
    let rows = w.audit_rows("gh-passthrough");
    assert_eq!(rows.len(), 1, "rows: {rows:?}");
    assert_eq!(rows[0]["agent"], "other/repo");
    assert_eq!(rows[0]["target_ref"], "7");
}

#[test]
fn gh_target_matches_watch_toml_case_insensitively() {
    let w = World::new(CHECKS_PASSING, "[]");
    let out = w.gh(&["pr", "merge", "5", "-s", "-R", "OWNER/Stub"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(w.gh_calls().is_empty(), "stub gh ran: {:?}", w.gh_calls());
    assert_eq!(w.audit_rows("merge").len(), 1);
}

#[test]
fn entry_without_workdir_is_no_match() {
    let w = World::new(CHECKS_PASSING, "[]");
    let watch = w.data.path().join("watch.toml");
    let mut text = std::fs::read_to_string(&watch).unwrap();
    text.push_str("\n[[repos]]\nname = \"half\"\ngithub = \"owner/half\"\n");
    std::fs::write(&watch, text).unwrap();

    let args = ["issue", "close", "7", "-R", "owner/half"];
    let out = w.gh(&args);
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(w.gh_calls(), vec![argv(&args)]);
    let rows = w.audit_rows("gh-passthrough");
    assert_eq!(rows.len(), 1, "rows: {rows:?}");
    assert_eq!(rows[0]["agent"], "owner/half");
}
