//! Integration tests: reflect / recall / whoami / forget / stats / consult / boost / chain / similar / dedupe.

use crate::common::*;

#[test]
fn reflect_and_recall_roundtrip() {
    let dir = tempfile::tempdir().unwrap();

    run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "test",
        "--text",
        "arrays are tricky in codegen",
    ]));

    let stdout = run_ok(legion_cmd(dir.path()).args([
        "recall",
        "--repo",
        "test",
        "--context",
        "codegen arrays",
    ]));
    assert!(
        stdout.contains("arrays are tricky"),
        "expected reflection in output, got: {stdout}"
    );
}

#[test]
fn recall_by_domain_filters_correctly() {
    let dir = tempfile::tempdir().unwrap();

    // Create two reflections: one with domain "identity", one with domain "checkpoint"
    run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "test",
        "--text",
        "I am the test agent",
        "--domain",
        "identity",
    ]));

    run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "test",
        "--text",
        "checkpoint before compact",
        "--domain",
        "checkpoint",
    ]));

    // Also create one without a domain
    run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "test",
        "--text",
        "generic reflection no domain",
    ]));

    // Recall with --domain identity should return only the identity reflection
    let stdout = run_ok(legion_cmd(dir.path()).args([
        "recall", "--repo", "test", "--domain", "identity", "--limit", "5",
    ]));
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
    let stdout = run_ok(legion_cmd(dir.path()).args([
        "recall",
        "--repo",
        "test",
        "--domain",
        "checkpoint",
        "--limit",
        "5",
    ]));
    assert!(
        stdout.contains("checkpoint before compact"),
        "expected checkpoint reflection, got: {stdout}"
    );
    assert!(
        !stdout.contains("I am the test agent"),
        "should not contain identity reflection: {stdout}"
    );

    // Recall with --domain nonexistent should return nothing
    let stdout = run_ok(legion_cmd(dir.path()).args([
        "recall",
        "--repo",
        "test",
        "--domain",
        "nonexistent",
        "--limit",
        "5",
    ]));
    assert!(
        stdout.trim().is_empty(),
        "expected empty output for nonexistent domain, got: {stdout}"
    );
}

#[test]
fn whoami_flag_stores_as_identity_domain() {
    let dir = tempfile::tempdir().unwrap();

    run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "test",
        "--whoami",
        "--text",
        "I am the test agent",
    ]));

    let stdout = run_ok(legion_cmd(dir.path()).args([
        "recall", "--repo", "test", "--domain", "identity", "--limit", "5",
    ]));
    assert!(
        stdout.contains("I am the test agent"),
        "expected whoami reflection under domain=identity, got: {stdout}"
    );
}

#[test]
fn whoami_conflicts_with_domain_flag() {
    let dir = tempfile::tempdir().unwrap();

    let (_stdout, stderr) = run_fail(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "test",
        "--whoami",
        "--domain",
        "something-else",
        "--text",
        "should not store",
    ]));
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

    run_ok(
        legion_cmd(dir.path())
            .args(["reflect", "--repo", "test", "--whoami", "--transcript"])
            .arg(&transcript),
    );

    let stdout = run_ok(legion_cmd(dir.path()).args([
        "recall", "--repo", "test", "--domain", "identity", "--limit", "5",
    ]));
    assert!(
        stdout.contains("I am the test agent from transcript"),
        "expected whoami reflection from transcript under domain=identity, got: {stdout}"
    );
}

#[test]
fn whoami_works_with_compound_repo() {
    let dir = tempfile::tempdir().unwrap();

    run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "alpha,beta",
        "--whoami",
        "--text",
        "shared identity text",
    ]));

    for repo in ["alpha", "beta"] {
        let stdout = run_ok(legion_cmd(dir.path()).args([
            "recall", "--repo", repo, "--domain", "identity", "--limit", "5",
        ]));
        assert!(
            stdout.contains("shared identity text"),
            "expected identity reflection in {repo}, got: {stdout}"
        );
    }
}

#[test]
fn whoami_subcommand_returns_identity_reflections() {
    let dir = tempfile::tempdir().unwrap();

    run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "test",
        "--whoami",
        "--text",
        "I am the test agent",
    ]));

    let stdout = run_ok(legion_cmd(dir.path()).args(["whoami", "--repo", "test"]));
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

    let stdout = run_ok(legion_cmd(dir.path()).args(["whoami", "--repo", "empty"]));
    assert!(stdout.is_empty(), "expected empty output, got: {stdout}");
}

#[test]
fn whoami_subcommand_requires_repo() {
    let dir = tempfile::tempdir().unwrap();

    run_fail(legion_cmd(dir.path()).args(["whoami"]));
}

#[test]
fn whoami_subcommand_isolates_by_repo() {
    let dir = tempfile::tempdir().unwrap();

    for (repo, text) in [("alpha", "alpha identity"), ("beta", "beta identity")] {
        run_ok(
            legion_cmd(dir.path()).args(["reflect", "--repo", repo, "--whoami", "--text", text]),
        );
    }

    let stdout = run_ok(legion_cmd(dir.path()).args(["whoami", "--repo", "alpha"]));
    assert!(stdout.contains("alpha identity"));
    assert!(
        !stdout.contains("beta identity"),
        "whoami leaked beta identity into alpha repo: {stdout}"
    );
}

#[test]
fn whoami_subcommand_filters_to_identity_domain() {
    let dir = tempfile::tempdir().unwrap();

    run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "test",
        "--whoami",
        "--text",
        "I am identity",
    ]));

    run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "test",
        "--text",
        "plain non-identity reflection",
    ]));

    let stdout = run_ok(legion_cmd(dir.path()).args(["whoami", "--repo", "test"]));
    assert!(stdout.contains("I am identity"));
    assert!(
        !stdout.contains("plain non-identity reflection"),
        "whoami included non-identity reflection: {stdout}"
    );
}

#[test]
fn whoami_subcommand_respects_limit() {
    // Regression coverage for the `--limit` flag on the shared
    // `format_capped_banner` path that `whoami` and `whatami` both use.
    // Cannot exercise this through `whoami` itself anymore: since #785,
    // domain=identity allows at most one live (parent_id-less) root per
    // repo, so a second or third `--whoami` write is refused before it
    // ever reaches this rendering path. `whatami` (domain=workflow) shares
    // the exact same `get_domain_roots` / `format_capped_banner` code and
    // is not identity-guarded, so it proves `--limit` still caps multiple
    // root rows correctly.
    let dir = tempfile::tempdir().unwrap();

    for i in 0..3 {
        let text = format!("workflow reflection {i}");
        run_ok(legion_cmd(dir.path()).args([
            "reflect", "--repo", "test", "--domain", "workflow", "--text", &text,
        ]));
    }

    let stdout = run_ok(legion_cmd(dir.path()).args(["whatami", "--repo", "test", "--limit", "1"]));
    let bullet_count = stdout.lines().filter(|l| l.starts_with("- ")).count();
    assert_eq!(
        bullet_count, 1,
        "expected 1 bullet, got {bullet_count}: {stdout}"
    );
}

#[test]
fn whoami_subcommand_emits_banner_and_chain_pointer() {
    // Pre-#785 this test also seeded a second, standalone (unchained)
    // identity root in the same repo to prove the chain pointer appears
    // only next to the chained one. Since #785, a repo can have at most
    // one live identity root, so that second root can no longer be
    // created via the CLI at all -- the mixed chained/unchained-entries
    // rendering case is unit-tested directly against `format_whoami` in
    // `src/recall.rs` (`format_whoami_worst_case_size_with_drops_and_first_entry_borrow`
    // and friends), which does not go through the DB and is unaffected by
    // this guard. This test now only proves the end-to-end wiring
    // (`whoami` -> `get_identity_roots` -> `is_in_chain` -> `format_whoami`)
    // for the single live root that can exist.
    let dir = tempfile::tempdir().unwrap();

    // Chain head: identity reflection plus a follow-up that links via --follows.
    let head_id = run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "test",
        "--whoami",
        "--text",
        "chained identity",
    ]))
    .trim()
    .to_owned();

    run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "test",
        "--text",
        "follow-up reflection",
        "--follows",
        &head_id,
    ]));

    let stdout = run_ok(legion_cmd(dir.path()).args(["whoami", "--repo", "test"]));

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
        "expected exactly one chain pointer (the only root, which is chained), got {chain_lines}: {stdout}"
    );
}

// --- #785: DB-layer identity-root insert guard ---

#[test]
fn reflect_whoami_second_root_rejected() {
    let dir = tempfile::tempdir().unwrap();

    let first_id = run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "test",
        "--whoami",
        "--text",
        "first identity",
    ]))
    .trim()
    .to_owned();

    let (_stdout, stderr) = run_fail(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "test",
        "--whoami",
        "--text",
        "second identity",
    ]));
    assert!(
        stderr.contains(&first_id),
        "expected the existing root's id in stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("--follows"),
        "expected --follows guidance in stderr, got: {stderr}"
    );

    // The rejected write must not have replaced the root.
    let stdout = run_ok(legion_cmd(dir.path()).args(["whoami", "--repo", "test"]));
    assert!(stdout.contains("first identity"));
    assert!(!stdout.contains("second identity"));
}

#[test]
fn reflect_domain_identity_second_root_rejected() {
    // Same guard, exercised via the raw --domain identity spelling instead
    // of --whoami, proving both CLI spellings hit the one DB-layer guard.
    let dir = tempfile::tempdir().unwrap();

    let first_id = run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "test",
        "--domain",
        "identity",
        "--text",
        "first identity",
    ]))
    .trim()
    .to_owned();

    let (_stdout, stderr) = run_fail(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "test",
        "--domain",
        "identity",
        "--text",
        "second identity",
    ]));
    assert!(
        stderr.contains(&first_id),
        "expected the existing root's id in stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("--follows"),
        "expected --follows guidance in stderr, got: {stderr}"
    );
}

#[test]
fn reflect_whoami_follows_existing_root_still_succeeds() {
    let dir = tempfile::tempdir().unwrap();

    let root_id = run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "test",
        "--whoami",
        "--text",
        "root identity",
    ]))
    .trim()
    .to_owned();

    run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "test",
        "--whoami",
        "--follows",
        &root_id,
        "--text",
        "supporting detail",
    ]));

    let stdout = run_ok(legion_cmd(dir.path()).args(["whoami", "--repo", "test"]));
    assert!(stdout.contains("root identity"));
    assert!(
        stdout.contains(&format!("legion chain --id {root_id}")),
        "expected a chain pointer once a --follows child exists, got: {stdout}"
    );
}

#[test]
fn reflect_whoami_force_does_not_bypass_second_root_guard() {
    // Regression test for the hypothesized historical bypass: the deleted
    // pre-whoami-rewrite hook's own comment claimed --force alone safely
    // "replaces" identity wholesale. It never did at the DB layer (--force
    // only ever skipped the near-duplicate embedding check), and the guard
    // introduced here does not look at --force at all.
    let dir = tempfile::tempdir().unwrap();

    run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "test",
        "--whoami",
        "--text",
        "root identity",
    ]));

    let (_stdout, stderr) = run_fail(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "test",
        "--whoami",
        "--force",
        "--text",
        "attempted rewrite",
    ]));
    assert!(
        stderr.contains("already has a live identity root"),
        "expected the identity-root guard error, got: {stderr}"
    );

    let stdout = run_ok(legion_cmd(dir.path()).args(["whoami", "--repo", "test"]));
    assert!(stdout.contains("root identity"));
    assert!(!stdout.contains("attempted rewrite"));
}

#[test]
fn reflect_forget_then_reflect_whoami_replaces_root() {
    let dir = tempfile::tempdir().unwrap();

    let root_id = run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "test",
        "--whoami",
        "--text",
        "old identity",
    ]))
    .trim()
    .to_owned();

    run_ok(legion_cmd(dir.path()).args(["forget", "--id", &root_id, "--repo", "test"]));

    run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "test",
        "--whoami",
        "--text",
        "new identity",
    ]));

    let stdout = run_ok(legion_cmd(dir.path()).args(["whoami", "--repo", "test"]));
    assert!(stdout.contains("new identity"));
    assert!(!stdout.contains("old identity"));
}

#[test]
fn reflect_checkpoint_shaped_text_into_identity_second_root_rejected() {
    // The exact leaked-content shape from the bug report (a
    // [CHECKPOINT]-shaped reflection landing in domain=identity) is now
    // structurally blocked regardless of wording, once a root exists.
    let dir = tempfile::tempdir().unwrap();

    run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "test",
        "--whoami",
        "--text",
        "real identity",
    ]));

    let checkpoint_text =
        "[CHECKPOINT]\nActive: refactoring the sync layer\nNext: land the migration";
    let (_stdout, stderr) = run_fail(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "test",
        "--domain",
        "identity",
        "--text",
        checkpoint_text,
    ]));
    assert!(
        stderr.contains("already has a live identity root"),
        "expected the identity-root guard error, got: {stderr}"
    );

    let stdout = run_ok(legion_cmd(dir.path()).args(["whoami", "--repo", "test"]));
    assert!(stdout.contains("real identity"));
    assert!(!stdout.contains("CHECKPOINT"));
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
    let id = run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "kelex",
        "--text",
        "doomed reflection about mapping rules",
    ]))
    .trim()
    .to_string();
    assert_uuid_format(&id);

    // Forget with the WRONG --repo must refuse the delete and exit nonzero.
    let (_stdout, stderr) =
        run_fail(legion_cmd(dir.path()).args(["forget", "--id", &id, "--repo", "rafters"]));
    assert!(
        stderr.contains("repo safety check failed"),
        "expected safety check error, got: {stderr}"
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
    let stdout = run_ok(legion_cmd(dir.path()).args([
        "recall",
        "--repo",
        "kelex",
        "--context",
        "mapping rules",
    ]));
    assert!(
        stdout.contains("doomed reflection"),
        "reflection should still be intact after rejected forget, got: {stdout}"
    );

    // Rejected forget attempts must be traceable in the audit log.
    // Destructive-command rejections are forensically relevant.
    let audit_stdout =
        run_ok(legion_cmd(dir.path()).args(["audit", "--action", "delete-reflection", "--json"]));
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
    let stdout = run_ok(legion_cmd(dir.path()).args(["forget", "--id", &id, "--repo", "kelex"]));
    assert!(
        stdout.contains("forgot reflection"),
        "expected forget confirmation, got: {stdout}"
    );

    // The successful delete also produces an audit entry with outcome=success.
    let audit_stdout =
        run_ok(legion_cmd(dir.path()).args(["audit", "--action", "delete-reflection", "--json"]));
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
    let stderr = run_ok_stderr(legion_cmd(dir.path()).args([
        "--verbose",
        "post",
        "--repo",
        "test",
        "--text",
        "verbose post",
    ]));
    assert!(
        stderr.contains("posting"),
        "expected verbose confirmation, got: {stderr}"
    );
}

#[test]
fn stats_on_empty_db() {
    let dir = tempfile::tempdir().unwrap();

    let stdout = run_ok(legion_cmd(dir.path()).args(["stats"]));
    assert!(
        stdout.contains("no reflections"),
        "expected empty message, got: {stdout}"
    );
}

#[test]
fn reflect_no_input_errors() {
    let dir = tempfile::tempdir().unwrap();

    // clap allows the call but the binary returns an error since
    // neither --text nor --transcript is provided.
    let (_stdout, stderr) = run_fail(legion_cmd(dir.path()).args(["reflect", "--repo", "test"]));
    assert!(
        stderr.contains("no reflection text provided"),
        "expected missing input error, got: {stderr}"
    );
}

#[test]
fn stats_after_reflections() {
    let dir = tempfile::tempdir().unwrap();

    run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "kelex",
        "--text",
        "first reflection",
    ]));
    run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "kelex",
        "--text",
        "second reflection",
    ]));
    run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "rafters",
        "--text",
        "rafters reflection",
    ]));

    let stdout = run_ok(legion_cmd(dir.path()).args(["stats"]));
    assert!(stdout.contains("kelex"), "should show kelex stats");
    assert!(stdout.contains("rafters"), "should show rafters stats");
}

#[test]
fn recall_with_no_matches() {
    let dir = tempfile::tempdir().unwrap();

    run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "test",
        "--text",
        "rust ownership rules",
    ]));

    // Should succeed but return empty since repo does not match
    let stdout = run_ok(legion_cmd(dir.path()).args([
        "recall",
        "--repo",
        "other-repo",
        "--context",
        "rust ownership",
    ]));
    assert!(
        !stdout.contains("rust ownership"),
        "should not find results in different repo"
    );
}

#[test]
fn data_dir_is_created_automatically() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("deep").join("nested").join("dir");

    run_ok(legion_cmd(&nested).args(["stats"]));
    assert!(nested.exists(), "data dir should have been created");
}

#[test]
fn consult_across_repos() {
    let dir = tempfile::tempdir().unwrap();

    // Reflect into two different repos
    run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "kelex",
        "--text",
        "Zod schema mapping is fragile",
    ]));

    run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "platform",
        "--text",
        "Zod validation at the edge works well",
    ]));

    // Consult across all repos
    let stdout =
        run_ok(legion_cmd(dir.path()).args(["consult", "--context", "Zod", "--limit", "10"]));
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

    let stdout = run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "platform,legion",
        "--text",
        "compound test reflection",
    ]));
    let ids: Vec<&str> = stdout.lines().collect();
    assert_eq!(ids.len(), 2, "expected 2 IDs on stdout, got: {stdout}");

    // Verify both repos have the reflection via recall
    let stdout = run_ok(legion_cmd(dir.path()).args([
        "recall",
        "--repo",
        "platform",
        "--context",
        "compound test",
    ]));
    assert!(
        stdout.contains("compound test reflection"),
        "expected reflection in platform recall, got: {stdout}"
    );

    let stdout = run_ok(legion_cmd(dir.path()).args([
        "recall",
        "--repo",
        "legion",
        "--context",
        "compound test",
    ]));
    assert!(
        stdout.contains("compound test reflection"),
        "expected reflection in legion recall, got: {stdout}"
    );
}

#[test]
fn cli_single_repo_still_works() {
    let dir = tempfile::tempdir().unwrap();

    let stdout = run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "platform",
        "--text",
        "single repo test",
    ]));
    assert!(
        !stdout.trim().is_empty(),
        "expected ID on stdout, got nothing"
    );
}

#[test]
fn reindex_rebuilds_from_database() {
    let dir = tempfile::tempdir().unwrap();

    // Create some reflections
    run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "test",
        "--text",
        "reindex test reflection about search",
    ]));

    // Run reindex
    let stderr = run_ok_stderr(legion_cmd(dir.path()).args(["--verbose", "reindex"]));
    assert!(
        stderr.contains("reindexed 1 reflections"),
        "expected reindex count, got: {stderr}"
    );

    // Verify search still works after reindex
    let stdout =
        run_ok(legion_cmd(dir.path()).args(["recall", "--repo", "test", "--context", "search"]));
    assert!(
        stdout.contains("reindex test reflection"),
        "expected reflection after reindex, got: {stdout}"
    );
}

#[test]
fn consult_no_matches() {
    let dir = tempfile::tempdir().unwrap();

    // Reflect something so the DB/index exist
    run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "test",
        "--text",
        "rust ownership rules",
    ]));

    // Consult with a term that will not match
    let stderr = run_ok_stderr(legion_cmd(dir.path()).args([
        "--verbose",
        "consult",
        "--context",
        "nonexistent_term_xyz",
    ]));
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
    let stderr = run_ok_stderr(legion_cmd(dir.path()).args([
        "--verbose",
        "consult",
        "--symbol",
        "Database",
    ]));
    assert!(
        stderr.contains("no SCIP index has a definition for `Database`"),
        "expected no-index message on stderr, got: {stderr}"
    );
}

#[test]
fn consult_symbol_empty_db_json_mode() {
    // #285: --symbol --json on a fresh DB returns the empty array.
    let dir = tempfile::tempdir().unwrap();
    let stdout = run_ok(legion_cmd(dir.path()).args(["consult", "--symbol", "Database", "--json"]));
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
    let (_stdout, stderr) = run_fail(legion_cmd(dir.path()).args(["consult"]));
    assert!(
        stderr.contains("--context") && stderr.contains("--symbol"),
        "error message should mention both modes, got: {stderr}"
    );
}

#[test]
fn consult_context_and_symbol_mutually_exclusive() {
    // #285: clap's conflicts_with should reject both flags at parse time.
    let dir = tempfile::tempdir().unwrap();
    run_fail(legion_cmd(dir.path()).args(["consult", "--context", "x", "--symbol", "Y"]));
}

#[test]
fn reflect_with_metadata_flags() {
    let dir = tempfile::tempdir().unwrap();

    let stdout = run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "kelex",
        "--text",
        "oklch color tokens work well",
        "--domain",
        "color-tokens",
        "--tags",
        "semantic-tokens,consumer",
    ]));
    assert!(
        !stdout.trim().is_empty(),
        "expected ID on stdout, got nothing"
    );
}

#[test]
fn boost_and_chain_roundtrip() {
    let dir = tempfile::tempdir().unwrap();

    // Create a reflection
    let id = run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "kelex",
        "--text",
        "first insight in a chain",
    ]))
    .trim()
    .to_string();

    // Boost the reflection
    let stderr = run_ok_stderr(legion_cmd(dir.path()).args(["--verbose", "boost", "--id", &id]));
    assert!(
        stderr.contains("boosted reflection"),
        "expected boost confirmation, got: {stderr}"
    );

    // Chain with a single node
    let stdout = run_ok(legion_cmd(dir.path()).args(["chain", "--id", &id]));
    assert!(
        stdout.contains("first insight"),
        "expected chain output, got: {stdout}"
    );
}

#[test]
fn chain_with_follows() {
    let dir = tempfile::tempdir().unwrap();

    // Create parent reflection
    let parent_id = run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "kelex",
        "--text",
        "root of the chain",
    ]))
    .trim()
    .to_string();

    // Create child reflection with --follows
    let child_id = run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "kelex",
        "--text",
        "builds on root",
        "--follows",
        &parent_id,
    ]))
    .trim()
    .to_string();

    // Chain from child should show both
    let stdout = run_ok(legion_cmd(dir.path()).args(["chain", "--id", &child_id]));
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

    let id =
        run_ok(legion_cmd(dir.path()).args(["reflect", "--repo", "kelex", "--text", "lone root"]))
            .trim()
            .to_string();

    let stdout = run_ok(legion_cmd(dir.path()).args(["chain", "--id", &id, "--full"]));
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

    let parent_id =
        run_ok(legion_cmd(dir.path()).args(["reflect", "--repo", "kelex", "--text", &long_text]))
            .trim()
            .to_string();

    let child_text = "B".repeat(400);
    run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "kelex",
        "--text",
        &child_text,
        "--follows",
        &parent_id,
    ]));

    let default_stdout = run_ok(legion_cmd(dir.path()).args(["chain", "--id", &parent_id]));
    assert!(
        default_stdout.contains("..."),
        "default chain output should show truncation ellipsis"
    );
    assert!(!default_stdout.contains(&long_text));

    let full_stdout = run_ok(legion_cmd(dir.path()).args(["chain", "--id", &parent_id, "--full"]));
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
    run_ok(legion_cmd(dir.path()).args(["reflect", "--repo", "test", "--text", "setup"]));

    let stderr = run_ok_stderr(legion_cmd(dir.path()).args(["boost", "--id", "nonexistent-uuid"]));
    assert!(
        stderr.contains("reflection not found"),
        "expected not-found message, got: {stderr}"
    );
}

// --- Card A: legion similar ---

#[test]
fn similar_nonexistent_id_exits_nonzero() {
    let dir = tempfile::tempdir().unwrap();

    // Exits non-zero (either "reflection not found" or "embed model unavailable")
    run_fail(legion_cmd(dir.path()).args([
        "similar",
        "--id",
        "00000000-0000-0000-0000-000000000000",
    ]));
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
    run_ok(legion_cmd(dir.path()).args(["reflect", "--repo", "test", "--text", "some reflection"]));

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

    run_ok(legion_cmd(dir.path()).args(["reflect", "--repo", "test", "--text", "min score test"]));

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

    run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "test",
        "--text",
        "some text",
        "--force",
    ]));
}

#[test]
fn reflect_dedupe_mode_off_accepted() {
    let dir = tempfile::tempdir().unwrap();

    run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "test",
        "--text",
        "some text",
        "--dedupe-mode",
        "off",
    ]));
}

#[test]
fn reflect_dedupe_mode_warn_stores_and_exits_zero() {
    let dir = tempfile::tempdir().unwrap();

    // Store first; second is a duplicate but with warn mode should still succeed.
    let text = "near duplicate reflection text for testing";

    run_ok(legion_cmd(dir.path()).args(["reflect", "--repo", "test", "--text", text]));

    // In warn mode: always exits zero (embed model may or may not be available).
    run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "test",
        "--text",
        text,
        "--dedupe-mode",
        "warn",
    ]));
}

/// Requires embed model -- tests that strict mode blocks duplicates.
#[test]
#[ignore]
fn reflect_dedupe_mode_strict_blocks_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    let text = "strict dedup test reflection content";

    // First store succeeds
    run_ok(legion_cmd(dir.path()).args(["reflect", "--repo", "test", "--text", text]));

    // Second store with strict mode should fail (near-duplicate)
    let (_stdout, stderr) = run_fail(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "test",
        "--text",
        text,
        "--dedupe-mode",
        "strict",
    ]));
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

    run_ok(legion_cmd(dir.path()).args(["reflect", "--repo", "test", "--text", text]));

    run_ok(legion_cmd(dir.path()).args([
        "reflect",
        "--repo",
        "test",
        "--text",
        text,
        "--dedupe-mode",
        "strict",
        "--force",
    ]));
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

    let stderr = run_ok_stderr(legion_cmd(data_dir.path()).args(["stats"]));
    assert!(
        !stderr.contains("first-run migration"),
        "LEGION_DATA_DIR override must suppress migration chatter\nstderr: {stderr}"
    );
}

#[test]
fn recall_archives_and_include_archives_are_mutually_exclusive() {
    // #457: passing both flags must fail at clap parse time, not silently
    // pick one or merge them.
    let dir = tempfile::tempdir().unwrap();
    run_fail(legion_cmd(dir.path()).args([
        "recall",
        "--repo",
        "test",
        "--context",
        "x",
        "--archives",
        "--include-archives",
    ]));
}

#[test]
fn recall_archives_flag_accepts_no_args_otherwise() {
    // Sanity: --archives alone parses, runs against an empty DB, exits 0.
    let dir = tempfile::tempdir().unwrap();
    run_ok(legion_cmd(dir.path()).args([
        "recall",
        "--repo",
        "test",
        "--context",
        "x",
        "--archives",
    ]));
}
