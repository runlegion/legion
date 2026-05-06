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

#[test]
fn recall_by_domain_filters_correctly() {
    let dir = tempfile::tempdir().unwrap();

    // Create two reflections: one with domain "identity", one with domain "checkpoint"
    let out = legion_cmd(dir.path())
        .args([
            "reflect",
            "--repo",
            "test",
            "--text",
            "I am the test agent",
            "--domain",
            "identity",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());

    let out = legion_cmd(dir.path())
        .args([
            "reflect",
            "--repo",
            "test",
            "--text",
            "checkpoint before compact",
            "--domain",
            "checkpoint",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());

    // Also create one without a domain
    let out = legion_cmd(dir.path())
        .args([
            "reflect",
            "--repo",
            "test",
            "--text",
            "generic reflection no domain",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());

    // Recall with --domain identity should return only the identity reflection
    let out = legion_cmd(dir.path())
        .args([
            "recall", "--repo", "test", "--domain", "identity", "--limit", "5",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains("I am the test agent"),
        "expected identity reflection, got: {stdout}"
    );
    assert!(
        !stdout.contains("checkpoint before compact"),
        "should not contain checkpoint reflection: {stdout}"
    );
    assert!(
        !stdout.contains("generic reflection"),
        "should not contain domainless reflection: {stdout}"
    );

    // Recall with --domain checkpoint should return only the checkpoint reflection
    let out = legion_cmd(dir.path())
        .args([
            "recall",
            "--repo",
            "test",
            "--domain",
            "checkpoint",
            "--limit",
            "5",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains("checkpoint before compact"),
        "expected checkpoint reflection, got: {stdout}"
    );
    assert!(
        !stdout.contains("I am the test agent"),
        "should not contain identity reflection: {stdout}"
    );

    // Recall with --domain nonexistent should return nothing
    let out = legion_cmd(dir.path())
        .args([
            "recall",
            "--repo",
            "test",
            "--domain",
            "nonexistent",
            "--limit",
            "5",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.trim().is_empty(),
        "expected empty output for nonexistent domain, got: {stdout}"
    );
}

#[test]
fn whoami_flag_stores_as_identity_domain() {
    let dir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path())
        .args([
            "reflect",
            "--repo",
            "test",
            "--whoami",
            "--text",
            "I am the test agent",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "whoami reflect failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = legion_cmd(dir.path())
        .args([
            "recall", "--repo", "test", "--domain", "identity", "--limit", "5",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains("I am the test agent"),
        "expected whoami reflection under domain=identity, got: {stdout}"
    );
}

#[test]
fn whoami_conflicts_with_domain_flag() {
    let dir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path())
        .args([
            "reflect",
            "--repo",
            "test",
            "--whoami",
            "--domain",
            "something-else",
            "--text",
            "should not store",
        ])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "expected --whoami + --domain to fail, but it succeeded"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cannot be used with"),
        "expected clap conflict error, got: {stderr}"
    );
}

#[test]
fn whoami_flag_works_with_transcript_input() {
    let dir = tempfile::tempdir().unwrap();
    let transcript = dir.path().join("transcript.jsonl");
    std::fs::write(
        &transcript,
        r#"{"role":"assistant","content":"I am the test agent from transcript"}
"#,
    )
    .unwrap();

    let out = legion_cmd(dir.path())
        .args(["reflect", "--repo", "test", "--whoami", "--transcript"])
        .arg(&transcript)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "whoami + transcript reflect failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = legion_cmd(dir.path())
        .args([
            "recall", "--repo", "test", "--domain", "identity", "--limit", "5",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains("I am the test agent from transcript"),
        "expected whoami reflection from transcript under domain=identity, got: {stdout}"
    );
}

#[test]
fn whoami_works_with_compound_repo() {
    let dir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path())
        .args([
            "reflect",
            "--repo",
            "alpha,beta",
            "--whoami",
            "--text",
            "shared identity text",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "compound whoami reflect failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    for repo in ["alpha", "beta"] {
        let out = legion_cmd(dir.path())
            .args([
                "recall", "--repo", repo, "--domain", "identity", "--limit", "5",
            ])
            .output()
            .unwrap();
        assert!(out.status.success());
        let stdout = String::from_utf8(out.stdout).unwrap();
        assert!(
            stdout.contains("shared identity text"),
            "expected identity reflection in {repo}, got: {stdout}"
        );
    }
}

#[test]
fn whoami_subcommand_returns_identity_reflections() {
    let dir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path())
        .args([
            "reflect",
            "--repo",
            "test",
            "--whoami",
            "--text",
            "I am the test agent",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());

    let out = legion_cmd(dir.path())
        .args(["whoami", "--repo", "test"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "whoami subcommand failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains("[Legion] Identity for test:"),
        "expected identity header, got: {stdout}"
    );
    assert!(
        stdout.contains("I am the test agent"),
        "expected identity text, got: {stdout}"
    );
}

#[test]
fn whoami_subcommand_silent_when_no_identity() {
    let dir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path())
        .args(["whoami", "--repo", "empty"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        out.stdout.is_empty(),
        "expected empty output, got: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn whoami_subcommand_requires_repo() {
    let dir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path()).args(["whoami"]).output().unwrap();
    assert!(!out.status.success());
}

#[test]
fn whoami_subcommand_isolates_by_repo() {
    let dir = tempfile::tempdir().unwrap();

    for (repo, text) in [("alpha", "alpha identity"), ("beta", "beta identity")] {
        let out = legion_cmd(dir.path())
            .args(["reflect", "--repo", repo, "--whoami", "--text", text])
            .output()
            .unwrap();
        assert!(out.status.success());
    }

    let out = legion_cmd(dir.path())
        .args(["whoami", "--repo", "alpha"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("alpha identity"));
    assert!(
        !stdout.contains("beta identity"),
        "whoami leaked beta identity into alpha repo: {stdout}"
    );
}

#[test]
fn whoami_subcommand_filters_to_identity_domain() {
    let dir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path())
        .args([
            "reflect",
            "--repo",
            "test",
            "--whoami",
            "--text",
            "I am identity",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());

    let out = legion_cmd(dir.path())
        .args([
            "reflect",
            "--repo",
            "test",
            "--text",
            "plain non-identity reflection",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());

    let out = legion_cmd(dir.path())
        .args(["whoami", "--repo", "test"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("I am identity"));
    assert!(
        !stdout.contains("plain non-identity reflection"),
        "whoami included non-identity reflection: {stdout}"
    );
}

#[test]
fn whoami_subcommand_respects_limit() {
    let dir = tempfile::tempdir().unwrap();

    for i in 0..3 {
        let text = format!("identity reflection {i}");
        let out = legion_cmd(dir.path())
            .args(["reflect", "--repo", "test", "--whoami", "--text", &text])
            .output()
            .unwrap();
        assert!(out.status.success());
    }

    let out = legion_cmd(dir.path())
        .args(["whoami", "--repo", "test", "--limit", "1"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    let bullet_count = stdout.lines().filter(|l| l.starts_with("- ")).count();
    assert_eq!(
        bullet_count, 1,
        "expected 1 bullet, got {bullet_count}: {stdout}"
    );
}

#[test]
fn whoami_subcommand_emits_banner_and_chain_pointer() {
    let dir = tempfile::tempdir().unwrap();

    // Standalone identity reflection -- no chain.
    let out = legion_cmd(dir.path())
        .args([
            "reflect",
            "--repo",
            "test",
            "--whoami",
            "--text",
            "standalone identity",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());

    // Chain head: identity reflection plus a follow-up that links via --follows.
    let out = legion_cmd(dir.path())
        .args([
            "reflect",
            "--repo",
            "test",
            "--whoami",
            "--text",
            "chained identity",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let head_id = String::from_utf8(out.stdout).unwrap().trim().to_owned();

    let out = legion_cmd(dir.path())
        .args([
            "reflect",
            "--repo",
            "test",
            "--text",
            "follow-up reflection",
            "--follows",
            &head_id,
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "follow-up reflect failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = legion_cmd(dir.path())
        .args(["whoami", "--repo", "test"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();

    assert!(
        stdout.contains("=== WHO YOU ARE -- READ THIS ==="),
        "missing opening banner: {stdout}"
    );
    assert!(
        stdout.contains("=== END IDENTITY ==="),
        "missing closing banner: {stdout}"
    );
    assert!(
        stdout.contains(&format!("legion chain --id {head_id}")),
        "missing chain pointer for chain head {head_id}: {stdout}"
    );
    let chain_lines = stdout
        .lines()
        .filter(|l| l.contains("legion chain --id"))
        .count();
    assert_eq!(
        chain_lines, 1,
        "expected exactly one chain pointer (only the chained reflection), got {chain_lines}: {stdout}"
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
fn forget_rejects_wrong_repo_safety_check() {
    let dir = tempfile::tempdir().unwrap();

    // Warm the db with a reflection on repo "kelex".
    let out = legion_cmd(dir.path())
        .args([
            "reflect",
            "--repo",
            "kelex",
            "--text",
            "doomed reflection about mapping rules",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "reflect failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_uuid_format(&id);

    // Forget with the WRONG --repo must refuse the delete and exit nonzero.
    let out = legion_cmd(dir.path())
        .args(["forget", "--id", &id, "--repo", "rafters"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "forget should have failed the safety check, stdout: {}, stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("ReflectionRepoMismatch"),
        "expected safety check error variant, got: {stderr}"
    );
    assert!(
        stderr.contains("kelex"),
        "expected actual repo in error, got: {stderr}"
    );
    assert!(
        stderr.contains("rafters"),
        "expected expected repo in error, got: {stderr}"
    );

    // Reflection must still be recallable -- the rejected delete must not
    // have touched the db or the index.
    let out = legion_cmd(dir.path())
        .args(["recall", "--repo", "kelex", "--context", "mapping rules"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("doomed reflection"),
        "reflection should still be intact after rejected forget, got: {stdout}"
    );

    // Rejected forget attempts must be traceable in the audit log.
    // Destructive-command rejections are forensically relevant.
    let out = legion_cmd(dir.path())
        .args(["audit", "--action", "delete-reflection", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let audit_stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        audit_stdout.contains("\"outcome\": \"rejected\""),
        "rejected forget should be audited, got: {audit_stdout}"
    );
    assert!(
        audit_stdout.contains("expected=rafters"),
        "audit details should name the mismatched expected repo, got: {audit_stdout}"
    );
    assert!(
        audit_stdout.contains("actual=kelex"),
        "audit details should name the actual repo, got: {audit_stdout}"
    );

    // Forget with the CORRECT --repo must succeed.
    let out = legion_cmd(dir.path())
        .args(["forget", "--id", &id, "--repo", "kelex"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "forget with correct repo failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("forgot reflection"),
        "expected forget confirmation, got: {stdout}"
    );

    // The successful delete also produces an audit entry with outcome=success.
    let out = legion_cmd(dir.path())
        .args(["audit", "--action", "delete-reflection", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let audit_stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        audit_stdout.contains("\"outcome\": \"success\""),
        "successful forget should be audited, got: {audit_stdout}"
    );
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
fn consult_symbol_empty_db_human_mode() {
    // #285: --symbol on a fresh DB with no SCIP indexes should exit 0
    // and print "no SCIP index..." on stderr in human mode.
    let dir = tempfile::tempdir().unwrap();
    let output = legion_cmd(dir.path())
        .args(["--verbose", "consult", "--symbol", "Database"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "consult --symbol should succeed on empty DB: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no SCIP index has a definition for `Database`"),
        "expected no-index message on stderr, got: {stderr}"
    );
}

#[test]
fn consult_symbol_empty_db_json_mode() {
    // #285: --symbol --json on a fresh DB returns the empty array.
    let dir = tempfile::tempdir().unwrap();
    let output = legion_cmd(dir.path())
        .args(["consult", "--symbol", "Database", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "consult --symbol --json should succeed on empty DB: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout.trim(),
        "[]",
        "expected `[]` for empty result, got: {stdout}"
    );
}

#[test]
fn consult_requires_context_or_symbol() {
    // #285: consult with neither --context nor --symbol must error.
    let dir = tempfile::tempdir().unwrap();
    let output = legion_cmd(dir.path()).args(["consult"]).output().unwrap();
    assert!(!output.status.success(), "consult with no args should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--context") && stderr.contains("--symbol"),
        "error message should mention both modes, got: {stderr}"
    );
}

#[test]
fn consult_context_and_symbol_mutually_exclusive() {
    // #285: clap's conflicts_with should reject both flags at parse time.
    let dir = tempfile::tempdir().unwrap();
    let output = legion_cmd(dir.path())
        .args(["consult", "--context", "x", "--symbol", "Y"])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "consult with both --context and --symbol should fail at parse time"
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
fn chain_full_single_node_emits_one_boundary() {
    // The identity-chain-load.sh hook (#345) counts boundary markers to
    // decide whether to skip injection (single-node chains are redundant
    // with the SessionStart banner). Lock the contract: lone-root chain
    // emits exactly one `--- ` line.
    let dir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path())
        .args(["reflect", "--repo", "kelex", "--text", "lone root"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();

    let out = legion_cmd(dir.path())
        .args(["chain", "--id", &id, "--full"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let boundary_count = stdout.lines().filter(|l| l.starts_with("--- ")).count();
    assert_eq!(
        boundary_count, 1,
        "single-node chain should emit exactly one boundary marker, got: {stdout}"
    );
}

#[test]
fn chain_full_emits_complete_text() {
    let dir = tempfile::tempdir().unwrap();
    let long_text = "A".repeat(500);

    let out = legion_cmd(dir.path())
        .args(["reflect", "--repo", "kelex", "--text", &long_text])
        .output()
        .unwrap();
    assert!(out.status.success());
    let parent_id = String::from_utf8_lossy(&out.stdout).trim().to_string();

    let child_text = "B".repeat(400);
    let out = legion_cmd(dir.path())
        .args([
            "reflect",
            "--repo",
            "kelex",
            "--text",
            &child_text,
            "--follows",
            &parent_id,
        ])
        .output()
        .unwrap();
    assert!(out.status.success());

    let default_output = legion_cmd(dir.path())
        .args(["chain", "--id", &parent_id])
        .output()
        .unwrap();
    let default_stdout = String::from_utf8_lossy(&default_output.stdout);
    assert!(
        default_stdout.contains("..."),
        "default chain output should show truncation ellipsis"
    );
    assert!(!default_stdout.contains(&long_text));

    let full_output = legion_cmd(dir.path())
        .args(["chain", "--id", &parent_id, "--full"])
        .output()
        .unwrap();
    assert!(full_output.status.success());
    let full_stdout = String::from_utf8_lossy(&full_output.stdout);
    assert!(
        full_stdout.contains(&long_text),
        "--full should emit complete parent text"
    );
    assert!(
        full_stdout.contains(&child_text),
        "--full should emit complete child text"
    );
    assert!(
        full_stdout.contains("--- "),
        "--full should use the boundary marker between reflections"
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

#[test]
fn pr_checks_errors_without_worksource_config() {
    let dir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path())
        .args(["pr", "checks", "--repo", "no-such-repo", "--number", "42"])
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

#[test]
fn pr_checks_json_flag_accepted() {
    let dir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path())
        .args([
            "pr",
            "checks",
            "--repo",
            "no-such-repo",
            "--number",
            "42",
            "--json",
        ])
        .output()
        .unwrap();

    // Fails at worksource resolution, not arg parsing -- confirms --json parses
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
// PR read-side (view / comments / reviews / checks --log-failed) with a
// stubbed worksource plugin. The plugin is a bash script that dispatches on
// the subcommand, ignores the env vars, and echoes the fixture contents.
//
// Gated on #[cfg(unix)] because the stub relies on a bash interpreter, an
// exec bit (chmod 0o755 via PermissionsExt), and the plugin-resolution path
// that legion uses via `Command::new(plugin_path)`. Windows CI builds and
// tests everything else; the worksource plugin itself is bash-based and the
// read surface is exercised end-to-end on ubuntu/macos runners.
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn pr_read_stub_plugin(view_pr: &str, comments: &str, reviews: &str, check_log: &str) -> String {
    // Encode each fixture as a single-quoted heredoc body so shell escaping
    // inside the fixture content (backticks, dollars, braces) cannot break
    // the dispatch script. Uses `cat <<'BODY'` which is literal.
    format!(
        r#"#!/bin/bash
set -e
case "${{1:-}}" in
  view-pr)
    cat <<'BODY'
{view_pr}
BODY
    ;;
  pr-comments)
    cat <<'BODY'
{comments}
BODY
    ;;
  pr-reviews)
    cat <<'BODY'
{reviews}
BODY
    ;;
  pr-checks)
    # Minimal fixture matching ExternalPRCheck shape so `pr checks --log-failed`
    # has something to iterate. One failing check, one success, plus one
    # failure whose link does not match the Actions pattern so the
    # non-Actions-link branch runs.
    cat <<'BODY'
[
  {{"name":"Clippy","state":"FAILURE","workflow":"CI","link":"https://github.com/ex/ex/actions/runs/1/job/42","description":""}},
  {{"name":"Tests","state":"SUCCESS","workflow":"CI","link":"https://github.com/ex/ex/actions/runs/1/job/99","description":""}},
  {{"name":"External","state":"FAILURE","workflow":"CI","link":"https://dashboard.example.com/checks/abc","description":""}}
]
BODY
    ;;
  pr-check-log)
    cat <<'BODY'
{check_log}
BODY
    ;;
  *)
    echo "stub: unknown subcommand $1" >&2
    exit 2
    ;;
esac
"#
    )
}

#[cfg(unix)]
fn setup_pr_read_stub(
    data_dir: &std::path::Path,
    plugin_root: &std::path::Path,
    body: &str,
) -> std::path::PathBuf {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    let worksources = plugin_root.join("worksources");
    fs::create_dir_all(&worksources).unwrap();
    let plugin_path = worksources.join("github");
    fs::write(&plugin_path, body).unwrap();
    let mut perm = fs::metadata(&plugin_path).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&plugin_path, perm).unwrap();

    // watch.toml pointing a fake repo at the stub plugin.
    let watch = format!(
        r#"poll_interval_secs = 30
cooldown_secs = 300

[[repos]]
name = "stub"
github = "owner/stub"
workdir = "{}"
worksource = "github"
"#,
        data_dir.display()
    );
    fs::write(data_dir.join("watch.toml"), watch).unwrap();

    plugin_path
}

#[cfg(unix)]
fn pr_read_cmd(data_dir: &std::path::Path, plugin_root: &std::path::Path) -> Command {
    let mut cmd = legion_cmd(data_dir);
    cmd.env("CLAUDE_PLUGIN_ROOT", plugin_root);
    cmd
}

#[cfg(unix)]
#[test]
fn pr_view_renders_body_and_metadata() {
    let data_dir = tempfile::tempdir().unwrap();
    let plugin_root = tempfile::tempdir().unwrap();
    let body = pr_read_stub_plugin(
        r#"{
  "number": 42,
  "title": "stub PR",
  "state": "OPEN",
  "author": "alice",
  "createdAt": "2026-04-21T10:00:00Z",
  "updatedAt": "2026-04-21T11:00:00Z",
  "body": "multi-line\nbody",
  "headRefName": "feat/x",
  "headSha": "deadbeef",
  "baseRefName": "main",
  "isDraft": false,
  "reviewDecision": "REVIEW_REQUIRED",
  "mergeable": "MERGEABLE"
}"#,
        "[]",
        "[]",
        "",
    );
    setup_pr_read_stub(data_dir.path(), plugin_root.path(), &body);

    let out = pr_read_cmd(data_dir.path(), plugin_root.path())
        .args(["pr", "view", "--repo", "stub", "--number", "42"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "pr view failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("PR #42"));
    assert!(stdout.contains("stub PR"));
    assert!(stdout.contains("OPEN"));
    assert!(stdout.contains("feat/x -> main"));
    assert!(stdout.contains("REVIEW_REQUIRED"));
    assert!(stdout.contains("multi-line"));
}

#[cfg(unix)]
#[test]
fn pr_view_json_roundtrips() {
    let data_dir = tempfile::tempdir().unwrap();
    let plugin_root = tempfile::tempdir().unwrap();
    let body = pr_read_stub_plugin(
        r#"{
  "number": 1,
  "title": "t",
  "state": "MERGED",
  "author": "a",
  "createdAt": "2026-04-21T00:00:00Z",
  "updatedAt": "2026-04-21T00:00:00Z",
  "body": "x",
  "headRefName": "f",
  "headSha": "0",
  "baseRefName": "main",
  "isDraft": false,
  "reviewDecision": null,
  "mergeable": null
}"#,
        "[]",
        "[]",
        "",
    );
    setup_pr_read_stub(data_dir.path(), plugin_root.path(), &body);

    let out = pr_read_cmd(data_dir.path(), plugin_root.path())
        .args(["pr", "view", "--repo", "stub", "--number", "1", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(parsed["number"], 1);
    assert_eq!(parsed["state"], "MERGED");
}

#[cfg(unix)]
#[test]
fn pr_comments_handles_empty_thread() {
    let data_dir = tempfile::tempdir().unwrap();
    let plugin_root = tempfile::tempdir().unwrap();
    let body = pr_read_stub_plugin("{}", "[]", "[]", "");
    setup_pr_read_stub(data_dir.path(), plugin_root.path(), &body);

    let out = pr_read_cmd(data_dir.path(), plugin_root.path())
        .args(["pr", "comments", "--repo", "stub", "--number", "1"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no comments"),
        "expected empty-thread notice, got stderr: {stderr}"
    );
}

#[cfg(unix)]
#[test]
fn pr_comments_renders_issue_and_review_mix() {
    let data_dir = tempfile::tempdir().unwrap();
    let plugin_root = tempfile::tempdir().unwrap();
    let body = pr_read_stub_plugin(
        "{}",
        r#"[
  {"id":"1","author":"alice","createdAt":"2026-04-21T09:00:00Z","updatedAt":"2026-04-21T09:00:00Z","body":"top-level thoughts","kind":"issue","path":null,"line":null,"inReplyToId":null},
  {"id":"2","author":"bob","createdAt":"2026-04-21T09:30:00Z","updatedAt":"2026-04-21T09:30:00Z","body":"fix this line","kind":"review","path":"src/foo.rs","line":42,"inReplyToId":null}
]"#,
        "[]",
        "",
    );
    setup_pr_read_stub(data_dir.path(), plugin_root.path(), &body);

    let out = pr_read_cmd(data_dir.path(), plugin_root.path())
        .args(["pr", "comments", "--repo", "stub", "--number", "1"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("[issue]"));
    assert!(stdout.contains("top-level thoughts"));
    assert!(stdout.contains("[review] src/foo.rs:42"));
    assert!(stdout.contains("fix this line"));
}

#[cfg(unix)]
#[test]
fn pr_reviews_renders_inline_comments_grouped() {
    let data_dir = tempfile::tempdir().unwrap();
    let plugin_root = tempfile::tempdir().unwrap();
    let body = pr_read_stub_plugin(
        "{}",
        "[]",
        r#"[
  {
    "id":"10",
    "author":"vault",
    "state":"CHANGES_REQUESTED",
    "submittedAt":"2026-04-21T10:00:00Z",
    "body":"needs work",
    "comments":[
      {"id":"20","author":"vault","createdAt":"2026-04-21T10:00:00Z","updatedAt":"2026-04-21T10:00:00Z","body":"inline nit","kind":"review","path":"src/x.rs","line":12,"inReplyToId":null}
    ]
  }
]"#,
        "",
    );
    setup_pr_read_stub(data_dir.path(), plugin_root.path(), &body);

    let out = pr_read_cmd(data_dir.path(), plugin_root.path())
        .args(["pr", "reviews", "--repo", "stub", "--number", "1"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("[CHANGES_REQUESTED] vault"));
    assert!(stdout.contains("needs work"));
    assert!(stdout.contains("[review] src/x.rs:12"));
    assert!(stdout.contains("inline nit"));
}

#[cfg(unix)]
#[test]
fn pr_checks_log_failed_streams_failing_job_logs() {
    let data_dir = tempfile::tempdir().unwrap();
    let plugin_root = tempfile::tempdir().unwrap();
    let body = pr_read_stub_plugin(
        "{}",
        "[]",
        "[]",
        "error[E0308]: mismatched types\n  --> src/lib.rs:3:5",
    );
    setup_pr_read_stub(data_dir.path(), plugin_root.path(), &body);

    let out = pr_read_cmd(data_dir.path(), plugin_root.path())
        .args([
            "pr",
            "checks",
            "--repo",
            "stub",
            "--number",
            "1",
            "--log-failed",
        ])
        .output()
        .unwrap();
    // Exits non-zero because the stub returns a FAILURE check.
    assert!(!out.status.success(), "expected non-zero exit on failure");
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Actions-linked failure: job id extracted from link, header + log emitted.
    assert!(stdout.contains("===== Clippy (42) ====="));
    assert!(stdout.contains("error[E0308]"));
    // Non-Actions link: skipped with a marker rather than crashing or
    // silently swallowing the failure.
    assert!(stdout.contains("===== External ====="));
    assert!(stdout.contains("non-Actions check link"));
}

/// A stub where pr-check-log always exits non-zero. Exercises the Err branch
/// of fetch_check_log inside the --log-failed loop -- partial failures must
/// render a marker and the outer run must still exit non-zero on the failing
/// check, independently of whether the log fetch succeeded.
#[cfg(unix)]
fn pr_read_stub_plugin_failing_log() -> String {
    r#"#!/bin/bash
set -e
case "${1:-}" in
  pr-checks)
    cat <<'BODY'
[{"name":"Clippy","state":"FAILURE","workflow":"CI","link":"https://github.com/ex/ex/actions/runs/1/job/42","description":""}]
BODY
    ;;
  pr-check-log)
    echo "gh api actions logs failed: HTTP 502" >&2
    exit 1
    ;;
  *)
    echo "stub: unknown subcommand $1" >&2
    exit 2
    ;;
esac
"#
    .to_string()
}

#[cfg(unix)]
#[test]
fn pr_checks_log_failed_marks_partial_log_fetch_failure() {
    let data_dir = tempfile::tempdir().unwrap();
    let plugin_root = tempfile::tempdir().unwrap();
    let body = pr_read_stub_plugin_failing_log();
    setup_pr_read_stub(data_dir.path(), plugin_root.path(), &body);

    let out = pr_read_cmd(data_dir.path(), plugin_root.path())
        .args([
            "pr",
            "checks",
            "--repo",
            "stub",
            "--number",
            "1",
            "--log-failed",
        ])
        .output()
        .unwrap();
    // Outer check failure still drives the exit code even when the log
    // fetch itself fails for the failing job.
    assert!(
        !out.status.success(),
        "expected non-zero exit even when log fetch fails"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("===== Clippy (42) ====="),
        "header still emitted: {stdout}"
    );
    assert!(
        stdout.contains("(log unavailable:"),
        "Err branch marker: {stdout}"
    );
}

#[cfg(unix)]
#[test]
fn pr_checks_log_failed_suppressed_under_json() {
    let data_dir = tempfile::tempdir().unwrap();
    let plugin_root = tempfile::tempdir().unwrap();
    let body = pr_read_stub_plugin("{}", "[]", "[]", "error[E0308]: mismatched types");
    setup_pr_read_stub(data_dir.path(), plugin_root.path(), &body);

    let out = pr_read_cmd(data_dir.path(), plugin_root.path())
        .args([
            "pr",
            "checks",
            "--repo",
            "stub",
            "--number",
            "1",
            "--json",
            "--log-failed",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    // JSON mode must emit a single valid JSON array with no log stream
    // leaking in and no "===== <job> =====" headers corrupting stdout.
    assert!(
        !stdout.contains("====="),
        "log headers leaked into JSON output: {stdout}"
    );
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout must be a single valid JSON array");
    assert!(parsed.is_array(), "expected JSON array, got {parsed}");
}

#[cfg(unix)]
#[test]
fn pr_view_surfaces_malformed_plugin_json_as_worksource_error() {
    // A plugin bug or a gh breaking-change that emits a shape-mismatched
    // blob must produce a loud WorkSource error, not an empty struct. The
    // rendered error does not need to be pretty, but the exit must be
    // non-zero and stderr must mention the PR so the failure site is
    // identifiable.
    let data_dir = tempfile::tempdir().unwrap();
    let plugin_root = tempfile::tempdir().unwrap();
    let body = pr_read_stub_plugin(
        "[this is not a valid ExternalPRDetails object]",
        "[]",
        "[]",
        "",
    );
    setup_pr_read_stub(data_dir.path(), plugin_root.path(), &body);

    let out = pr_read_cmd(data_dir.path(), plugin_root.path())
        .args(["pr", "view", "--repo", "stub", "--number", "1"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "expected non-zero exit on malformed JSON"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.is_empty(),
        "expected a non-empty error message on malformed plugin output"
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

/// A variant of `legion_cmd` that also overrides the home directory so the
/// usage command reads sessions from the tempdir instead of the real
/// `~/.claude/projects/`.
///
/// `dirs::home_dir()` on Windows uses `SHGetKnownFolderPath(FOLDERID_Profile)`
/// and ignores both `HOME` and `USERPROFILE`, so the usage handler honors a
/// `LEGION_HOME` override that the test sets here.
fn legion_cmd_with_home(data_dir: &std::path::Path, home_dir: &std::path::Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_legion"));
    cmd.env("LEGION_DATA_DIR", data_dir);
    cmd.env("LEGION_HOME", home_dir);
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

// ---------------------------------------------------------------------------
// Quality gate tests
// ---------------------------------------------------------------------------

/// `legion quality-gate record` writes a row and prints a UUIDv7 on stdout.
#[test]
fn quality_gate_record_prints_id() {
    let dir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path())
        .args([
            "quality-gate",
            "record",
            "--skill",
            "legion-simplify",
            "--result",
            "clean",
        ])
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "quality-gate record failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_uuid_format(&id);
}

/// `legion quality-gate record` with findings-count and details-json succeeds.
#[test]
fn quality_gate_record_with_details() {
    let dir = tempfile::tempdir().unwrap();
    let details = r#"{"result":"issues","findings_count":2,"findings":[]}"#;

    let out = legion_cmd(dir.path())
        .args([
            "quality-gate",
            "record",
            "--skill",
            "legion-simplify",
            "--result",
            "issues",
            "--findings-count",
            "2",
            "--details-json",
            details,
        ])
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "quality-gate record with details failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_uuid_format(&id);
}

/// `legion pr create` exits 1 with a clear error when no quality gate exists for HEAD.
///
/// The test uses a repo name that has no watch.toml entry, which means the gate
/// check fires first and the process exits before reaching worksource resolution.
/// This test is not #[ignore] because the gate check requires no work source.
#[test]
fn pr_create_refuses_without_quality_gate() {
    let dir = tempfile::tempdir().unwrap();

    // No gate recorded -- the DB is empty.
    let out = legion_cmd(dir.path())
        .args(["pr", "create", "--repo", "test-repo", "--title", "My PR"])
        .output()
        .unwrap();

    assert!(
        !out.status.success(),
        "pr create should fail without a gate"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no clean legion-simplify gate") || stderr.contains("legion-simplify"),
        "error should mention legion-simplify gate, got: {stderr}"
    );
    assert!(
        stderr.contains("/legion-simplify") || stderr.contains("legion-simplify"),
        "error should guide toward the skill, got: {stderr}"
    );
}

/// `legion pr create --skip-gates` bypasses the quality gate, writes an audit entry,
/// and fails at worksource resolution (not at gate check).
///
/// This confirms --skip-gates exits past the gate. The specific exit message
/// from worksource failure varies by platform, so we only check the gate was not
/// the failure reason.
#[test]
fn pr_create_skip_gates_bypasses_gate_check() {
    let dir = tempfile::tempdir().unwrap();

    // No gate recorded, but --skip-gates should bypass that check.
    let out = legion_cmd(dir.path())
        .args([
            "pr",
            "create",
            "--repo",
            "no-such-repo",
            "--title",
            "Bootstrap PR",
            "--skip-gates",
        ])
        .output()
        .unwrap();

    // Should fail, but NOT with the "no clean legion-simplify gate" message.
    // It fails later because no watch.toml entry exists for "no-such-repo".
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("no clean legion-simplify gate"),
        "skip-gates should bypass gate error, got: {stderr}"
    );
}

/// Verify that `legion mcp` runs as a spec-compliant stdio-only MCP server:
/// no HTTP port bind, no watch loop. Each Claude Code session spawns its own
/// `legion mcp` subprocess via plugin.json mcpServers, so a port bind would
/// conflict across concurrent sessions and a watch loop would spawn recursive
/// agent sessions. The long-lived HTTP + watch process is `legion daemon`,
/// kept as a separate singleton and unrelated to this stdio subprocess.
///
/// This test binds a port first to guarantee that `legion mcp` must skip the
/// HTTP bind entirely (attempting to bind an already-taken port would surface
/// as a startup error).
#[test]
fn legion_mcp_subcommand_is_stdio_only() {
    use std::io::Write;

    let data_dir = tempfile::tempdir().unwrap();

    // Hold a port so that if `legion mcp` ever tries to start an HTTP server,
    // the bind would fail and the subprocess would surface as an error.
    let blocker = std::net::TcpListener::bind("127.0.0.1:0").unwrap();

    let mut child = legion_cmd(data_dir.path())
        .args(["mcp"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn legion mcp");

    // Send a valid MCP initialize request, then close stdin so the stdio loop
    // returns and the process exits cleanly.
    let stdin = child.stdin.as_mut().expect("failed to open child stdin");
    stdin
        .write_all(
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"test\",\"version\":\"1\"}}}\n",
        )
        .expect("failed to write initialize to stdin");
    drop(child.stdin.take());

    let output = child
        .wait_with_output()
        .expect("failed to wait for legion mcp");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "legion mcp exited nonzero\nstatus: {:?}\nstderr: {}",
        output.status,
        stderr
    );

    // Stdout must contain a valid MCP initialize response with the right
    // protocol version, proving the stdio loop actually ran.
    assert!(
        stdout.contains("\"protocolVersion\":\"2024-11-05\""),
        "legion mcp stdout missing initialize response\nstdout: {stdout}"
    );

    // Stderr must NOT mention HTTP server startup or watch loop activity,
    // proving legion mcp is stdio-only and does not start either.
    assert!(
        !stderr.contains("channel server at http://"),
        "legion mcp must not start HTTP server\nstderr: {stderr}"
    );
    assert!(
        !stderr.contains("watch active"),
        "legion mcp must not start watch loop\nstderr: {stderr}"
    );

    // Keep the blocker alive until the assertions complete so the conflict
    // surface stays hot for the duration of the test.
    drop(blocker);
}

/// Verify that setting `LEGION_DATA_DIR` suppresses the plugin-data-dir
/// migration. Without this guard, a test tempdir target would inherit
/// content from the real user's plugin data dir on whatever machine the
/// tests run on -- leaking `~/.claude/plugins/data/legion-legion/` state
/// into fresh-tempdir tests and causing unpredictable failures that
/// depend on the tester's local setup.
///
/// The explicit override signals "I know what I'm doing, stay out of the
/// filesystem." When set, no migration runs regardless of source state.
#[test]
fn data_dir_override_suppresses_migration() {
    let data_dir = tempfile::tempdir().unwrap();

    let output = legion_cmd(data_dir.path())
        .args(["stats"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stats failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("first-run migration"),
        "LEGION_DATA_DIR override must suppress migration chatter\nstderr: {stderr}"
    );
}

/// Test that `legion sync` returns an error when no work source is configured.
/// This verifies the command parses and executes, even when it fails.
#[test]
fn sync_command_errors_without_worksource_config() {
    let data_dir = tempfile::tempdir().unwrap();

    // Run sync for a repo with no watch.toml entry - should fail gracefully
    let output = legion_cmd(data_dir.path())
        .args(["sync", "--repo", "nonexistent-repo"])
        .output()
        .unwrap();

    assert!(!output.status.success(), "sync should fail without config");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no work source configured"),
        "expected 'no work source configured' error, got: {stderr}"
    );
}

/// Verify `legion daemon-spawn` is idempotent: when a live daemon PID is
/// already recorded, a second spawn detects it and prints "already running"
/// instead of forking a duplicate.
///
/// The test writes the current test-process PID into `daemon.pid` before
/// calling `daemon-spawn`. The test process is guaranteed alive for the
/// duration of the test, so `is_process_alive` will return true and the
/// spawn path is correctly skipped. This isolates the idempotency check
/// from the question of whether a real daemon child would survive startup
/// in a CI environment (port conflicts, missing watch.toml, etc.), which
/// is a separate concern and not what this test is guarding.
///
/// Unix-only: `is_process_alive` uses `kill -0` on Unix and returns `false`
/// unconditionally on Windows (there is no portable equivalent), so this
/// idempotency path cannot be exercised on Windows. The entire daemon
/// auto-spawn feature is Unix-targeted anyway -- `setup-binary.sh` is bash
/// and the log paths (`~/Library/Logs`, `$XDG_STATE_HOME`) are Unix-only.
#[cfg(unix)]
#[test]
fn daemon_auto_spawn_is_idempotent() {
    let data_dir = tempfile::tempdir().unwrap();
    let pid_file = data_dir.path().join("daemon.pid");

    // Pre-populate daemon.pid with this test process's own PID. The test
    // process is definitionally alive while this runs, so is_process_alive
    // will return true when daemon-spawn checks it.
    let test_pid = std::process::id();
    std::fs::write(&pid_file, test_pid.to_string()).unwrap();

    let output = legion_cmd(data_dir.path())
        .args(["daemon-spawn"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "daemon-spawn with live PID failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("already running"),
        "expected 'already running' message, got: {stderr}"
    );

    // The PID file must be unchanged -- daemon-spawn should not overwrite
    // a live PID with a fresh spawn.
    let after = std::fs::read_to_string(&pid_file).unwrap();
    assert_eq!(
        after.trim(),
        test_pid.to_string(),
        "daemon-spawn must not overwrite a live PID file"
    );
}

/// Verify `legion daemon-spawn` clears a stale PID file (one whose recorded
/// PID is no longer alive) before attempting a fresh spawn. This is the
/// recovery path for the "daemon crashed, left its PID file behind" case.
///
/// The test writes an unlikely-to-be-in-use PID into `daemon.pid`, then
/// invokes `daemon-spawn`. Because the PID is not alive, the function must
/// remove the stale file and proceed to spawn. We do not assert anything
/// about whether the spawned child survives -- that is a separate concern.
/// We only assert that the stale PID was cleared (file was either removed
/// or overwritten with a different PID) and the command exited zero.
///
/// Unix-only for the same reason as `daemon_auto_spawn_is_idempotent`: the
/// feature uses Unix-only process semantics and shell plumbing, and this
/// test shells out via `kill` for cleanup. See the doc comment on that
/// test for the full rationale.
#[cfg(unix)]
#[test]
fn daemon_auto_spawn_clears_stale_pid() {
    let data_dir = tempfile::tempdir().unwrap();
    let pid_file = data_dir.path().join("daemon.pid");

    // Write a stale PID: use 2^31 - 1, the maximum valid POSIX PID, which is
    // almost never a live process on any real system.
    let stale_pid: u32 = (i32::MAX) as u32;
    std::fs::write(&pid_file, stale_pid.to_string()).unwrap();

    let output = legion_cmd(data_dir.path())
        .args(["daemon-spawn"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "daemon-spawn with stale PID failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The stale PID must be gone. Either the file was removed during the
    // stale-cleanup step, or it was overwritten with a new child PID. Either
    // way, the stale value must not still be there.
    if pid_file.exists() {
        let after = std::fs::read_to_string(&pid_file).unwrap();
        assert_ne!(
            after.trim(),
            stale_pid.to_string(),
            "stale PID must be cleared or replaced"
        );

        // Best-effort cleanup: if a real daemon child was spawned, try to
        // kill it so we do not leave orphans behind after the test.
        if let Ok(pid) = after.trim().parse::<i32>() {
            let _ = std::process::Command::new("kill")
                .arg(pid.to_string())
                .output();
        }
    }
}

/// End-to-end test: spawn `legion mcp` as a subprocess, perform the MCP
/// `initialize` handshake, then fire FOUR separate `legion post` CLI
/// invocations covering every branch of the recipient filter:
///
/// - **MUSING_DELIVERED**: plain text from `sender-repo` -- general musing
///   from a different repo, MUST deliver.
/// - **OWN_POST_SUPPRESSED**: plain text from `recv-repo` (same as
///   `clientInfo.name`) -- own-post suppression, MUST NOT deliver.
/// - **NAMED_SIGNAL_DELIVERED**: `@recv-repo` signal from `sender-repo` --
///   targeted signal to this client, MUST deliver with `is_signal="true"`.
/// - **WRONG_SIGNAL_SUPPRESSED**: `@other-repo` signal from `sender-repo` --
///   targeted signal to a different client, MUST NOT deliver.
///
/// For every delivered frame, the test also parses the wire payload (the
/// `<channel>` XML inside `params.content`) and asserts the `repo`,
/// `is_signal`, and CDATA body are correct -- this locks the wire format so
/// a future refactor of `build_channel_content` cannot silently change the
/// shape of the message Claude Code parses on the other end.
///
/// This test is the primary regression guard for issue #220. Prior to the
/// fix, the MCP notifier thread subscribed to an in-process
/// `tokio::sync::broadcast` channel, which cannot cross process boundaries.
/// Any write made from a separate process -- a `legion post` CLI command,
/// a second Claude Code session's MCP subprocess, the standalone HTTP
/// daemon -- was silently invisible to the notifier. This test exercises
/// exactly that path across every filter branch and must fail if any of
/// them regresses (the prior PR #221 review highlighted that a test
/// covering only the general-musing branch would let a `client_repo_cell`
/// wiring break slip through).
#[test]
fn mcp_push_bridge_delivers_cross_process_post() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().expect("tempdir");

    // Warm the database once before spawning the MCP subprocess. Legion's
    // schema migrations are not concurrency-safe at first-open time: two
    // processes racing to ALTER TABLE on a fresh DB produce "duplicate
    // column name" errors. A single synchronous CLI command drives the
    // full migration path to completion, so subsequent openers see a
    // ready schema.
    let warmup = Command::new(env!("CARGO_BIN_EXE_legion"))
        .env("LEGION_DATA_DIR", dir.path())
        .args(["post", "--repo", "warmup-repo", "--text", "schema warmup"])
        .output()
        .expect("spawn legion post (warmup)");
    assert!(
        warmup.status.success(),
        "warmup post failed: {}",
        String::from_utf8_lossy(&warmup.stderr)
    );

    // Spawn the MCP subprocess with a tight poll interval so the test
    // finishes quickly instead of waiting on the 500ms default.
    let mut child = Command::new(env!("CARGO_BIN_EXE_legion"))
        .env("LEGION_DATA_DIR", dir.path())
        .env("LEGION_MCP_POLL_MS", "50")
        .args(["mcp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn legion mcp");

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let child_stderr = child.stderr.take().expect("stderr");
    let mut reader = BufReader::new(stdout);

    // Drain subprocess stderr in a background thread. If the notifier spams
    // errors (DB failure, etc.) and fills the stderr pipe, the child can
    // block on `eprintln!`, which interacts badly with the shared stdout
    // mutex. Draining stderr prevents that and captures the lines so the
    // failure message can include them.
    let captured_stderr = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    {
        let captured = std::sync::Arc::clone(&captured_stderr);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(child_stderr);
            let mut line = String::new();
            while let Ok(n) = reader.read_line(&mut line) {
                if n == 0 {
                    break;
                }
                if let Ok(mut s) = captured.lock() {
                    s.push_str(&line);
                }
                line.clear();
            }
        });
    }

    // 1. Send initialize. `clientInfo.name = "recv-repo"` is load-bearing:
    //    without it, the notifier cannot suppress own-posts or route
    //    named signals.
    let init = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "recv-repo", "version": "0.0.1" }
        }
    });
    writeln!(stdin, "{}", serde_json::to_string(&init).unwrap()).expect("write initialize");
    stdin.flush().expect("flush");

    // 2. Read the initialize response.
    let mut init_line = String::new();
    reader
        .read_line(&mut init_line)
        .expect("read initialize response");
    let init_resp: serde_json::Value =
        serde_json::from_str(init_line.trim()).expect("parse initialize response");
    assert_eq!(init_resp["id"], 1, "initialize response id mismatch");
    assert_eq!(
        init_resp["result"]["serverInfo"]["name"], "legion-channel",
        "wrong server name"
    );

    // 3. Fire four cross-process posts covering every filter branch. The
    //    markers are unique per-case so the assertions can distinguish
    //    which frames arrived without parsing text content.
    let musing_marker = "MCP_PUSH_MUSING_DELIVERED_9f2a1b";
    let own_post_marker = "MCP_PUSH_OWN_POST_SUPPRESSED_9f2a1b";
    let named_signal_marker = "MCP_PUSH_NAMED_SIGNAL_DELIVERED_9f2a1b";
    let wrong_signal_marker = "MCP_PUSH_WRONG_SIGNAL_SUPPRESSED_9f2a1b";

    // Order matters: fire the "must not deliver" posts FIRST so that when
    // the later "must deliver" posts arrive, we know the prior ones have
    // already been polled and filtered. If MUSING_DELIVERED arrives and
    // OWN_POST_SUPPRESSED is not in the observed set by then, we can
    // conclude the notifier's filter actively suppressed it, not just
    // that it had not been polled yet.
    let posts = [
        ("recv-repo", own_post_marker.to_string()),
        (
            "sender-repo",
            format!("@other-repo review:approved -- {}", wrong_signal_marker),
        ),
        ("sender-repo", musing_marker.to_string()),
        (
            "sender-repo",
            format!("@recv-repo review:approved -- {}", named_signal_marker),
        ),
    ];

    for (repo, text) in &posts {
        let post_out = Command::new(env!("CARGO_BIN_EXE_legion"))
            .env("LEGION_DATA_DIR", dir.path())
            .args(["post", "--repo", repo, "--text", text])
            .output()
            .expect("spawn legion post");
        assert!(
            post_out.status.success(),
            "legion post failed ({}): {}",
            repo,
            String::from_utf8_lossy(&post_out.stderr)
        );
    }

    // 4. Drain subprocess stdout until BOTH deliverable markers have been
    //    seen OR the deadline expires. Each line is captured regardless
    //    of whether it matches.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut observed_lines: Vec<String> = Vec::new();
    let mut observed_frames: Vec<serde_json::Value> = Vec::new();

    // Read in a dedicated thread so we can enforce the deadline via
    // channel recv_timeout instead of blocking forever on a dead pipe.
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut reader = reader;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let mut musing_frame: Option<serde_json::Value> = None;
    let mut signal_frame: Option<serde_json::Value> = None;

    while Instant::now() < deadline && (musing_frame.is_none() || signal_frame.is_none()) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match rx.recv_timeout(remaining) {
            Ok(line) => {
                observed_lines.push(line.clone());
                let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
                    continue;
                };
                if v["method"] != "notifications/claude/channel" {
                    continue;
                }
                observed_frames.push(v.clone());
                let content = v["params"]["content"].as_str().unwrap_or("");
                if content.contains(musing_marker) {
                    musing_frame = Some(v);
                } else if content.contains(named_signal_marker) {
                    signal_frame = Some(v);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    // Always kill the subprocess and collect captured stderr before
    // asserting so a failure does not leave a zombie daemon behind and so
    // the failure message is diagnosable.
    let _ = child.kill();
    let _ = child.wait();
    let stderr_snapshot = captured_stderr
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default();

    let failure_context = || {
        format!(
            "observed {} frames:\n{}\ncaptured stderr:\n{}",
            observed_frames.len(),
            observed_lines.join(""),
            stderr_snapshot
        )
    };

    // Positive assertion 1: general-musing branch delivered.
    let musing = musing_frame.unwrap_or_else(|| {
        panic!(
            "did not observe musing notification carrying {}; {}",
            musing_marker,
            failure_context()
        )
    });
    let musing_content = musing["params"]["content"].as_str().expect("content str");
    assert!(
        musing_content.contains(r#"repo="sender-repo""#),
        "musing frame wire repo attribute wrong: {musing_content}"
    );
    assert!(
        musing_content.contains(r#"is_signal="false""#),
        "musing frame is_signal attribute wrong: {musing_content}"
    );
    assert!(
        musing_content.contains(&format!("<![CDATA[{musing_marker}]]>")),
        "musing frame CDATA body does not match marker: {musing_content}"
    );

    // Positive assertion 2: @recv-repo named-signal branch delivered.
    let signal = signal_frame.unwrap_or_else(|| {
        panic!(
            "did not observe named-signal notification carrying {}; {}",
            named_signal_marker,
            failure_context()
        )
    });
    let signal_content = signal["params"]["content"].as_str().expect("content str");
    assert!(
        signal_content.contains(r#"repo="sender-repo""#),
        "signal frame wire repo attribute wrong: {signal_content}"
    );
    assert!(
        signal_content.contains(r#"is_signal="true""#),
        "signal frame is_signal attribute wrong: {signal_content}"
    );
    assert!(
        signal_content.contains(named_signal_marker),
        "signal frame CDATA body does not match marker: {signal_content}"
    );

    // Negative assertion 1: own-post (recv-repo → recv-repo) was suppressed.
    // Both deliverable frames have arrived by this point, so any intervening
    // polls that would have delivered OWN_POST_SUPPRESSED have already run.
    for frame in &observed_frames {
        let content = frame["params"]["content"].as_str().unwrap_or("");
        assert!(
            !content.contains(own_post_marker),
            "own-post suppression regression: frame carrying {own_post_marker} was delivered; {content}"
        );
    }

    // Negative assertion 2: wrong-recipient signal (@other-repo) was suppressed.
    for frame in &observed_frames {
        let content = frame["params"]["content"].as_str().unwrap_or("");
        assert!(
            !content.contains(wrong_signal_marker),
            "wrong-recipient signal suppression regression: frame carrying {wrong_signal_marker} was delivered; {content}"
        );
    }
}

// -- legion watch add / remove / list -----------------------------------------

#[test]
fn watch_add_creates_entry() {
    let dir = tempfile::tempdir().unwrap();
    let workdir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path())
        .args([
            "watch",
            "add",
            "--name",
            "rafters",
            workdir.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "watch add failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("added:"),
        "expected 'added:' in output: {stdout}"
    );
    assert!(
        stdout.contains("rafters"),
        "expected name in output: {stdout}"
    );
}

#[test]
fn watch_add_derives_name_from_path_basename() {
    let dir = tempfile::tempdir().unwrap();
    let workdir_parent = tempfile::tempdir().unwrap();
    let workdir = workdir_parent.path().join("my-project");
    std::fs::create_dir(&workdir).unwrap();

    let out = legion_cmd(dir.path())
        .args(["watch", "add", workdir.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "watch add (no --name) failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("my-project"),
        "expected derived name 'my-project' in output: {stdout}"
    );

    let listed = legion_cmd(dir.path())
        .args(["watch", "list"])
        .output()
        .unwrap();
    let listed_stdout = String::from_utf8_lossy(&listed.stdout);
    assert!(
        listed_stdout.contains("my-project"),
        "expected 'my-project' in list output: {listed_stdout}"
    );
}

#[test]
fn watch_add_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let workdir = tempfile::tempdir().unwrap();

    let first = legion_cmd(dir.path())
        .args([
            "watch",
            "add",
            "--name",
            "rafters",
            workdir.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(first.status.success());

    let second = legion_cmd(dir.path())
        .args([
            "watch",
            "add",
            "--name",
            "rafters",
            workdir.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(second.status.success());
    let stdout = String::from_utf8_lossy(&second.stdout);
    assert!(
        stdout.contains("already present"),
        "expected 'already present' for duplicate: {stdout}"
    );
}

#[test]
fn watch_add_with_agent_flag() {
    let dir = tempfile::tempdir().unwrap();
    let workdir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path())
        .args([
            "watch",
            "add",
            "--name",
            "legion",
            "--agent",
            "my-agent",
            workdir.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "watch add --agent failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("my-agent"),
        "expected agent name in output: {stdout}"
    );
}

#[test]
fn watch_list_shows_entries() {
    let dir = tempfile::tempdir().unwrap();
    let workdir = tempfile::tempdir().unwrap();

    // Empty list first.
    let empty = legion_cmd(dir.path())
        .args(["watch", "list"])
        .output()
        .unwrap();
    assert!(empty.status.success());
    let stdout = String::from_utf8_lossy(&empty.stdout);
    assert!(
        stdout.contains("no repos"),
        "empty list should say 'no repos': {stdout}"
    );

    // Add one entry.
    legion_cmd(dir.path())
        .args([
            "watch",
            "add",
            "--name",
            "rafters",
            workdir.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();

    let list = legion_cmd(dir.path())
        .args(["watch", "list"])
        .output()
        .unwrap();
    assert!(list.status.success());
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(
        stdout.contains("rafters"),
        "list should show 'rafters': {stdout}"
    );
}

#[test]
fn watch_remove_removes_entry() {
    let dir = tempfile::tempdir().unwrap();
    let workdir = tempfile::tempdir().unwrap();

    legion_cmd(dir.path())
        .args([
            "watch",
            "add",
            "--name",
            "rafters",
            workdir.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();

    let out = legion_cmd(dir.path())
        .args(["watch", "remove", "rafters"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "watch remove failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("removed:"),
        "expected 'removed:' in output: {stdout}"
    );

    // Verify it is gone from list.
    let list = legion_cmd(dir.path())
        .args(["watch", "list"])
        .output()
        .unwrap();
    let list_out = String::from_utf8_lossy(&list.stdout);
    assert!(
        !list_out.contains("rafters"),
        "entry should be gone after remove: {list_out}"
    );
}

#[test]
fn watch_remove_missing_entry_reports_not_found() {
    let dir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path())
        .args(["watch", "remove", "nonexistent"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("not found"),
        "expected 'not found' for unknown name: {stdout}"
    );
}

#[test]
fn watch_add_rejects_nonexistent_workdir() {
    let dir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path())
        .args([
            "watch",
            "add",
            "--name",
            "test",
            "/nonexistent/path/that/does/not/exist",
        ])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "watch add should fail for non-directory path"
    );
}

#[test]
fn mesh_headroom_on_empty_store_notices_no_samples() {
    let dir = tempfile::tempdir().unwrap();
    let out = legion_cmd(dir.path())
        .args(["mesh", "headroom"])
        .output()
        .unwrap();
    assert!(out.status.success(), "headroom on empty store exits 0");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no samples yet"),
        "empty-store notice expected, got stderr: {stderr}"
    );
}

#[test]
fn mesh_headroom_json_on_empty_store_returns_array() {
    let dir = tempfile::tempdir().unwrap();
    let out = legion_cmd(dir.path())
        .args(["mesh", "headroom", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert!(parsed.is_array(), "expected JSON array, got {parsed}");
    assert_eq!(parsed.as_array().unwrap().len(), 0);
}

#[test]
fn mesh_pick_on_empty_store_exits_nonzero() {
    let dir = tempfile::tempdir().unwrap();
    let out = legion_cmd(dir.path())
        .args(["mesh", "pick"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "pick must fail when no fresh host exists"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no fresh host"),
        "error message must name the condition, got: {stderr}"
    );
    assert!(
        !stderr.contains("WorkSource"),
        "error must not surface as a WorkSource variant -- operators grep that token for real plugin failures, got: {stderr}"
    );
}

/// Seed a single rate-limit sample by invoking `legion statusline` with a
/// minimal synthetic Claude Code JSON payload. Returns the path to a
/// transcript file we do NOT create -- statusline tolerates a missing
/// transcript (skips usage sample) and still writes the rate-limit row.
fn seed_rate_limit_sample(data_dir: &std::path::Path, five_hour_pct: f64, seven_day_pct: f64) {
    let session_id = format!("seed-{}", uuid::Uuid::now_v7());
    let payload = serde_json::json!({
        "session_id": session_id,
        "rate_limits": {
            "five_hour": { "used_percentage": five_hour_pct, "resets_at": 0 },
            "seven_day": { "used_percentage": seven_day_pct, "resets_at": 0 },
        },
    });
    let mut child = legion_cmd(data_dir)
        .args(["statusline"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "statusline seed failed");
}

#[test]
fn mesh_headroom_json_shape_pins_field_names() {
    let dir = tempfile::tempdir().unwrap();
    seed_rate_limit_sample(dir.path(), 40.0, 55.0);

    let out = legion_cmd(dir.path())
        .args(["mesh", "headroom", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "headroom failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let rows = parsed.as_array().expect("expected array");
    assert!(
        !rows.is_empty(),
        "seed should produce at least one host row"
    );
    let row = &rows[0];
    // Contract: downstream schedulers / hooks will key on these names.
    // A typo or camelCase flip is a silent breakage otherwise.
    for field in [
        "hostname",
        "score",
        "fiveHourPct",
        "sevenDayPct",
        "lastEffectiveTokens",
        "sampledAt",
        "ageSecs",
        "stale",
    ] {
        assert!(
            row.get(field).is_some(),
            "missing field '{field}' in headroom JSON: {row}"
        );
    }
}

#[test]
fn mesh_pick_json_shape_pins_field_names() {
    let dir = tempfile::tempdir().unwrap();
    seed_rate_limit_sample(dir.path(), 40.0, 55.0);

    let out = legion_cmd(dir.path())
        .args(["mesh", "pick", "--json", "--for-task", "card-xyz"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "pick --json failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    for field in ["hostname", "score", "forTask"] {
        assert!(
            parsed.get(field).is_some(),
            "missing field '{field}' in pick JSON: {parsed}"
        );
    }
    assert_eq!(
        parsed["forTask"].as_str(),
        Some("card-xyz"),
        "--for-task must round-trip into the JSON payload"
    );
}

#[test]
fn mesh_pick_exclude_removes_named_host() {
    let dir = tempfile::tempdir().unwrap();
    seed_rate_limit_sample(dir.path(), 10.0, 10.0);
    // Figure out the host's name from headroom output.
    let head = legion_cmd(dir.path())
        .args(["mesh", "headroom", "--json"])
        .output()
        .unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&head.stdout)).unwrap();
    let host = parsed[0]["hostname"].as_str().unwrap().to_string();

    // Excluding the only fresh host must exit 1 with "no fresh host".
    let out = legion_cmd(dir.path())
        .args(["mesh", "pick", "--exclude", &host])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "pick must fail when every fresh host is excluded"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no fresh host"),
        "error must name condition, got: {stderr}"
    );
}

#[test]
fn mesh_stale_cutoff_env_override_is_respected() {
    // Default cutoff is 600s. Seed a sample, sleep so the sample is
    // definitely older than 3 seconds, then run pick with a 3-second
    // cutoff. Sample must register as stale; pick must fail.
    let dir = tempfile::tempdir().unwrap();
    seed_rate_limit_sample(dir.path(), 40.0, 55.0);
    std::thread::sleep(std::time::Duration::from_secs(4));

    let out = legion_cmd(dir.path())
        .env("LEGION_MESH_STALE_SECS", "3")
        .args(["mesh", "pick"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "with cutoff=3s the >=4s-old sample is stale; pick must fail. stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no fresh host"),
        "stale env-override path must surface the same no-fresh-host error, got: {stderr}"
    );
}

#[test]
fn pending_replies_emits_wake_prompt_for_request_signals() {
    let dir = tempfile::tempdir().unwrap();

    // smugglr asks platform to review an RFC. Under #404 the wake/reply gate
    // is verb-only, so the right shape is `--verb request` (not the pre-#404
    // `--verb review --status request` workaround).
    let out = legion_cmd(dir.path())
        .args([
            "signal",
            "--repo",
            "smugglr",
            "--to",
            "platform",
            "--verb",
            "request",
            "--note",
            "RFC review at vault-2026/projects/smuggler/fence/rfc.md",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());

    // legion announces shipping -- informational, must NOT trip pending-replies.
    let out = legion_cmd(dir.path())
        .args([
            "signal",
            "--repo",
            "legion",
            "--to",
            "platform",
            "--verb",
            "announce",
            "--note",
            "v0.9.5 shipped",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());

    let out = legion_cmd(dir.path())
        .args(["pending-replies", "--repo", "platform"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "pending-replies failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("REQUIRES A REPLY"),
        "verb=request must surface as reply-required, got: {stdout}"
    );
    assert!(
        stdout.contains("RFC review"),
        "expected the signal note in the prompt, got: {stdout}"
    );
    assert!(
        !stdout.contains("v0.9.5 shipped"),
        "informational announce must not appear in pending-replies, got: {stdout}"
    );
}

#[test]
fn pending_replies_silent_when_nothing_pending() {
    let dir = tempfile::tempdir().unwrap();

    let out = legion_cmd(dir.path())
        .args([
            "signal", "--repo", "legion", "--to", "platform", "--verb", "announce", "--note", "fyi",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());

    let out = legion_cmd(dir.path())
        .args(["pending-replies", "--repo", "platform"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        out.stdout.is_empty(),
        "pending-replies should print nothing when no reply-required signals exist, got: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}
