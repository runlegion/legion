//! Integration tests: plugin-surface governance, verify gate, autonomy budget, board goal, burn-rate gate.

use crate::common::*;

// #530: grep-enforcement parity lock.
//
// legion's own command/skill surfaces must never grant Grep or Glob. The
// recall/sym-before-grep doctrine is enforced for the main session by the
// PreToolUse hooks (pre-grep, pre-bash-grep, pre-read-sym), but those
// hooks do NOT fire while a slash command's or
// skill's own tool allowlist is active, and they do NOT fire on subagents.
// The allowlist (`allowed-tools`) is the only enforcement on those surfaces
// and is strictly stronger than `disallowed-tools` (whitelist vs blacklist).
//
// This test fails if anyone re-grants Grep/Glob to a legion surface, so a
// future "let me just grep here" edit has to be deliberate -- it can't slip
// in silently and weaken the doctrine. See #530 and
// docs/decisions/2026-05-grep-enforcement-stays-in-hooks.md.
#[test]
fn legion_command_and_skill_surfaces_never_grant_grep() {
    let plugin = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin");

    // Collect every frontmatter-bearing surface: commands/*.md + skills/*/SKILL.md.
    let mut surfaces: Vec<std::path::PathBuf> = Vec::new();
    let commands = plugin.join("commands");
    for entry in std::fs::read_dir(&commands).expect("plugin/commands must exist") {
        let path = entry.expect("readable commands entry").path();
        if path.extension().and_then(|e| e.to_str()) == Some("md") {
            surfaces.push(path);
        }
    }
    let skills = plugin.join("skills");
    for entry in std::fs::read_dir(&skills).expect("plugin/skills must exist") {
        let skill_md = entry
            .expect("readable skills entry")
            .path()
            .join("SKILL.md");
        if skill_md.is_file() {
            surfaces.push(skill_md);
        }
    }
    assert!(
        !surfaces.is_empty(),
        "expected to find legion command/skill surfaces under plugin/"
    );

    for path in surfaces {
        let body = std::fs::read_to_string(&path).expect("readable surface");
        let tools = parse_allowed_tools(&body);
        let Some(tools) = tools else {
            // A surface with no allowlist inherits the full toolset and is
            // governed by the main-session hooks instead -- not this test's
            // concern. (All legion surfaces currently declare one.)
            continue;
        };
        for forbidden in ["Grep", "Glob"] {
            assert!(
                !tools.iter().any(|t| t == forbidden),
                "{} grants {} in its allowed-tools -- legion surfaces must use sym/recall, \
                 not grep (see #530). If a scan is genuinely needed, route it through Bash \
                 where pre-bash-grep applies the sym ladder.",
                path.display(),
                forbidden
            );
        }
    }
}

// Extract the tool identifiers granted by an `allowed-tools:` frontmatter
// key, handling both forms legion surfaces use:
//   - inline:     `allowed-tools: Bash, Read`  or  `allowed-tools: ["Bash"]`
//   - block list: `allowed-tools:` then indented `  - Bash` lines
// Returns None when the key is absent (no allowlist declared). Tokens are
// split on the structural delimiters so a coincidental substring cannot
// match -- only a standalone tool identifier does.
fn parse_allowed_tools(body: &str) -> Option<Vec<String>> {
    let lines: Vec<&str> = body.lines().collect();
    let idx = lines
        .iter()
        .position(|l| l.trim_start().starts_with("allowed-tools:"))?;

    let split_tokens = |s: &str| -> Vec<String> {
        s.split([',', '[', ']', '"', '\'', '-'])
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
            .map(|t| t.to_string())
            .collect()
    };

    // Inline value after the colon (empty for the block-list form).
    let inline = lines[idx].split_once(':').map(|(_, v)| v).unwrap_or("");
    let mut tools = split_tokens(inline);

    // Block-list form: indented `- Tool` continuation lines follow the key
    // until the next frontmatter key or the closing `---`.
    if tools.is_empty() {
        for line in &lines[idx + 1..] {
            let trimmed = line.trim_start();
            if trimmed.starts_with("- ") {
                tools.extend(split_tokens(trimmed));
            } else if line.starts_with(|c: char| !c.is_whitespace()) {
                break; // dedent to a new key (or `---`) ends the list
            }
        }
    }

    Some(tools)
}

#[test]
fn parse_allowed_tools_handles_inline_and_block_forms() {
    // Inline comma form (skills).
    assert_eq!(
        parse_allowed_tools("---\nallowed-tools: Bash, Read\n---\n"),
        Some(vec!["Bash".to_string(), "Read".to_string()])
    );
    // Inline JSON-array form (commands).
    assert_eq!(
        parse_allowed_tools("---\nallowed-tools: [\"Bash\"]\n---\n"),
        Some(vec!["Bash".to_string()])
    );
    // Block-list form -- the blind spot the inline-only parser would miss.
    let block = "---\nname: x\nallowed-tools:\n  - Bash\n  - Grep\nversion: 1\n---\n";
    let tools = parse_allowed_tools(block).expect("block form parsed");
    assert!(
        tools.iter().any(|t| t == "Grep"),
        "block-list Grep must be detected, got {tools:?}"
    );
    // Absent key.
    assert_eq!(parse_allowed_tools("---\nname: x\n---\n"), None);
}

// #520 verify gate, end-to-end at the CLI boundary: a card with acceptance
// criteria cannot reach Done until `legion verify` records a clean verdict.
#[test]
fn verify_gate_blocks_done_until_clean_then_allows() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();

    // Create a card carrying two acceptance criteria (parsed from --context).
    let card = run_ok(legion_cmd(data).args([
        "kanban",
        "create",
        "--from",
        "kelex",
        "--to",
        "kelex",
        "--text",
        "ship the thing",
        "--context",
        "## Acceptance criteria\n- crit one\n- crit two\n",
    ]))
    .trim()
    .to_string();

    // Promote to a Done-eligible state: Backlog -> Pending -> Accepted.
    run_ok(legion_cmd(data).args(["kanban", "assign", "--id", &card, "--to", "kelex"]));
    run_ok(legion_cmd(data).args(["kanban", "accept", "--id", &card]));

    // Done must be refused: the card has criteria but no verify verdict.
    let (_stdout, stderr) = run_fail(
        legion_cmd(data).args(["done", "--repo", "kelex", "--text", "done", "--id", &card]),
    );
    assert!(
        stderr.contains("verify verdict"),
        "expected a verify-gate error, got: {stderr}"
    );

    // Record a clean verify verdict (both criteria pass with evidence).
    let verdicts = data.join("verdicts.json");
    std::fs::write(
        &verdicts,
        r#"[
          {"criterion":"crit one","verdict":"pass","evidence":"tests::crit_one"},
          {"criterion":"crit two","verdict":"pass","evidence":"src/x.rs:10 behavior"}
        ]"#,
    )
    .unwrap();
    run_ok(legion_cmd(data).args([
        "verify",
        "--repo",
        "kelex",
        "--card",
        &card,
        "--verdicts-file",
        verdicts.to_str().unwrap(),
    ]));

    // Done now succeeds.
    run_ok(legion_cmd(data).args(["done", "--repo", "kelex", "--text", "done", "--id", &card]));
}

// #520: a Fail verdict keeps the card blocked from Done.
#[test]
fn verify_gate_fail_keeps_done_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();

    let card = run_ok(legion_cmd(data).args([
        "kanban",
        "create",
        "--from",
        "kelex",
        "--to",
        "kelex",
        "--text",
        "wip",
        "--context",
        "## Acceptance criteria\n- only crit\n",
    ]))
    .trim()
    .to_string();
    run_ok(legion_cmd(data).args(["kanban", "assign", "--id", &card, "--to", "kelex"]));
    run_ok(legion_cmd(data).args(["kanban", "accept", "--id", &card]));

    let verdicts = data.join("v.json");
    std::fs::write(
        &verdicts,
        r#"[{"criterion":"only crit","verdict":"fail","evidence":"not implemented"}]"#,
    )
    .unwrap();
    run_fail(legion_cmd(data).args([
        "verify",
        "--repo",
        "kelex",
        "--card",
        &card,
        "--verdicts-file",
        verdicts.to_str().unwrap(),
    ]));

    run_fail(legion_cmd(data).args(["done", "--repo", "kelex", "--text", "done", "--id", &card]));
}

// #524 autonomy budget, end-to-end at the CLI: operator work bypasses, a spend
// accumulates, and exhaustion stops self-directed work cleanly (non-zero exit,
// no error). A fresh DB has no rate sample, so the ceiling is the conservative
// default (15 units).
#[test]
fn autonomy_budget_gates_self_directed_work_not_operator_work() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();

    let status = || run_ok(legion_cmd(data).args(["autonomy", "status", "--repo", "kelex"]));

    // Fresh: nothing spent against the default ceiling.
    assert!(status().contains("0/15"), "got: {}", status());

    // Operator-requested work bypasses and spends nothing.
    let op_stdout = run_ok(legion_cmd(data).args([
        "autonomy",
        "gate",
        "--repo",
        "kelex",
        "--kind",
        "self-accept",
        "--operator",
    ]));
    assert!(op_stdout.contains("not budget-gated"), "got: {op_stdout}");
    assert!(status().contains("0/15"), "operator work must not spend");

    // A self-directed spend of the whole ceiling is allowed and accumulates.
    run_ok(legion_cmd(data).args([
        "autonomy",
        "gate",
        "--repo",
        "kelex",
        "--kind",
        "self-accept",
        "--cost",
        "15",
    ]));
    assert!(status().contains("15/15"), "got: {}", status());

    // The next self-directed spend is refused -- cleanly (non-zero, no panic).
    let (stdout, stderr) = run_fail(legion_cmd(data).args([
        "autonomy",
        "gate",
        "--repo",
        "kelex",
        "--kind",
        "free-time",
    ]));
    assert!(
        stdout.contains("exhausted"),
        "got stdout: {stdout} stderr: {stderr}"
    );

    // ...but operator work still proceeds even when exhausted.
    run_ok(legion_cmd(data).args([
        "autonomy",
        "gate",
        "--repo",
        "kelex",
        "--kind",
        "self-accept",
        "--operator",
    ]));
}

// #524/#546: the --banner reminder. Available budget tells the agent
// self-directed work is sanctioned; a spent budget tells it to pause.
#[test]
fn autonomy_status_banner_reminds_to_spend_or_pause() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();

    let banner =
        || run_ok(legion_cmd(data).args(["autonomy", "status", "--repo", "kelex", "--banner"]));

    // Fresh budget -> sanctioned-to-spend framing.
    let fresh = banner();
    assert!(fresh.contains("sanctioned"), "got: {fresh}");
    assert!(fresh.contains("without asking"), "got: {fresh}");

    // Spend the ceiling, then the banner flips to the paused framing.
    run_ok(legion_cmd(data).args([
        "autonomy",
        "gate",
        "--repo",
        "kelex",
        "--kind",
        "self-accept",
        "--cost",
        "15",
    ]));
    let spent = banner();
    assert!(spent.contains("spent for this week"), "got: {spent}");
    assert!(
        spent.contains("operator-requested work still proceeds"),
        "got: {spent}"
    );
}

// #525 board-derived goal: set from the Accepted card's AC, cleared once the
// card leaves Accepted (terminal/blocked). No separate goal state -- the board
// is the source of truth, re-derived on each `legion goal` call.
#[test]
fn board_goal_sets_on_accept_and_clears_off_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();

    let goal = || run_ok(legion_cmd(data).args(["goal", "--repo", "kelex"]));

    let card = run_ok(legion_cmd(data).args([
        "kanban",
        "create",
        "--from",
        "kelex",
        "--to",
        "kelex",
        "--text",
        "ship the goal",
        "--context",
        "## Acceptance criteria\n- crit alpha\n- crit beta\n",
    ]))
    .trim()
    .to_string();

    // Pending (assigned, not accepted) -> no goal yet.
    run_ok(legion_cmd(data).args(["kanban", "assign", "--id", &card, "--to", "kelex"]));
    assert!(
        goal().trim().is_empty(),
        "no goal before the card is accepted"
    );

    // Accepted -> the card's AC becomes the goal.
    run_ok(legion_cmd(data).args(["kanban", "accept", "--id", &card]));
    let active = goal();
    assert!(active.contains("ship the goal"), "got: {active}");
    assert!(
        active.contains("crit alpha") && active.contains("crit beta"),
        "got: {active}"
    );

    // Blocked (off Accepted) -> goal clears, by board state alone.
    run_ok(legion_cmd(data).args([
        "kanban",
        "block",
        "--id",
        &card,
        "--reason",
        "waiting on upstream",
    ]));
    assert!(
        goal().trim().is_empty(),
        "goal clears once the card leaves Accepted"
    );
}

// #551 burn-rate gate: self-directed work is paused when the latest rate-limit
// sample exceeds the threshold. The gate fires on the 5h OR 7d window.

// Missing sample (fresh DB) does not fire: fail-open so the work-unit budget
// remains the sole governor when no rate data exists.
#[test]
fn burn_rate_gate_no_sample_is_fail_open() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();

    // No statusline seed -> no rate_limit_samples row for this host.
    run_ok(legion_cmd(data).args([
        "autonomy",
        "gate",
        "--repo",
        "kelex",
        "--kind",
        "self-accept",
    ]));
}

// Seeded sample above threshold -> gate denies self-directed work (non-zero exit,
// calm message, no panic).
#[test]
fn burn_rate_gate_throttled_when_sample_exceeds_threshold() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();

    // Seed a sample with 5h at 95% (above the default 90% threshold).
    seed_rate_limit_sample(data, 95.0, 50.0);

    let (stdout, stderr) = run_fail(legion_cmd(data).args([
        "autonomy",
        "gate",
        "--repo",
        "kelex",
        "--kind",
        "self-accept",
    ]));
    assert!(
        stdout.contains("paused"),
        "throttled message must say paused, got: {stdout}"
    );
    assert!(
        stdout.contains("5h"),
        "throttled message must name the triggering window, got: {stdout}"
    );
    // No panic, no Rust backtrace in stderr.
    assert!(
        !stderr.contains("thread 'main' panicked"),
        "gate must not panic, got stderr: {stderr}"
    );
}

// --operator bypasses the burn-rate gate even when the sample is above threshold.
#[test]
fn burn_rate_gate_operator_bypasses_throttle() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();

    // Seed a sample that would throttle self-directed work.
    seed_rate_limit_sample(data, 95.0, 95.0);

    run_ok(legion_cmd(data).args([
        "autonomy",
        "gate",
        "--repo",
        "kelex",
        "--kind",
        "self-accept",
        "--operator",
    ]));
}

// The 7d window alone triggers the gate.
#[test]
fn burn_rate_gate_fires_on_seven_day_window() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();

    // 5h is fine, 7d is above threshold.
    seed_rate_limit_sample(data, 40.0, 92.0);

    let (stdout, _stderr) = run_fail(legion_cmd(data).args([
        "autonomy",
        "gate",
        "--repo",
        "kelex",
        "--kind",
        "self-accept",
    ]));
    assert!(
        stdout.contains("7d"),
        "throttled message must name the 7d window, got: {stdout}"
    );
}

// autonomy status includes burn-rate line when a sample exists.
#[test]
fn autonomy_status_includes_burn_rate_line_when_sample_exists() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();

    seed_rate_limit_sample(data, 40.0, 55.0);

    let stdout = run_ok(legion_cmd(data).args(["autonomy", "status", "--repo", "kelex"]));
    // Should have a rate headroom line.
    assert!(
        stdout.contains("Rate headroom:") || stdout.contains("Rate limit warning:"),
        "status must include burn-rate line when a sample exists, got: {stdout}"
    );
    assert!(
        stdout.contains("remaining"),
        "burn-rate line must describe remaining headroom, got: {stdout}"
    );
}
