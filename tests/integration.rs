use std::process::Command;

fn legion_cmd(data_dir: &std::path::Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_legion"));
    cmd.env("LEGION_DATA_DIR", data_dir);
    cmd
}

#[test]
fn reflect_and_recall_roundtrip() {
    let dir = tempfile::tempdir().unwrap();

    let output = legion_cmd(dir.path())
        .args([
            "reflect",
            "--repo",
            "test",
            "--text",
            "arrays are tricky in codegen",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "reflect failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let output = legion_cmd(dir.path())
        .args(["recall", "--repo", "test", "--context", "codegen arrays"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "recall failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("arrays are tricky"),
        "expected reflection in output, got: {stdout}"
    );
}

/// Validate that a string looks like a UUIDv7 (36 chars, 4 hyphens).
fn assert_uuid_format(s: &str) {
    let trimmed = s.trim();
    assert!(
        trimmed.len() == 36 && trimmed.chars().filter(|c| *c == '-').count() == 4,
        "expected UUIDv7 format, got: {trimmed}"
    );
}

#[test]
fn quiet_by_default() {
    let dir = tempfile::tempdir().unwrap();

    // Reflect without --verbose should produce no stderr
    let out = legion_cmd(dir.path())
        .args(["reflect", "--repo", "test", "--text", "quiet test"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.is_empty(),
        "expected no stderr without --verbose, got: {stderr}"
    );
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_uuid_format(&id);

    // Post without --verbose should also be quiet
    let out = legion_cmd(dir.path())
        .args(["post", "--repo", "test", "--text", "quiet post"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.is_empty(),
        "expected no stderr without --verbose, got: {stderr}"
    );
    assert_uuid_format(&String::from_utf8_lossy(&out.stdout));
}

#[test]
fn verbose_shows_confirmation() {
    let dir = tempfile::tempdir().unwrap();

    // Reflect with --verbose should produce confirmation on stderr
    let out = legion_cmd(dir.path())
        .args([
            "--verbose",
            "reflect",
            "--repo",
            "test",
            "--text",
            "verbose test",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("storing reflection"),
        "expected verbose confirmation, got: {stderr}"
    );
    assert_uuid_format(&String::from_utf8_lossy(&out.stdout));

    // Post with --verbose
    let out = legion_cmd(dir.path())
        .args([
            "--verbose",
            "post",
            "--repo",
            "test",
            "--text",
            "verbose post",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("posting"),
        "expected verbose confirmation, got: {stderr}"
    );
}

#[test]
fn stats_on_empty_db() {
    let dir = tempfile::tempdir().unwrap();

    let output = legion_cmd(dir.path()).args(["stats"]).output().unwrap();
    assert!(
        output.status.success(),
        "stats failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("no reflections"),
        "expected empty message, got: {stdout}"
    );
}

#[test]
fn reflect_no_input_errors() {
    let dir = tempfile::tempdir().unwrap();

    let output = legion_cmd(dir.path())
        .args(["reflect", "--repo", "test"])
        .output()
        .unwrap();
    // clap allows the call but the binary returns an error since
    // neither --text nor --transcript is provided.
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("NoReflectionInput"),
        "expected missing input error, got: {stderr}"
    );
}

#[test]
fn stats_after_reflections() {
    let dir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path())
        .args(["reflect", "--repo", "kelex", "--text", "first reflection"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "reflect 1 failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = legion_cmd(dir.path())
        .args(["reflect", "--repo", "kelex", "--text", "second reflection"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "reflect 2 failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = legion_cmd(dir.path())
        .args([
            "reflect",
            "--repo",
            "rafters",
            "--text",
            "rafters reflection",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "reflect 3 failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let output = legion_cmd(dir.path()).args(["stats"]).output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("kelex"), "should show kelex stats");
    assert!(stdout.contains("rafters"), "should show rafters stats");
}

#[test]
fn recall_with_no_matches() {
    let dir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path())
        .args([
            "reflect",
            "--repo",
            "test",
            "--text",
            "rust ownership rules",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "reflect failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let output = legion_cmd(dir.path())
        .args([
            "recall",
            "--repo",
            "other-repo",
            "--context",
            "rust ownership",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    // Should succeed but return empty since repo does not match
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        !stdout.contains("rust ownership"),
        "should not find results in different repo"
    );
}

#[test]
fn data_dir_is_created_automatically() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("deep").join("nested").join("dir");

    let output = legion_cmd(&nested).args(["stats"]).output().unwrap();
    assert!(
        output.status.success(),
        "should create nested dirs: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(nested.exists(), "data dir should have been created");
}

#[test]
fn consult_across_repos() {
    let dir = tempfile::tempdir().unwrap();

    // Reflect into two different repos
    let out = legion_cmd(dir.path())
        .args([
            "reflect",
            "--repo",
            "kelex",
            "--text",
            "Zod schema mapping is fragile",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "reflect kelex failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = legion_cmd(dir.path())
        .args([
            "reflect",
            "--repo",
            "platform",
            "--text",
            "Zod validation at the edge works well",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "reflect platform failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Consult across all repos
    let output = legion_cmd(dir.path())
        .args(["consult", "--context", "Zod", "--limit", "10"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "consult failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("[kelex]"),
        "expected [kelex] in output, got: {stdout}"
    );
    assert!(
        stdout.contains("[platform]"),
        "expected [platform] in output, got: {stdout}"
    );
    assert!(
        stdout.contains("Cross-repo reflections"),
        "expected header in output, got: {stdout}"
    );
}

#[test]
fn cli_compound_repo_flag() {
    let dir = tempfile::tempdir().unwrap();

    let output = legion_cmd(dir.path())
        .args([
            "reflect",
            "--repo",
            "platform,legion",
            "--text",
            "compound test reflection",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "compound reflect failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let ids: Vec<&str> = stdout.lines().collect();
    assert_eq!(ids.len(), 2, "expected 2 IDs on stdout, got: {stdout}");

    // Verify both repos have the reflection via recall
    let output = legion_cmd(dir.path())
        .args(["recall", "--repo", "platform", "--context", "compound test"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("compound test reflection"),
        "expected reflection in platform recall, got: {stdout}"
    );

    let output = legion_cmd(dir.path())
        .args(["recall", "--repo", "legion", "--context", "compound test"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("compound test reflection"),
        "expected reflection in legion recall, got: {stdout}"
    );
}

#[test]
fn cli_single_repo_still_works() {
    let dir = tempfile::tempdir().unwrap();

    let output = legion_cmd(dir.path())
        .args([
            "reflect",
            "--repo",
            "platform",
            "--text",
            "single repo test",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "single repo reflect failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.trim().is_empty(),
        "expected ID on stdout, got nothing"
    );
}

#[test]
fn post_and_bullpen_roundtrip() {
    let dir = tempfile::tempdir().unwrap();

    // Post a message
    let out = legion_cmd(dir.path())
        .args([
            "post",
            "--repo",
            "kelex",
            "--text",
            "shared insight about schema parsing",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "post failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.trim().is_empty(),
        "expected ID on stdout, got nothing"
    );

    // Read the bullpen from a different repo
    let output = legion_cmd(dir.path())
        .args(["bullpen", "--repo", "rafters"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "bullpen failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("[kelex]"),
        "expected repo attribution, got: {stdout}"
    );
    assert!(
        stdout.contains("shared insight about schema parsing"),
        "expected post text, got: {stdout}"
    );
    assert!(
        stdout.contains("[Legion] Bullpen"),
        "expected bullpen header, got: {stdout}"
    );
}

#[test]
fn bullpen_count_output() {
    let dir = tempfile::tempdir().unwrap();

    // Post two messages
    let out = legion_cmd(dir.path())
        .args(["post", "--repo", "kelex", "--text", "first shared thought"])
        .output()
        .unwrap();
    assert!(out.status.success());

    let out = legion_cmd(dir.path())
        .args([
            "post",
            "--repo",
            "rafters",
            "--text",
            "second shared thought",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());

    // Check count from a reader that has not read the bullpen
    let output = legion_cmd(dir.path())
        .args(["bullpen", "--repo", "platform", "--count"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "bullpen count failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("2 unread posts on the bullpen"),
        "expected unread count, got: {stdout}"
    );

    // Read the bullpen to mark as read
    let out = legion_cmd(dir.path())
        .args(["bullpen", "--repo", "platform"])
        .output()
        .unwrap();
    assert!(out.status.success());

    // Count should now be zero (no output)
    let output = legion_cmd(dir.path())
        .args(["bullpen", "--repo", "platform", "--count"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.is_empty(),
        "expected no output for zero unread, got: {stdout}"
    );
}

#[test]
fn bullpen_count_includes_pending_tasks() {
    let dir = tempfile::tempdir().unwrap();

    // Post to bullpen
    let out = legion_cmd(dir.path())
        .args(["post", "--repo", "kelex", "--text", "a shared thought"])
        .output()
        .unwrap();
    assert!(out.status.success());

    // Create a pending task for the reader
    let out = legion_cmd(dir.path())
        .args([
            "task",
            "create",
            "--from",
            "kelex",
            "--to",
            "platform",
            "--text",
            "urgent work",
            "--priority",
            "high",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());

    // Count should show both posts and tasks
    let output = legion_cmd(dir.path())
        .args(["bullpen", "--repo", "platform", "--count"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("1 unread posts, 1 pending tasks on the bullpen"),
        "expected combined count, got: {stdout}"
    );
}

#[test]
fn reindex_rebuilds_from_database() {
    let dir = tempfile::tempdir().unwrap();

    // Create some reflections
    let out = legion_cmd(dir.path())
        .args([
            "reflect",
            "--repo",
            "test",
            "--text",
            "reindex test reflection about search",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "reflect failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Run reindex
    let output = legion_cmd(dir.path())
        .args(["--verbose", "reindex"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "reindex failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("reindexed 1 reflections"),
        "expected reindex count, got: {stderr}"
    );

    // Verify search still works after reindex
    let output = legion_cmd(dir.path())
        .args(["recall", "--repo", "test", "--context", "search"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("reindex test reflection"),
        "expected reflection after reindex, got: {stdout}"
    );
}

#[test]
fn consult_no_matches() {
    let dir = tempfile::tempdir().unwrap();

    // Reflect something so the DB/index exist
    let out = legion_cmd(dir.path())
        .args([
            "reflect",
            "--repo",
            "test",
            "--text",
            "rust ownership rules",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "reflect failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Consult with a term that will not match
    let output = legion_cmd(dir.path())
        .args(["--verbose", "consult", "--context", "nonexistent_term_xyz"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "consult should succeed even with no matches: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("no reflections matched"),
        "expected no-match message on stderr, got: {stderr}"
    );
}

#[test]
fn reflect_with_metadata_flags() {
    let dir = tempfile::tempdir().unwrap();

    let output = legion_cmd(dir.path())
        .args([
            "reflect",
            "--repo",
            "kelex",
            "--text",
            "oklch color tokens work well",
            "--domain",
            "color-tokens",
            "--tags",
            "semantic-tokens,consumer",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "reflect with meta failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.trim().is_empty(),
        "expected ID on stdout, got nothing"
    );
}

#[test]
fn boost_and_chain_roundtrip() {
    let dir = tempfile::tempdir().unwrap();

    // Create a reflection
    let out = legion_cmd(dir.path())
        .args([
            "reflect",
            "--repo",
            "kelex",
            "--text",
            "first insight in a chain",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();

    // Boost the reflection
    let output = legion_cmd(dir.path())
        .args(["--verbose", "boost", "--id", &id])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "boost failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("boosted reflection"),
        "expected boost confirmation, got: {stderr}"
    );

    // Chain with a single node
    let output = legion_cmd(dir.path())
        .args(["chain", "--id", &id])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "chain failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("first insight"),
        "expected chain output, got: {stdout}"
    );
}

#[test]
fn chain_with_follows() {
    let dir = tempfile::tempdir().unwrap();

    // Create parent reflection
    let out = legion_cmd(dir.path())
        .args(["reflect", "--repo", "kelex", "--text", "root of the chain"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let parent_id = String::from_utf8_lossy(&out.stdout).trim().to_string();

    // Create child reflection with --follows
    let out = legion_cmd(dir.path())
        .args([
            "reflect",
            "--repo",
            "kelex",
            "--text",
            "builds on root",
            "--follows",
            &parent_id,
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "child reflect failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let child_id = String::from_utf8_lossy(&out.stdout).trim().to_string();

    // Chain from child should show both
    let output = legion_cmd(dir.path())
        .args(["chain", "--id", &child_id])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("root of the chain"),
        "expected parent in chain, got: {stdout}"
    );
    assert!(
        stdout.contains("builds on root"),
        "expected child in chain, got: {stdout}"
    );
}

#[test]
fn boost_nonexistent_id() {
    let dir = tempfile::tempdir().unwrap();

    // Need to create the DB first
    let out = legion_cmd(dir.path())
        .args(["reflect", "--repo", "test", "--text", "setup"])
        .output()
        .unwrap();
    assert!(out.status.success());

    let output = legion_cmd(dir.path())
        .args(["boost", "--id", "nonexistent-uuid"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "boost should succeed even for missing ID: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("reflection not found"),
        "expected not-found message, got: {stderr}"
    );
}

#[test]
fn post_with_metadata_flags() {
    let dir = tempfile::tempdir().unwrap();

    let output = legion_cmd(dir.path())
        .args([
            "post",
            "--repo",
            "rafters",
            "--text",
            "shared domain knowledge",
            "--domain",
            "auth",
            "--tags",
            "security,jwt",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "post with meta failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.trim().is_empty(),
        "expected ID on stdout, got nothing"
    );

    // Verify it shows up on the bullpen
    let output = legion_cmd(dir.path())
        .args(["bullpen", "--repo", "kelex"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("shared domain knowledge"),
        "expected post on bullpen, got: {stdout}"
    );
}

#[test]
fn surface_shows_recent_posts() {
    let dir = tempfile::tempdir().unwrap();

    // Post to the bullpen
    let out = legion_cmd(dir.path())
        .args(["post", "--repo", "rafters", "--text", "synapse insight"])
        .output()
        .unwrap();
    assert!(out.status.success());

    // Surface for a different repo should show the post
    let output = legion_cmd(dir.path())
        .args(["surface", "--repo", "kelex"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "surface failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("[Synapse]"),
        "expected synapse header, got: {stdout}"
    );
    assert!(
        stdout.contains("synapse insight"),
        "expected post in surface output, got: {stdout}"
    );
}

#[test]
fn surface_empty_database() {
    let dir = tempfile::tempdir().unwrap();

    // Need to initialize the DB first
    let out = legion_cmd(dir.path())
        .args(["reflect", "--repo", "test", "--text", "setup"])
        .output()
        .unwrap();
    assert!(out.status.success());

    let output = legion_cmd(dir.path())
        .args(["surface", "--repo", "kelex"])
        .output()
        .unwrap();
    assert!(output.status.success());
    // No bullpen posts, no high-value, no chains -- should be empty
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.is_empty(),
        "expected empty surface for no highlights, got: {stdout}"
    );
}

#[test]
fn bullpen_aliases_backward_compatible() {
    let dir = tempfile::tempdir().unwrap();

    // Seed a post
    let out = legion_cmd(dir.path())
        .args(["post", "--repo", "kelex", "--text", "alias test"])
        .output()
        .unwrap();
    assert!(out.status.success());

    // Old "board" alias still works
    let output = legion_cmd(dir.path())
        .args(["board", "--repo", "rafters"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "board alias should still work: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Short "bp" alias works
    let output = legion_cmd(dir.path())
        .args(["bp", "--repo", "rafters"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "bp alias should work: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn signal_command_posts_formatted_signal() {
    let dir = tempfile::tempdir().unwrap();

    let output = legion_cmd(dir.path())
        .args([
            "signal", "--repo", "kelex", "--to", "legion", "--verb", "review", "--status",
            "approved", "--note", "ship it",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "signal failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify signal appears on the bullpen
    let output = legion_cmd(dir.path())
        .args(["bullpen", "--repo", "platform"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("@legion"),
        "expected signal recipient on bullpen, got: {stdout}"
    );
    assert!(
        stdout.contains("review"),
        "expected signal verb on bullpen, got: {stdout}"
    );
}

#[test]
fn signal_with_details() {
    let dir = tempfile::tempdir().unwrap();

    let output = legion_cmd(dir.path())
        .args([
            "signal",
            "--repo",
            "kelex",
            "--to",
            "legion",
            "--verb",
            "review",
            "--status",
            "approved",
            "--details",
            "surface:cap-output,chain:confirmed",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "signal with details failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn bullpen_signals_filter() {
    let dir = tempfile::tempdir().unwrap();

    // Post a signal
    legion_cmd(dir.path())
        .args([
            "signal", "--repo", "kelex", "--to", "legion", "--verb", "review", "--status",
            "approved",
        ])
        .output()
        .unwrap();

    // Post a musing
    legion_cmd(dir.path())
        .args([
            "post",
            "--repo",
            "rafters",
            "--text",
            "deep thoughts about design patterns",
        ])
        .output()
        .unwrap();

    // --signals should show only the signal
    let output = legion_cmd(dir.path())
        .args(["bullpen", "--repo", "platform", "--signals"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("@legion"), "expected signal, got: {stdout}");
    assert!(
        !stdout.contains("deep thoughts"),
        "expected no musings in --signals, got: {stdout}"
    );

    // --musings should show only the musing
    let output = legion_cmd(dir.path())
        .args(["bullpen", "--repo", "courses", "--musings"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("deep thoughts"),
        "expected musing, got: {stdout}"
    );
    assert!(
        !stdout.contains("@legion"),
        "expected no signals in --musings, got: {stdout}"
    );
}

#[test]
fn task_full_lifecycle() {
    let dir = tempfile::tempdir().unwrap();

    // Create a task
    let out = legion_cmd(dir.path())
        .args([
            "task",
            "create",
            "--from",
            "kelex",
            "--to",
            "legion",
            "--text",
            "implement search feature",
            "--priority",
            "high",
            "--context",
            "BM25 index needed",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "task create failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let task_id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(!task_id.is_empty(), "expected task ID on stdout");

    // List inbound tasks for legion
    let out = legion_cmd(dir.path())
        .args(["task", "list", "--repo", "legion"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains("implement search feature"),
        "expected task text in list, got: {stdout}"
    );
    assert!(
        stdout.contains("[pending]"),
        "expected pending status, got: {stdout}"
    );
    assert!(
        stdout.contains("from:kelex"),
        "expected from attribution, got: {stdout}"
    );
    assert!(
        stdout.contains("[high]"),
        "expected high priority, got: {stdout}"
    );

    // Accept the task
    let out = legion_cmd(dir.path())
        .args(["--verbose", "task", "accept", "--id", &task_id])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "task accept failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("task accepted"),
        "expected accept confirmation, got: {stderr}"
    );

    // Complete the task
    let out = legion_cmd(dir.path())
        .args([
            "--verbose",
            "task",
            "done",
            "--id",
            &task_id,
            "--note",
            "shipped in v0.2",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "task done failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("task completed"),
        "expected completion confirmation, got: {stderr}"
    );

    // List should show done status
    let out = legion_cmd(dir.path())
        .args(["task", "list", "--repo", "legion"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains("[done]"),
        "expected done status, got: {stdout}"
    );
}

#[test]
fn task_block_flow() {
    let dir = tempfile::tempdir().unwrap();

    // Create and accept
    let out = legion_cmd(dir.path())
        .args([
            "task",
            "create",
            "--from",
            "kelex",
            "--to",
            "legion",
            "--text",
            "blocked task",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let task_id = String::from_utf8_lossy(&out.stdout).trim().to_string();

    let out = legion_cmd(dir.path())
        .args(["task", "accept", "--id", &task_id])
        .output()
        .unwrap();
    assert!(out.status.success());

    // Block the task
    let out = legion_cmd(dir.path())
        .args([
            "--verbose",
            "task",
            "block",
            "--id",
            &task_id,
            "--reason",
            "waiting on upstream",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "task block failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("task blocked"),
        "expected block confirmation, got: {stderr}"
    );
}

#[test]
fn task_list_outbound() {
    let dir = tempfile::tempdir().unwrap();

    // Create tasks from kelex
    let out = legion_cmd(dir.path())
        .args([
            "task",
            "create",
            "--from",
            "kelex",
            "--to",
            "legion",
            "--text",
            "task for legion",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());

    let out = legion_cmd(dir.path())
        .args([
            "task",
            "create",
            "--from",
            "kelex",
            "--to",
            "rafters",
            "--text",
            "task for rafters",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());

    // List outbound from kelex
    let out = legion_cmd(dir.path())
        .args(["task", "list", "--repo", "kelex", "--from"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains("task for legion"),
        "expected legion task, got: {stdout}"
    );
    assert!(
        stdout.contains("task for rafters"),
        "expected rafters task, got: {stdout}"
    );
    assert!(
        stdout.contains("outbound"),
        "expected outbound label, got: {stdout}"
    );
}

#[test]
fn task_invalid_state_transition() {
    let dir = tempfile::tempdir().unwrap();

    // Create a task
    let out = legion_cmd(dir.path())
        .args([
            "task",
            "create",
            "--from",
            "kelex",
            "--to",
            "legion",
            "--text",
            "cannot skip accept",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let task_id = String::from_utf8_lossy(&out.stdout).trim().to_string();

    // Try to complete a pending task (should fail)
    let out = legion_cmd(dir.path())
        .args(["task", "done", "--id", &task_id])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "completing a pending task should fail"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("InvalidTaskTransition"),
        "expected state transition error, got: {stderr}"
    );
}

#[test]
fn task_surface_shows_pending() {
    let dir = tempfile::tempdir().unwrap();

    // Need to init DB first
    let out = legion_cmd(dir.path())
        .args(["reflect", "--repo", "test", "--text", "setup"])
        .output()
        .unwrap();
    assert!(out.status.success());

    // Create a pending task for legion
    let out = legion_cmd(dir.path())
        .args([
            "task",
            "create",
            "--from",
            "kelex",
            "--to",
            "legion",
            "--text",
            "pending task for surface",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());

    // Surface should show the pending task
    let output = legion_cmd(dir.path())
        .args(["surface", "--repo", "legion"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "surface failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("pending task for surface"),
        "expected pending task in surface, got: {stdout}"
    );
    assert!(
        stdout.contains("Task from kelex"),
        "expected task attribution, got: {stdout}"
    );
}

// --- Kanban CLI tests ---

#[test]
fn kanban_create_and_list() {
    let dir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path())
        .args([
            "kanban",
            "create",
            "--from",
            "sean",
            "--to",
            "kelex",
            "--text",
            "implement search",
            "--priority",
            "high",
            "--labels",
            "backend,search",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "kanban create failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_eq!(id.len(), 36, "expected UUID, got: {id}");

    let out = legion_cmd(dir.path())
        .args(["kanban", "list", "--repo", "kelex"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("implement search"),
        "expected card text, got: {stdout}"
    );
    assert!(
        stdout.contains("[high]"),
        "expected priority, got: {stdout}"
    );
    // Labels are no longer shown inline -- they're stored but not displayed in list output
}

#[test]
fn kanban_work_picks_up_card() {
    let dir = tempfile::tempdir().unwrap();

    // Create two cards with different priorities
    legion_cmd(dir.path())
        .args([
            "kanban",
            "create",
            "--from",
            "sean",
            "--to",
            "kelex",
            "--text",
            "low prio",
            "--priority",
            "low",
        ])
        .output()
        .unwrap();
    legion_cmd(dir.path())
        .args([
            "kanban",
            "create",
            "--from",
            "sean",
            "--to",
            "kelex",
            "--text",
            "high prio",
            "--priority",
            "high",
        ])
        .output()
        .unwrap();

    // Work should pick up the high priority card
    let out = legion_cmd(dir.path())
        .args(["work", "--repo", "kelex"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("high prio"),
        "expected high prio card, got: {stdout}"
    );
    assert!(
        stdout.contains("Priority: high"),
        "expected priority line, got: {stdout}"
    );
}

#[test]
fn kanban_work_empty_queue() {
    let dir = tempfile::tempdir().unwrap();

    // Init DB
    legion_cmd(dir.path())
        .args(["reflect", "--repo", "test", "--text", "setup"])
        .output()
        .unwrap();

    let out = legion_cmd(dir.path())
        .args(["--verbose", "work", "--repo", "kelex"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no pending work"),
        "expected no-work message, got: {stderr}"
    );
}

#[test]
fn kanban_work_peek_does_not_accept() {
    let dir = tempfile::tempdir().unwrap();

    legion_cmd(dir.path())
        .args([
            "kanban",
            "create",
            "--from",
            "sean",
            "--to",
            "kelex",
            "--text",
            "peek test",
            "--priority",
            "med",
        ])
        .output()
        .unwrap();

    // Peek should show card but not accept
    let out = legion_cmd(dir.path())
        .args(["work", "--repo", "kelex", "--peek"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("peek test"), "expected card, got: {stdout}");

    // Card should still be pending (work without peek should still get it)
    let out = legion_cmd(dir.path())
        .args(["work", "--repo", "kelex"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("peek test"),
        "card should still be available, got: {stdout}"
    );
}

#[test]
fn kanban_full_lifecycle() {
    let dir = tempfile::tempdir().unwrap();

    // Create
    let out = legion_cmd(dir.path())
        .args([
            "kanban",
            "create",
            "--from",
            "sean",
            "--to",
            "kelex",
            "--text",
            "lifecycle test",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();

    // Accept
    let out = legion_cmd(dir.path())
        .args(["kanban", "accept", "--id", &id])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "accept failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Review
    let out = legion_cmd(dir.path())
        .args(["kanban", "review", "--id", &id])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "review failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Done via kanban
    let out = legion_cmd(dir.path())
        .args(["done", "--repo", "kelex", "--text", "shipped", "--id", &id])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "done failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn kanban_block_unblock() {
    let dir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path())
        .args([
            "kanban",
            "create",
            "--from",
            "sean",
            "--to",
            "kelex",
            "--text",
            "block test",
        ])
        .output()
        .unwrap();
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();

    legion_cmd(dir.path())
        .args(["kanban", "accept", "--id", &id])
        .output()
        .unwrap();

    let out = legion_cmd(dir.path())
        .args([
            "kanban",
            "block",
            "--id",
            &id,
            "--reason",
            "waiting on upstream",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "block failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = legion_cmd(dir.path())
        .args(["kanban", "unblock", "--id", &id])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "unblock failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn kanban_invalid_transition() {
    let dir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path())
        .args([
            "kanban",
            "create",
            "--from",
            "sean",
            "--to",
            "kelex",
            "--text",
            "invalid test",
        ])
        .output()
        .unwrap();
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();

    // Try to review a pending card (should fail -- must accept first)
    let out = legion_cmd(dir.path())
        .args(["kanban", "review", "--id", &id])
        .output()
        .unwrap();
    assert!(!out.status.success(), "review of pending card should fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("InvalidCardTransition"),
        "expected transition error, got: {stderr}"
    );
}

#[test]
fn kanban_with_source_url() {
    let dir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path())
        .args([
            "kanban",
            "create",
            "--from",
            "sean",
            "--to",
            "kelex",
            "--text",
            "github issue",
            "--source-url",
            "https://github.com/runlegion/legion/issues/42",
            "--source-type",
            "github",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_eq!(id.len(), 36);

    let out = legion_cmd(dir.path())
        .args(["kanban", "list", "--repo", "kelex"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("github.com"),
        "expected source URL in output, got: {stdout}"
    );
}

// --- kanban view tests ---

#[test]
fn kanban_view_human_output() {
    let dir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path())
        .args([
            "kanban",
            "create",
            "--from",
            "sean",
            "--to",
            "kelex",
            "--text",
            "view me",
            "--priority",
            "high",
            "--labels",
            "backend",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();

    let out = legion_cmd(dir.path())
        .args(["kanban", "view", "--id", &id])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "view failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(&id), "card id in output");
    assert!(stdout.contains("view me"), "title in output");
    assert!(stdout.contains("high"), "priority in output");
}

#[test]
fn kanban_view_json_output() {
    let dir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path())
        .args([
            "kanban",
            "create",
            "--from",
            "sean",
            "--to",
            "kelex",
            "--text",
            "json view",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();

    let out = legion_cmd(dir.path())
        .args(["kanban", "view", "--id", &id, "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "view --json failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("output should be valid JSON");
    assert_eq!(parsed["id"].as_str().unwrap(), id, "id matches");
    assert_eq!(parsed["from_repo"].as_str().unwrap(), "sean");
}

#[test]
fn kanban_view_not_found_exits_nonzero() {
    let dir = tempfile::tempdir().unwrap();

    // Initialize DB
    legion_cmd(dir.path())
        .args(["reflect", "--repo", "test", "--text", "setup"])
        .output()
        .unwrap();

    let out = legion_cmd(dir.path())
        .args([
            "kanban",
            "view",
            "--id",
            "00000000-0000-0000-0000-000000000000",
        ])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "expected non-zero exit for missing card"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("card not found"),
        "expected card not found message, got: {stderr}"
    );
}

// --- kanban update tests ---

#[test]
fn kanban_update_text() {
    let dir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path())
        .args([
            "kanban",
            "create",
            "--from",
            "sean",
            "--to",
            "kelex",
            "--text",
            "original title",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();

    let out = legion_cmd(dir.path())
        .args([
            "kanban",
            "update",
            "--id",
            &id,
            "--repo",
            "sean",
            "--text",
            "updated title",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "update failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Should print the card id
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.trim() == id, "expected card id, got: {stdout}");

    // Verify the title changed
    let out = legion_cmd(dir.path())
        .args(["kanban", "view", "--id", &id, "--json"])
        .output()
        .unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("valid JSON");
    assert_eq!(parsed["text"].as_str().unwrap(), "updated title");
}

#[test]
fn kanban_update_priority() {
    let dir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path())
        .args([
            "kanban",
            "create",
            "--from",
            "sean",
            "--to",
            "kelex",
            "--text",
            "prio test",
            "--priority",
            "low",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();

    let out = legion_cmd(dir.path())
        .args([
            "kanban",
            "update",
            "--id",
            &id,
            "--repo",
            "sean",
            "--priority",
            "critical",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "update failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = legion_cmd(dir.path())
        .args(["kanban", "view", "--id", &id, "--json"])
        .output()
        .unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("valid JSON");
    assert_eq!(parsed["priority"].as_str().unwrap(), "critical");
}

#[test]
fn kanban_update_add_labels_deduplicates() {
    let dir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path())
        .args([
            "kanban",
            "create",
            "--from",
            "sean",
            "--to",
            "kelex",
            "--text",
            "label test",
            "--labels",
            "backend,api",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();

    let out = legion_cmd(dir.path())
        .args([
            "kanban",
            "update",
            "--id",
            &id,
            "--repo",
            "sean",
            "--add-labels",
            "api,frontend",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "add-labels failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = legion_cmd(dir.path())
        .args(["kanban", "view", "--id", &id, "--json"])
        .output()
        .unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).expect("valid JSON");
    let labels_raw = parsed["labels"].as_str().unwrap_or("");
    let label_parts: Vec<&str> = labels_raw.split(',').collect();
    assert_eq!(
        label_parts.iter().filter(|&&l| l == "api").count(),
        1,
        "api appears exactly once"
    );
    assert!(label_parts.contains(&"frontend"), "frontend added");
    assert!(label_parts.contains(&"backend"), "backend preserved");
}

#[test]
fn kanban_update_no_flags_exits_nonzero() {
    let dir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path())
        .args([
            "kanban", "create", "--from", "sean", "--to", "kelex", "--text", "no flags",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();

    let out = legion_cmd(dir.path())
        .args(["kanban", "update", "--id", &id, "--repo", "sean"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "expected non-zero exit when no fields provided"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no fields to update"),
        "expected helpful message, got: {stderr}"
    );
}

// --- kanban list --json tests ---

#[test]
fn kanban_list_json_emits_jsonl() {
    let dir = tempfile::tempdir().unwrap();

    legion_cmd(dir.path())
        .args([
            "kanban",
            "create",
            "--from",
            "sean",
            "--to",
            "kelex",
            "--text",
            "## Card Alpha",
            "--labels",
            "backend",
        ])
        .output()
        .unwrap();
    legion_cmd(dir.path())
        .args([
            "kanban",
            "create",
            "--from",
            "sean",
            "--to",
            "kelex",
            "--text",
            "Card Beta",
        ])
        .output()
        .unwrap();

    let out = legion_cmd(dir.path())
        .args(["kanban", "list", "--repo", "kelex", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "list --json failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "two lines for two cards");

    for line in &lines {
        let parsed: serde_json::Value =
            serde_json::from_str(line).expect("each line should be valid JSON");
        assert!(parsed["id"].is_string(), "id present");
        assert!(parsed["title"].is_string(), "title present");
        assert!(parsed["status"].is_string(), "status present");
        assert!(
            !parsed.as_object().unwrap().contains_key("context"),
            "no context in summary"
        );
    }

    // Card Alpha should have heading stripped
    let alpha_line = lines.iter().find(|&&l| l.contains("Card Alpha"));
    assert!(
        alpha_line.is_some(),
        "Card Alpha (heading stripped) found in output"
    );
}

#[test]
fn kanban_list_json_labels_are_array() {
    let dir = tempfile::tempdir().unwrap();

    legion_cmd(dir.path())
        .args([
            "kanban",
            "create",
            "--from",
            "sean",
            "--to",
            "kelex",
            "--text",
            "labeled",
            "--labels",
            "backend,api",
        ])
        .output()
        .unwrap();

    let out = legion_cmd(dir.path())
        .args(["kanban", "list", "--repo", "kelex", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout.lines().next().expect("has output");
    let parsed: serde_json::Value = serde_json::from_str(line).expect("valid JSON");
    let labels = parsed["labels"].as_array().expect("labels is array");
    assert!(labels.iter().any(|l| l.as_str() == Some("backend")));
    assert!(labels.iter().any(|l| l.as_str() == Some("api")));
}

/// Verify `legion pr close` fails with a clear error when the repo has no
/// work source config in watch.toml. Network access is not available in tests,
/// so we confirm the CLI is correctly wired without invoking `gh`.
#[test]
fn pr_close_errors_without_worksource_config() {
    let dir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path())
        .args(["pr", "close", "--repo", "no-such-repo", "--number", "42"])
        .output()
        .unwrap();

    assert!(
        !out.status.success(),
        "expected failure when no work source configured"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no work source configured"),
        "expected 'no work source configured' in stderr, got: {stderr}"
    );
}

/// Verify `legion pr close --delete-branch` flag is accepted by the CLI parser.
/// Errors at the worksource level (no config) rather than at argument parsing.
#[test]
fn pr_close_delete_branch_flag_accepted() {
    let dir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path())
        .args([
            "pr",
            "close",
            "--repo",
            "no-such-repo",
            "--number",
            "42",
            "--reason",
            "superseded",
            "--delete-branch",
        ])
        .output()
        .unwrap();

    // Fails at worksource resolution, not arg parsing -- confirms all flags parse
    assert!(
        !out.status.success(),
        "expected failure when no work source configured"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no work source configured"),
        "expected worksource error, got: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Usage command integration tests
// ---------------------------------------------------------------------------

/// Build a minimal assistant-turn JSONL event string for fixture files.
fn usage_assistant_event(
    input: u64,
    output: u64,
    cache_write: u64,
    cache_read: u64,
    ts: &str,
) -> String {
    format!(
        r#"{{"type":"assistant","timestamp":"{ts}","message":{{"role":"assistant","usage":{{"input_tokens":{input},"output_tokens":{output},"cache_creation_input_tokens":{cache_write},"cache_read_input_tokens":{cache_read}}}}}}}"#
    )
}

/// A variant of `legion_cmd` that also overrides HOME so the usage command
/// reads sessions from the tempdir instead of the real ~/.claude/projects/.
fn legion_cmd_with_home(data_dir: &std::path::Path, home_dir: &std::path::Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_legion"));
    cmd.env("LEGION_DATA_DIR", data_dir);
    cmd.env("HOME", home_dir);
    cmd
}

#[test]
fn usage_today_shows_table_header() {
    let data_dir = tempfile::tempdir().unwrap();
    let home_dir = tempfile::tempdir().unwrap();

    // Set up one session under a fake slug.
    let ts = "2099-01-01T00:00:00.000Z"; // far future -- always "today" test breaks if date matches, so use today
    let today = chrono::Utc::now()
        .format("%Y-%m-%dT00:01:00.000Z")
        .to_string();
    let slug = "-Users-test-projects-myrepo";
    let slug_dir = home_dir.path().join(".claude/projects").join(slug);
    std::fs::create_dir_all(&slug_dir).unwrap();
    let event = usage_assistant_event(100, 200, 0, 0, &today);
    std::fs::write(
        slug_dir.join("aaaaaaaa-0000-0000-0000-000000000001.jsonl"),
        format!("{event}\n"),
    )
    .unwrap();

    let _ = ts; // used for reference in comment above

    let out = legion_cmd_with_home(data_dir.path(), home_dir.path())
        .args(["usage"])
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "usage failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Table should contain "id" and "cost" column headers.
    assert!(
        stdout.contains("id") && stdout.contains("cost"),
        "expected table header in output, got: {stdout}"
    );
}

#[test]
fn usage_json_flag_emits_parseable_json() {
    let data_dir = tempfile::tempdir().unwrap();
    let home_dir = tempfile::tempdir().unwrap();

    let today = chrono::Utc::now()
        .format("%Y-%m-%dT00:02:00.000Z")
        .to_string();
    let slug = "-Users-test-projects-testjson";
    let slug_dir = home_dir.path().join(".claude/projects").join(slug);
    std::fs::create_dir_all(&slug_dir).unwrap();
    let event = usage_assistant_event(50, 100, 0, 0, &today);
    std::fs::write(
        slug_dir.join("bbbbbbbb-0000-0000-0000-000000000002.jsonl"),
        format!("{event}\n"),
    )
    .unwrap();

    let out = legion_cmd_with_home(data_dir.path(), home_dir.path())
        .args(["usage", "--today", "--json"])
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "usage --json failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Must be valid JSON (parseable).
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("usage --json output is not valid JSON: {e}\noutput: {stdout}"));
    // Should be an array of sessions.
    assert!(parsed.is_array(), "expected JSON array, got: {parsed}");
}

#[test]
fn usage_by_repo_groups_sessions() {
    let data_dir = tempfile::tempdir().unwrap();
    let home_dir = tempfile::tempdir().unwrap();

    let today = chrono::Utc::now()
        .format("%Y-%m-%dT00:03:00.000Z")
        .to_string();
    let slug = "-Users-test-projects-groupedrepo";
    let slug_dir = home_dir.path().join(".claude/projects").join(slug);
    std::fs::create_dir_all(&slug_dir).unwrap();

    // Two sessions, same slug = same repo.
    let event = usage_assistant_event(10, 20, 0, 0, &today);
    std::fs::write(
        slug_dir.join("cccccccc-0000-0000-0000-000000000003.jsonl"),
        format!("{event}\n"),
    )
    .unwrap();
    std::fs::write(
        slug_dir.join("dddddddd-0000-0000-0000-000000000004.jsonl"),
        format!("{event}\n"),
    )
    .unwrap();

    let out = legion_cmd_with_home(data_dir.path(), home_dir.path())
        .args(["usage", "--today", "--by-repo"])
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "usage --by-repo failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // The grouped repo name should appear.
    assert!(
        stdout.contains("groupedrepo"),
        "expected repo name in --by-repo output, got: {stdout}"
    );
}

#[test]
fn usage_session_flag_exits_nonzero_when_not_found() {
    let data_dir = tempfile::tempdir().unwrap();
    let home_dir = tempfile::tempdir().unwrap();

    // Create an empty projects dir so discovery works.
    std::fs::create_dir_all(home_dir.path().join(".claude/projects")).unwrap();

    let out = legion_cmd_with_home(data_dir.path(), home_dir.path())
        .args(["usage", "--session", "nonexistent-uuid-1234"])
        .output()
        .unwrap();

    assert!(
        !out.status.success(),
        "expected non-zero exit for missing session"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("session not found"),
        "expected 'session not found' error, got: {stderr}"
    );
}

#[test]
fn usage_no_sessions_prints_no_sessions_found() {
    let data_dir = tempfile::tempdir().unwrap();
    let home_dir = tempfile::tempdir().unwrap();

    // Create empty projects dir.
    std::fs::create_dir_all(home_dir.path().join(".claude/projects")).unwrap();

    let out = legion_cmd_with_home(data_dir.path(), home_dir.path())
        .args(["usage", "--today"])
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "usage with empty projects should succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("no sessions found"),
        "expected 'no sessions found', got: {stdout}"
    );
}

// --- Card A: legion similar ---

#[test]
fn similar_nonexistent_id_exits_nonzero() {
    let dir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path())
        .args(["similar", "--id", "00000000-0000-0000-0000-000000000000"])
        .output()
        .unwrap();

    // Exits non-zero (either "reflection not found" or "embed model unavailable")
    assert!(
        !out.status.success(),
        "expected non-zero exit for nonexistent id"
    );
}

#[test]
fn similar_json_flag_accepted() {
    // Verify the --json flag parses without panic. Exits non-zero because no model/id,
    // but argument parsing must succeed.
    let dir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path())
        .args([
            "similar",
            "--id",
            "00000000-0000-0000-0000-000000000000",
            "--json",
        ])
        .output()
        .unwrap();

    // Should exit non-zero, not panic
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("panicked"),
        "should not panic, got: {stderr}"
    );
}

// --- Card B: recall --cosine-only and --min-score ---

#[test]
fn recall_cosine_only_flag_accepted_by_parser() {
    // When embed model is unavailable, --cosine-only exits non-zero with a clear error.
    // This verifies the flag is accepted by the parser and the handler runs.
    let dir = tempfile::tempdir().unwrap();

    // Reflect something first so there's data
    legion_cmd(dir.path())
        .args(["reflect", "--repo", "test", "--text", "some reflection"])
        .output()
        .unwrap();

    let out = legion_cmd(dir.path())
        .args([
            "recall",
            "--repo",
            "test",
            "--context",
            "some",
            "--cosine-only",
        ])
        .output()
        .unwrap();

    // Either succeeds (model available) or fails with embedding error (not a parse error)
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("unexpected argument"),
        "flag should parse correctly, got: {stderr}"
    );
}

#[test]
fn recall_min_score_flag_accepted_by_parser() {
    let dir = tempfile::tempdir().unwrap();

    legion_cmd(dir.path())
        .args(["reflect", "--repo", "test", "--text", "min score test"])
        .output()
        .unwrap();

    let out = legion_cmd(dir.path())
        .args([
            "recall",
            "--repo",
            "test",
            "--context",
            "min score",
            "--min-score",
            "0.5",
        ])
        .output()
        .unwrap();

    // Should either succeed or fail on embedding, not on arg parsing
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("unexpected argument"),
        "flag should parse correctly, got: {stderr}"
    );
}

// --- Card C: reflect --dedupe-mode and --force ---

#[test]
fn reflect_force_flag_accepted() {
    let dir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path())
        .args([
            "reflect",
            "--repo",
            "test",
            "--text",
            "some text",
            "--force",
        ])
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "reflect --force should succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn reflect_dedupe_mode_off_accepted() {
    let dir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path())
        .args([
            "reflect",
            "--repo",
            "test",
            "--text",
            "some text",
            "--dedupe-mode",
            "off",
        ])
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "reflect --dedupe-mode off should succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn reflect_dedupe_mode_warn_stores_and_exits_zero() {
    let dir = tempfile::tempdir().unwrap();

    // Store first; second is a duplicate but with warn mode should still succeed.
    let text = "near duplicate reflection text for testing";

    legion_cmd(dir.path())
        .args(["reflect", "--repo", "test", "--text", text])
        .output()
        .unwrap();

    let out = legion_cmd(dir.path())
        .args([
            "reflect",
            "--repo",
            "test",
            "--text",
            text,
            "--dedupe-mode",
            "warn",
        ])
        .output()
        .unwrap();

    // In warn mode: always exits zero (embed model may or may not be available).
    assert!(
        out.status.success(),
        "reflect --dedupe-mode warn should exit zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Requires embed model -- tests that strict mode blocks duplicates.
#[test]
#[ignore]
fn reflect_dedupe_mode_strict_blocks_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    let text = "strict dedup test reflection content";

    // First store succeeds
    let first = legion_cmd(dir.path())
        .args(["reflect", "--repo", "test", "--text", text])
        .output()
        .unwrap();
    assert!(first.status.success());

    // Second store with strict mode should fail (near-duplicate)
    let second = legion_cmd(dir.path())
        .args([
            "reflect",
            "--repo",
            "test",
            "--text",
            text,
            "--dedupe-mode",
            "strict",
        ])
        .output()
        .unwrap();
    assert!(
        !second.status.success(),
        "strict mode should refuse identical reflection"
    );
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("near-duplicate"),
        "error message should mention near-duplicate, got: {stderr}"
    );
}

/// Requires embed model -- tests that --force bypasses strict dedupe.
#[test]
#[ignore]
fn reflect_force_bypasses_strict_dedupe() {
    let dir = tempfile::tempdir().unwrap();
    let text = "force bypass test reflection";

    legion_cmd(dir.path())
        .args(["reflect", "--repo", "test", "--text", text])
        .output()
        .unwrap();

    let out = legion_cmd(dir.path())
        .args([
            "reflect",
            "--repo",
            "test",
            "--text",
            text,
            "--dedupe-mode",
            "strict",
            "--force",
        ])
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "--force should bypass strict mode: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
