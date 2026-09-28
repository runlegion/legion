//! The shipped artifact (`plugin/legion-cmd/policy.json`, #1337): exactly
//! the four Bash lists and the rules for tools other than Bash, with today's
//! never-run and ask coverage and the three power switches.

use legion_cmd::{PolicyError, builtin_no_go, parse_policy};
use serde_json::Value;

const SHIPPED_POLICY_JSON: &str = include_str!("../../../plugin/legion-cmd/policy.json");

fn shipped() -> legion_cmd::Policy {
    parse_policy(SHIPPED_POLICY_JSON).expect("the shipped policy parses")
}

#[test]
fn the_shipped_file_has_exactly_the_four_lists_and_tools() {
    let root: Value = serde_json::from_str(SHIPPED_POLICY_JSON).expect("valid JSON");
    let mut keys: Vec<&str> = root
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec!["ask", "never_run", "power_switches", "proxy", "tools"]
    );
    let tools = root["tools"].as_object().expect("tools is an object");
    for bash_only in ["Bash", "Grep", "Glob"] {
        assert!(!tools.contains_key(bash_only), "tools carries {bash_only}");
    }
    // The Read size refusal is gone (#1338): a Read runs whatever its size.
    assert!(!tools.contains_key("Read"), "tools carries Read");
}

#[test]
fn the_proxy_list_is_git_gh_grep_and_rg() {
    assert_eq!(shipped().proxy, vec!["git", "gh", "grep", "rg"]);
}

#[test]
fn todays_never_run_and_ask_coverage_is_kept() {
    let policy = shipped();
    let never_run: Vec<String> = policy
        .never_run_entries()
        .into_iter()
        .map(|e| e.id)
        .collect();
    for id in [
        "rm-recursive-force-root",
        "mkfs-device",
        "dd-to-disk",
        "fork-bomb",
        "chmod-chown-recursive-root",
        "git-force-push-main",
        "git-force-refspec-main",
        "sqlite3-legion-db",
        "rm-recursive-force",
    ] {
        assert!(never_run.contains(&id.to_string()), "never-run lost {id}");
    }
    assert_eq!(builtin_no_go().len(), 7);
    let ask: Vec<&str> = policy.ask.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ask, vec!["curl-network"]);
    let switches: Vec<&str> = policy
        .power_switches
        .iter()
        .map(|e| e.id.as_str())
        .collect();
    assert_eq!(
        switches,
        vec![
            "push-force",
            "push-force-refspec",
            "merge-despite-failures",
            "issue-close-force"
        ]
    );
    for entry in policy
        .never_run
        .iter()
        .chain(&policy.ask)
        .chain(&policy.power_switches)
    {
        assert!(!entry.reason.is_empty(), "{} carries no reason", entry.id);
    }
}

#[test]
fn a_file_with_any_other_top_level_key_fails_to_parse() {
    let mut root: Value = serde_json::from_str(SHIPPED_POLICY_JSON).expect("valid JSON");
    for extra in ["route", "sym_jobs", "wrappers", "no_go", "anything"] {
        let mut with_extra = root.clone();
        with_extra
            .as_object_mut()
            .expect("an object")
            .insert(extra.to_string(), serde_json::json!({}));
        let err = parse_policy(&with_extra.to_string()).expect_err("an extra key fails");
        assert_eq!(
            err,
            PolicyError::UnknownField {
                pointer: format!("/{extra}"),
                field: extra.to_string(),
            }
        );
    }
    // Removing a list is fine: each is optional.
    root.as_object_mut().expect("an object").remove("ask");
    parse_policy(&root.to_string()).expect("a missing list parses");
}
