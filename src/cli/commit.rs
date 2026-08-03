//! `legion commit` (#854): the audited commit verb, closing the last
//! unaudited hop in the mutation path.
//!
//! `legion push` (#791/#834) routes pushes and #836 fences `gh`, but the
//! commit itself -- where authorship, trailers, and signing actually
//! happen -- was raw `git` with no verb, no audit row, and no preflight.
//! A perimeter assembled by fencing the hops that caused incidents leaves
//! the hops that have not yet caused one, and those read as covered
//! precisely because everything around them is (019fc600).
//!
//! Three things this verb does that `git commit` does not:
//!
//! 1. **Signer preflight.** A locked signer fails once, by name, before
//!    anything is written -- not as five cryptic retries. The probe signs a
//!    throwaway commit object with [`git commit-tree -S`](probe_signer),
//!    which is git's own `sign_buffer` path; `git commit --dry-run` does
//!    not exercise the signer at all.
//! 2. **Message conventions, refused by name.** Subject shape, the
//!    `Co-Authored-By` trailer, and the no-emoji rule are checked before
//!    the commit runs, so a violation costs a re-run instead of an amend.
//! 3. **An audit row on every attempt**, refusals included, carrying the
//!    resolved checkout, pre/post HEAD, card id, and the quality-gate state
//!    of the commit being built on.
//!
//! The PreToolUse hook that rewrites plain `git commit` into this verb is
//! deliberately NOT part of this change -- it is the second half of #854.
//! Note for whoever builds it: this validator requires a scope on every
//! subject (see [`validate_subject`]), so routing every `git commit` through
//! here makes that repo-wide policy.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::cli::util::{audit, open_db, relay_and_capture_stderr};
use crate::verify::GateResult;
use crate::{db, error};

/// Commit types accepted in a subject line, derived from this repo's own
/// history rather than from the Conventional Commits spec: these are the
/// eight that appear across the last 600 subjects (`feat` and `fix` account
/// for most of them; `perf` and `style` are rare but real). Types the repo
/// has never used (`ci`, `build`, `revert`) are deliberately absent -- the
/// rule encodes what the repo does, and widening it is a decision someone
/// should make on purpose.
const COMMIT_TYPES: [&str; 8] = [
    "feat", "fix", "chore", "docs", "refactor", "test", "perf", "style",
];

/// Quality gates whose verdict is recorded on the audit row, in the order
/// the pipeline runs them. Same skill keys `pr create` reads (`src/cli/pr.rs`),
/// which is what makes the two surfaces comparable in the audit log.
const RECORDED_GATES: [(&str, &str); 2] = [
    ("simplify", "legion-simplify"),
    ("pr_write", "legion-pr-write"),
];

/// Codepoint ranges rejected anywhere in a commit message.
///
/// Not a complete Unicode-emoji property table -- deliberately. The blocks
/// below cover every emoji anyone actually reaches for (check marks,
/// sparkles, rockets, warning signs, flags, emoticons) while leaving Misc
/// Technical (U+2300..U+23FF) alone, because that block mixes a handful of
/// emoji in with the Mac modifier glyphs, and a false refusal on a
/// legitimate character is worse than missing a stopwatch.
const EMOJI_RANGES: [(u32, u32); 5] = [
    (0x2600, 0x27BF),   // Misc Symbols + Dingbats: warning, check mark, sparkles
    (0x2B00, 0x2BFF),   // Misc Symbols and Arrows: stars
    (0x20E3, 0x20E3),   // Combining Enclosing Keycap
    (0xFE0F, 0xFE0F),   // Variation Selector-16 (the emoji-presentation selector)
    (0x1F000, 0x1FAFF), // Cards through Symbols and Pictographs Extended-A
];

/// Message given to the throwaway probe commit object so a stray `git fsck`
/// on a dangling object reads as intentional rather than as corruption.
const PROBE_MESSAGE: &str = "legion signer preflight (#854)";

/// Trailer key every commit message must end with, including the colon.
/// Matched case-insensitively -- see [`is_coauthor_trailer`].
const COAUTHOR_KEY: &str = "co-authored-by:";

/// Commit staged changes in the CWD's checkout, audited.
///
/// Order is load-bearing: the signer preflight runs before message
/// validation even though validation is far cheaper, because a locked
/// signer needs the operator to go unlock something while a malformed
/// subject is a five-second fix. Surfacing the slow failure first means one
/// round trip instead of two.
pub(crate) fn handle_commit(
    repo: String,
    message: Option<String>,
    message_file: Option<String>,
    card: Option<String>,
) -> error::Result<()> {
    // Argument-shape errors are resolved before anything is audited: no
    // mutation has been attempted yet, so there is nothing to record.
    let text: String = resolve_message(message.as_deref(), message_file.as_deref())?;

    let checkout: PathBuf = resolve_checkout()?;
    let branch: String = current_branch(&checkout)?;
    let pre_sha: Option<String> = head_sha(&checkout)?;

    // Opened before the commit (not after) so a DB-open failure fails fast
    // rather than masking the commit result behind a DB error once the
    // commit has already landed -- same ordering as `legion push`.
    let database = open_db()?;

    // Gate state describes the commit being built ON, so it is read from
    // the PRE-commit HEAD and read before the commit runs.
    let gates: serde_json::Value = gate_state(&database, pre_sha.as_deref());

    let result: error::Result<String> = preflight_validate_and_commit(&checkout, &text);

    // Every attempt leaves a row, refusals included. A refused commit is an
    // attempted mutation that the perimeter blocked, and that is exactly
    // the signal the audit trail exists to carry -- especially once the
    // deferred hook starts routing every plain `git commit` through here.
    // The error (if any) propagates AFTER the row is written.
    let details = serde_json::json!({
        "checkout": checkout.display().to_string(),
        "pre_sha": pre_sha,
        "post_sha": result.as_ref().ok(),
        "card_id": card,
        "gates": gates,
    })
    .to_string();
    audit(
        &database,
        &db::AuditInput {
            agent: &repo,
            action: "commit",
            target_type: "branch",
            target_ref: &branch,
            task_id: card.as_deref(),
            source_type: "git",
            details: Some(&details),
            outcome: if result.is_ok() { "success" } else { "failure" },
        },
    );

    let post_sha = result?;
    println!(
        "committed {} on {branch} ({})",
        short_sha(&post_sha),
        checkout.display()
    );
    Ok(())
}

/// Preflight the signer, validate the message, then commit. Split out from
/// [`handle_commit`] so every failure between "we know which checkout and
/// branch" and "the commit landed" funnels through one `Result` the audit
/// row can classify, instead of each refusal needing its own audit call.
fn preflight_validate_and_commit(checkout: &Path, message: &str) -> error::Result<String> {
    preflight_signer(checkout)?;
    validate_message(message)?;
    run_git_commit(checkout, message)
}

/// Resolve the commit message from exactly one of `--message` /
/// `--message-file`. Both or neither is a refusal rather than a silent
/// precedence rule -- a verb that quietly picks one when given two is how a
/// stale file ends up as the commit message.
fn resolve_message(message: Option<&str>, message_file: Option<&str>) -> error::Result<String> {
    match (message, message_file) {
        (Some(_), Some(_)) => Err(error::LegionError::CommitRefused {
            reason: "--message and --message-file are mutually exclusive -- pass exactly one"
                .to_string(),
        }),
        (Some(m), None) => Ok(m.to_string()),
        (None, Some(p)) => {
            std::fs::read_to_string(p).map_err(|e| error::LegionError::CommitRefused {
                reason: format!("failed to read --message-file '{p}': {e}"),
            })
        }
        (None, None) => Err(error::LegionError::CommitRefused {
            reason: "no commit message -- pass --message or --message-file".to_string(),
        }),
    }
}

/// Resolve the checkout to commit in: the repo root containing the CWD.
///
/// v1 deliberately does NOT do `legion push`-style cross-checkout
/// resolution. Push has to, because it takes a `--branch` that may live in
/// a different worktree than the caller stands in; commit operates on a
/// staged index, which is per-checkout state that only the CWD's checkout
/// has. Resolving to the repo root (rather than using the CWD verbatim)
/// still matters -- it makes the verb work from a subdirectory, and it is
/// the path recorded on the audit row.
fn resolve_checkout() -> error::Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| error::LegionError::WorkSource(format!("failed to run git rev-parse: {e}")))?;
    if !output.status.success() {
        return Err(error::LegionError::CommitRefused {
            reason: "not inside a git repository -- run legion commit from the checkout whose \
                     staged changes you want committed"
                .to_string(),
        });
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        return Err(error::LegionError::CommitRefused {
            reason: "git rev-parse --show-toplevel returned an empty path".to_string(),
        });
    }
    Ok(PathBuf::from(path))
}

/// Branch name for the audit row's `target_ref`.
///
/// `git branch --show-current` rather than `rev-parse --abbrev-ref HEAD`:
/// it is the only one of the two that answers on an unborn branch (a repo
/// with zero commits), where `rev-parse` fails outright. Empty output means
/// a detached HEAD, which is recorded as such rather than being smuggled in
/// as the literal string "HEAD".
fn current_branch(checkout: &Path) -> error::Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["branch", "--show-current"])
        .output()
        .map_err(|e| {
            error::LegionError::WorkSource(format!("failed to run git branch --show-current: {e}"))
        })?;
    if !output.status.success() {
        return Err(error::LegionError::WorkSource(format!(
            "git branch --show-current failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() {
        Ok("detached".to_string())
    } else {
        Ok(branch)
    }
}

/// Current HEAD commit, or `None` when HEAD is unborn (a repo whose first
/// commit is the one being made).
///
/// Not [`crate::cli::util::git_head_commit_and_branch`]: that helper reports
/// an unresolvable HEAD as "is this a git repo?", which is the wrong
/// diagnosis for a repo with zero commits and would send the caller looking
/// in the wrong place. `-q --verify` distinguishes the two cleanly.
fn head_sha(checkout: &Path) -> error::Result<Option<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["rev-parse", "-q", "--verify", "HEAD"])
        .output()
        .map_err(|e| {
            error::LegionError::WorkSource(format!("failed to run git rev-parse HEAD: {e}"))
        })?;
    if !output.status.success() {
        return Ok(None);
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.is_empty() {
        Ok(None)
    } else {
        Ok(Some(sha))
    }
}

/// First 8 characters of a commit hash, for the confirmation line. Shorter
/// input (should not happen for a real SHA) is returned as-is.
///
/// `get` (byte-index-checked) rather than a raw slice, matching the same
/// choice and the same reasoning as `cli::index_cmd`'s 7-char display form:
/// a real SHA is all-ASCII hex so byte 8 is always a char boundary, but the
/// value arrives via `from_utf8_lossy` on git's stdout, and no code path
/// here may assume it cannot panic.
fn short_sha(sha: &str) -> &str {
    sha.get(..8).unwrap_or(sha)
}

/// Quality-gate verdicts for the pre-commit HEAD, as the `gates` object on
/// the audit row.
///
/// Three states, not the two the issue sketched: a gate that recorded
/// ISSUES is neither clean nor absent, and collapsing it into either bucket
/// would put a falsehood in the audit trail. A DB read failure records
/// "unknown" and warns, matching [`audit`]'s own posture -- a details field
/// must never be the reason a row goes unwritten.
fn gate_state(database: &db::Database, pre_sha: Option<&str>) -> serde_json::Value {
    let mut gates = serde_json::Map::new();
    for (key, skill) in RECORDED_GATES {
        let verdict: &str = match pre_sha {
            None => "absent",
            Some(sha) => match database.get_quality_gate(sha, skill) {
                Ok(None) => "absent",
                Ok(Some(gate)) if gate.result == GateResult::Clean => "clean",
                Ok(Some(_)) => "issues",
                Err(e) => {
                    eprintln!("[legion] warning: could not read {skill} gate: {e}");
                    "unknown"
                }
            },
        };
        gates.insert(
            key.to_string(),
            serde_json::Value::String(verdict.to_string()),
        );
    }
    serde_json::Value::Object(gates)
}

/// Fail once, by name, if commit signing is configured but the signer
/// cannot sign right now.
///
/// `commit.gpgsign` alone decides whether this runs; `gpg.format` only
/// selects which program name appears in the error. The probe is `git
/// commit-tree -S` against the tree we are about to commit, and the
/// resulting oid is discarded.
///
/// Why `commit-tree` and not invoking the configured signer directly: the
/// direct route means reimplementing git's key selection -- `user.signingkey`
/// resolution, ssh literal-key-vs-path handling, and openpgp's fallback of
/// deriving the key from the committer email -- and a probe that picks a
/// different key than the real commit will is worse than no probe. Going
/// through `commit-tree` is git's own `sign_buffer` path by construction.
/// It writes one unreferenced object into the object store and touches no
/// ref, index, or working-tree file, which is what "before touching
/// anything" means in the sense that matters. `git commit --dry-run` is not
/// an option: it never reaches the signer.
fn preflight_signer(checkout: &Path) -> error::Result<()> {
    if !signing_enabled(checkout) {
        return Ok(());
    }
    let program: String = signer_program(checkout);
    let tree: String = probe_tree(checkout)?;
    probe_signer(checkout, &tree, &program)
}

/// Run the probe. Split from [`preflight_signer`] so the config reads and
/// the sign attempt stay separately readable.
fn probe_signer(checkout: &Path, tree: &str, program: &str) -> error::Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["commit-tree", "-S", "-m", PROBE_MESSAGE, tree])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| {
            error::LegionError::WorkSource(format!("failed to run git commit-tree: {e}"))
        })?;
    if output.status.success() {
        return Ok(());
    }
    Err(error::LegionError::CommitSigningUnavailable {
        program: program.to_string(),
        detail: signer_failure_detail(&String::from_utf8_lossy(&output.stderr)),
    })
}

/// Whether `commit.gpgsign` is on. An absent key exits non-zero, which is
/// "off" -- there is no signer to preflight, and this verb never turns
/// signing on or off, it only reports what is configured.
fn signing_enabled(checkout: &Path) -> bool {
    git_config(checkout, "commit.gpgsign").as_deref() == Some("true")
}

/// Name of the program git would invoke to sign, for the error message.
/// Mirrors git's own format-to-program mapping and its defaults, so an
/// operator reading the refusal sees the binary they need to unlock.
fn signer_program(checkout: &Path) -> String {
    let format: String =
        git_config(checkout, "gpg.format").unwrap_or_else(|| "openpgp".to_string());
    let (key, default) = match format.as_str() {
        "ssh" => ("gpg.ssh.program", "ssh-keygen"),
        "x509" => ("gpg.x509.program", "gpgsm"),
        _ => ("gpg.program", "gpg"),
    };
    git_config(checkout, key).unwrap_or_else(|| default.to_string())
}

/// Read one git config value, `None` when unset or unreadable.
/// `--type=bool` is not used: `signing_enabled` compares against "true" and
/// the other reads are strings, so one accessor covers both.
fn git_config(checkout: &Path, key: &str) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["config", "--get", key])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() { None } else { Some(value) }
}

/// Tree object for the probe to sign: HEAD's tree when there is a HEAD,
/// otherwise the staged tree (the unborn-branch case, where HEAD^{tree}
/// does not resolve). `write-tree` reads the index and writes tree objects;
/// it does not modify the index.
fn probe_tree(checkout: &Path) -> error::Result<String> {
    let head_tree = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["rev-parse", "-q", "--verify", "HEAD^{tree}"])
        .output()
        .map_err(|e| {
            error::LegionError::WorkSource(format!(
                "failed to run git rev-parse HEAD^{{tree}}: {e}"
            ))
        })?;
    if head_tree.status.success() {
        let tree = String::from_utf8_lossy(&head_tree.stdout)
            .trim()
            .to_string();
        if !tree.is_empty() {
            return Ok(tree);
        }
    }

    let written = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["write-tree"])
        .output()
        .map_err(|e| {
            error::LegionError::WorkSource(format!("failed to run git write-tree: {e}"))
        })?;
    if !written.status.success() {
        return Err(error::LegionError::WorkSource(format!(
            "git write-tree failed, so the signer could not be preflighted: {}",
            String::from_utf8_lossy(&written.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&written.stdout).trim().to_string())
}

/// Pull the useful line out of a failed signing attempt's stderr.
///
/// git relays the signer's own stderr behind a bare `error: ` prefix, so a
/// signer that fails silently (a stub like `/usr/bin/false`, or an agent
/// that exits without a message) leaves nothing but the prefix -- measured,
/// not assumed: `/usr/bin/false` produces exactly `"error: \n"`. Stripping
/// the prefix and falling back to a literal keeps the refusal from reading
/// "signing unavailable (...):  -- unlock your signer and re-run" with a
/// hole where the reason should be.
fn signer_failure_detail(stderr: &str) -> String {
    for line in stderr.lines() {
        let trimmed = line.trim();
        let stripped = trimmed
            .strip_prefix("error:")
            .or_else(|| trimmed.strip_prefix("fatal:"))
            .unwrap_or(trimmed)
            .trim();
        if !stripped.is_empty() {
            return stripped.to_string();
        }
    }
    "the signer exited non-zero without a message".to_string()
}

/// Enforce this repo's commit-message conventions, refusing by name.
///
/// Four rules, each a distinct refusal so the caller knows what to change:
/// no emoji anywhere, a conventional subject line, a blank line after the
/// subject, and a `Co-Authored-By` trailer as the last line. The emoji scan
/// runs first because it is the only rule that can fire on the subject AND
/// the body, and reporting "bad subject" for a message whose real problem is
/// a rocket in paragraph three would send the caller to the wrong line.
fn validate_message(message: &str) -> error::Result<()> {
    if let Some(ch) = find_emoji(message) {
        return Err(error::LegionError::CommitRefused {
            reason: format!(
                "commit message contains an emoji ({ch:?}, U+{:04X}) -- legion commit messages \
                 are plain text",
                ch as u32
            ),
        });
    }

    let lines: Vec<&str> = message.lines().collect();
    if lines.iter().all(|l| l.trim().is_empty()) {
        // Covers both a zero-line message and one that is nothing but
        // whitespace, so the subject lookup below has no empty-vec case
        // left to handle.
        return Err(error::LegionError::CommitRefused {
            reason: "commit message is empty".to_string(),
        });
    }
    let subject: &str = lines.first().copied().unwrap_or("");
    if subject.trim().is_empty() {
        return Err(error::LegionError::CommitRefused {
            reason: "commit message starts with a blank line -- the first line is the subject"
                .to_string(),
        });
    }
    validate_subject(subject)?;

    match lines.get(1) {
        None => {
            return Err(error::LegionError::CommitRefused {
                reason: "commit message has no body -- it must end with a 'Co-Authored-By: \
                         <name> <email>' trailer"
                    .to_string(),
            });
        }
        Some(second) if !second.trim().is_empty() => {
            return Err(error::LegionError::CommitRefused {
                reason: format!("the subject must be followed by a blank line, found: {second:?}"),
            });
        }
        Some(_) => {}
    }

    let last: &str = lines
        .iter()
        .rev()
        .find(|l| !l.trim().is_empty())
        .copied()
        .unwrap_or("");
    if !is_coauthor_trailer(last) {
        return Err(error::LegionError::CommitRefused {
            reason: format!(
                "commit message must end with a 'Co-Authored-By: <name> <email>' trailer, \
                 last line was: {last:?}"
            ),
        });
    }

    Ok(())
}

/// Accept `<type>(<scope>): <summary>`.
///
/// The pattern encoded here is what this repo's history actually contains,
/// not the Conventional Commits grammar. Of the last 50 subjects, every
/// conventional one carries a scope -- 43 of them an issue ref (`#854`), the
/// rest a bare word (`release`, `worksource`) -- so the scope is REQUIRED,
/// which is what makes `chore(release): 0.25.0` pass and a bare
/// `chore: bump` fail. Types come from [`COMMIT_TYPES`].
///
/// No length cap: this repo routinely writes subjects well past 72
/// characters (`feat(#780): provenance + void columns, validator registry,
/// ...`), and a rule the repo violates on most commits is not a rule.
fn validate_subject(subject: &str) -> error::Result<()> {
    let refuse = |detail: &str| error::LegionError::CommitRefused {
        reason: format!(
            "bad subject line ({detail}) -- expected '<type>(<scope>): <summary>' where <type> \
             is one of {} and <scope> is an issue ref like '#854' or a bare word like 'release'",
            COMMIT_TYPES.join("/")
        ),
    };

    let Some((prefix, summary)) = subject.split_once(": ") else {
        return Err(refuse("no '<type>(<scope>): ' prefix"));
    };
    if summary.trim().is_empty() {
        return Err(refuse("empty summary after the colon"));
    }

    let Some((commit_type, scope_with_paren)) = prefix.split_once('(') else {
        return Err(refuse("missing '(<scope>)'"));
    };
    if !COMMIT_TYPES.contains(&commit_type) {
        return Err(refuse(&format!("unknown type '{commit_type}'")));
    }
    let Some(scope) = scope_with_paren.strip_suffix(')') else {
        // Tell the two failures apart rather than blaming the paren for
        // both: `feat(#854: x` really is unclosed, but `feat(#854) oops: x`
        // closed it and then kept going, and reporting that as "unclosed"
        // sends the caller looking for a bracket that is right there.
        return Err(refuse(if scope_with_paren.contains(')') {
            "unexpected text after '(<scope>)'"
        } else {
            "unclosed '(' in the scope"
        }));
    };
    if scope.is_empty() {
        return Err(refuse("empty scope"));
    }
    if !scope
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "#._/-".contains(c))
    {
        return Err(refuse(&format!(
            "scope '{scope}' has unexpected characters"
        )));
    }

    Ok(())
}

/// The first emoji codepoint in `text`, if any. See [`EMOJI_RANGES`] for
/// what counts and what deliberately does not.
fn find_emoji(text: &str) -> Option<char> {
    text.chars().find(|ch| {
        let cp = *ch as u32;
        EMOJI_RANGES
            .iter()
            .any(|(start, end)| cp >= *start && cp <= *end)
    })
}

/// Whether `line` is a `Co-Authored-By` trailer with a non-empty value.
/// Case-insensitive on the key: this repo's history carries both
/// `Co-Authored-By:` and GitHub's canonical `Co-authored-by:`, and refusing
/// one of the two spellings the repo already uses would be inventing a rule.
fn is_coauthor_trailer(line: &str) -> bool {
    let trimmed = line.trim();
    let Some(rest) = trimmed
        .get(..COAUTHOR_KEY.len())
        .filter(|head| head.eq_ignore_ascii_case(COAUTHOR_KEY))
        .map(|head| &trimmed[head.len()..])
    else {
        return false;
    };
    !rest.trim().is_empty()
}

/// Run `git commit -F <tempfile>` in `checkout` and return the new HEAD.
///
/// `--cleanup=whitespace` is pinned on the command line rather than left to
/// git's default for `-F`. The default IS `whitespace`, but an ambient
/// `commit.cleanup` overrides it, and this verb runs in real repos with real
/// config -- the same argument that pins `-M50%` on every `git diff` in
/// `cli::util`. It matters here because `strip` would delete any body line
/// beginning with `#`, and issue refs at the start of a line are ordinary in
/// this repo's commit bodies.
///
/// No `-a`: only the staged index is committed, exactly as `git commit`
/// would. Nothing here stages anything.
fn run_git_commit(checkout: &Path, message: &str) -> error::Result<String> {
    // UUIDv7 name rather than a fixed one so two concurrent agents (the
    // situation 019fc46c is about) cannot overwrite each other's message
    // file between write and read.
    let msg_path: PathBuf =
        std::env::temp_dir().join(format!("legion-commit-{}.msg", uuid::Uuid::now_v7()));
    std::fs::write(&msg_path, message)?;

    let outcome = spawn_git_commit(checkout, &msg_path);

    // Best-effort: a leftover message file in the temp dir is noise, not a
    // failure, and must never mask the commit's own result.
    let _ = std::fs::remove_file(&msg_path);

    outcome?;

    head_sha(checkout)?.ok_or_else(|| {
        error::LegionError::WorkSource(
            "git commit reported success but HEAD does not resolve".to_string(),
        )
    })
}

/// Spawn the commit with stderr relayed live and captured. Live relay is not
/// cosmetic here: this repo's `pre-commit` hook runs a nested Claude review
/// with a 120-second budget, and a two-minute silence with no output reads
/// as a hang.
fn spawn_git_commit(checkout: &Path, msg_path: &Path) -> error::Result<()> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["commit", "--cleanup=whitespace", "-F"])
        .arg(msg_path)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| error::LegionError::WorkSource(format!("failed to spawn git commit: {e}")))?;

    let stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| error::LegionError::WorkSource("git commit stderr missing".to_string()))?;
    let captured = relay_and_capture_stderr(stderr_pipe);

    let status = child
        .wait()
        .map_err(|e| error::LegionError::WorkSource(format!("git commit wait failed: {e}")))?;

    if !status.success() {
        return Err(error::LegionError::CommitFailed { stderr: captured });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A message that satisfies every rule, for the tests that mutate one
    /// thing at a time.
    fn good_message() -> String {
        "feat(#854): legion commit\n\
         \n\
         Body text.\n\
         \n\
         Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>\n"
            .to_string()
    }

    fn refusal_reason(err: error::LegionError) -> String {
        match err {
            error::LegionError::CommitRefused { reason } => reason,
            other => panic!("expected CommitRefused, got {other:?}"),
        }
    }

    #[test]
    fn validate_message_accepts_the_house_style() {
        validate_message(&good_message()).expect("the house style must validate");
    }

    #[test]
    fn validate_message_accepts_release_scope() {
        let msg = "chore(release): 0.25.0\n\nCo-Authored-By: Claude <x@y.invalid>\n";
        validate_message(msg).expect("chore(release) is real history and must validate");
    }

    #[test]
    fn validate_message_accepts_bare_word_scope() {
        let msg = "fix(worksource): name the operation\n\nCo-authored-by: Claude <x@y.invalid>\n";
        validate_message(msg).expect("a bare-word scope is real history and must validate");
    }

    #[test]
    fn validate_message_refuses_empty() {
        let reason = refusal_reason(validate_message("   \n\n  \n").unwrap_err());
        assert!(reason.contains("empty"), "got: {reason}");
    }

    #[test]
    fn validate_message_refuses_unscoped_subject() {
        let msg = "feat: no scope here\n\nCo-Authored-By: Claude <x@y.invalid>\n";
        let reason = refusal_reason(validate_message(msg).unwrap_err());
        assert!(reason.contains("missing '(<scope>)'"), "got: {reason}");
    }

    #[test]
    fn validate_message_refuses_unknown_type() {
        let msg = "wip(#854): something\n\nCo-Authored-By: Claude <x@y.invalid>\n";
        let reason = refusal_reason(validate_message(msg).unwrap_err());
        assert!(reason.contains("unknown type 'wip'"), "got: {reason}");
    }

    #[test]
    fn validate_message_refuses_missing_prefix_entirely() {
        let msg = "just some words\n\nCo-Authored-By: Claude <x@y.invalid>\n";
        let reason = refusal_reason(validate_message(msg).unwrap_err());
        assert!(
            reason.contains("no '<type>(<scope>): ' prefix"),
            "got: {reason}"
        );
    }

    #[test]
    fn validate_message_refuses_empty_summary() {
        let msg = "feat(#854): \n\nCo-Authored-By: Claude <x@y.invalid>\n";
        let reason = refusal_reason(validate_message(msg).unwrap_err());
        assert!(reason.contains("empty summary"), "got: {reason}");
    }

    #[test]
    fn validate_subject_tells_an_unclosed_paren_from_trailing_text() {
        // Both fail, but for opposite reasons, and a caller sent looking for
        // a missing ')' that is right there loses the round trip.
        let unclosed = refusal_reason(validate_subject("feat(#854: summary").unwrap_err());
        assert!(unclosed.contains("unclosed '('"), "got: {unclosed}");

        let trailing = refusal_reason(validate_subject("feat(#854) oops: summary").unwrap_err());
        assert!(
            trailing.contains("unexpected text after"),
            "got: {trailing}"
        );
    }

    #[test]
    fn validate_message_refuses_missing_trailer() {
        let msg = "feat(#854): legion commit\n\nBody without a trailer.\n";
        let reason = refusal_reason(validate_message(msg).unwrap_err());
        assert!(reason.contains("Co-Authored-By"), "got: {reason}");
    }

    #[test]
    fn validate_message_refuses_subject_only() {
        let msg = "feat(#854): legion commit\n";
        let reason = refusal_reason(validate_message(msg).unwrap_err());
        assert!(reason.contains("no body"), "got: {reason}");
    }

    #[test]
    fn validate_message_refuses_missing_blank_line_after_subject() {
        let msg =
            "feat(#854): legion commit\nBody on line two.\n\nCo-Authored-By: C <x@y.invalid>\n";
        let reason = refusal_reason(validate_message(msg).unwrap_err());
        assert!(reason.contains("blank line"), "got: {reason}");
    }

    #[test]
    fn validate_message_refuses_leading_blank_line() {
        let msg = "\nfeat(#854): legion commit\n\nCo-Authored-By: C <x@y.invalid>\n";
        let reason = refusal_reason(validate_message(msg).unwrap_err());
        assert!(reason.contains("starts with a blank line"), "got: {reason}");
    }

    #[test]
    fn validate_message_refuses_emoji_in_subject() {
        let msg = "feat(#854): ship it \u{1F680}\n\nCo-Authored-By: C <x@y.invalid>\n";
        let reason = refusal_reason(validate_message(msg).unwrap_err());
        assert!(reason.contains("emoji"), "got: {reason}");
        assert!(reason.contains("U+1F680"), "got: {reason}");
    }

    #[test]
    fn validate_message_refuses_emoji_in_body() {
        // The emoji scan must beat the subject check to the punch, or the
        // caller is told the subject is wrong when the subject is fine.
        let msg =
            "feat(#854): legion commit\n\nAll done \u{2705}\n\nCo-Authored-By: C <x@y.invalid>\n";
        let reason = refusal_reason(validate_message(msg).unwrap_err());
        assert!(reason.contains("emoji"), "got: {reason}");
    }

    #[test]
    fn find_emoji_ignores_ordinary_prose_and_punctuation() {
        // Every one of these appears in this repo's real commit bodies and
        // must not be mistaken for an emoji: em dash, curly quotes, arrows
        // written as ASCII, backticks, accented latin.
        assert_eq!(
            find_emoji("a -- b \u{2014} \u{201c}quoted\u{201d} -> `code` caf\u{e9}"),
            None
        );
    }

    #[test]
    fn find_emoji_catches_each_configured_range() {
        assert_eq!(find_emoji("x \u{2705}"), Some('\u{2705}')); // dingbats
        assert_eq!(find_emoji("x \u{2B50}"), Some('\u{2B50}')); // misc symbols/arrows
        assert_eq!(find_emoji("x \u{1F389}"), Some('\u{1F389}')); // pictographs
        assert_eq!(find_emoji("x \u{FE0F}"), Some('\u{FE0F}')); // variation selector
        assert_eq!(find_emoji("x \u{20E3}"), Some('\u{20E3}')); // keycap
    }

    #[test]
    fn is_coauthor_trailer_matches_both_spellings_in_history() {
        assert!(is_coauthor_trailer("Co-Authored-By: Claude <x@y.invalid>"));
        assert!(is_coauthor_trailer("Co-authored-by: Claude <x@y.invalid>"));
        assert!(is_coauthor_trailer(
            "  co-authored-by: Claude <x@y.invalid>  "
        ));
    }

    #[test]
    fn is_coauthor_trailer_rejects_empty_value_and_near_misses() {
        assert!(!is_coauthor_trailer("Co-Authored-By:"));
        assert!(!is_coauthor_trailer("Co-Authored-By:   "));
        assert!(!is_coauthor_trailer("Signed-off-by: Claude <x@y.invalid>"));
        assert!(!is_coauthor_trailer(""));
        // Multi-byte lead: the key match must not slice mid-codepoint.
        assert!(!is_coauthor_trailer(
            "\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}"
        ));
    }

    #[test]
    fn resolve_message_requires_exactly_one_source() {
        let reason = refusal_reason(resolve_message(None, None).unwrap_err());
        assert!(
            reason.contains("--message or --message-file"),
            "got: {reason}"
        );

        let reason = refusal_reason(resolve_message(Some("x"), Some("y")).unwrap_err());
        assert!(reason.contains("mutually exclusive"), "got: {reason}");

        assert_eq!(resolve_message(Some("hello"), None).unwrap(), "hello");
    }

    #[test]
    fn resolve_message_names_the_unreadable_file() {
        let reason =
            refusal_reason(resolve_message(None, Some("/nonexistent/legion/msg")).unwrap_err());
        assert!(reason.contains("/nonexistent/legion/msg"), "got: {reason}");
    }

    #[test]
    fn signer_failure_detail_falls_back_when_the_signer_says_nothing() {
        // Measured from `git -c gpg.ssh.program=/usr/bin/false commit-tree -S`:
        // stderr is exactly "error: \n", which strips to nothing.
        assert_eq!(
            signer_failure_detail("error: \n"),
            "the signer exited non-zero without a message"
        );
        assert_eq!(
            signer_failure_detail(""),
            "the signer exited non-zero without a message"
        );
    }

    #[test]
    fn signer_failure_detail_uses_the_first_informative_line() {
        let stderr = "error: \nerror: gpg failed to sign the data\nfatal: failed to write commit\n";
        assert_eq!(signer_failure_detail(stderr), "gpg failed to sign the data");
    }

    /// Record a gate row so the three verdicts can be told apart.
    fn record_gate(database: &db::Database, sha: &str, skill: &str, result: GateResult) {
        use crate::db::quality_gates::QualityGateInput;
        use crate::verify::GateProvenance;

        database
            .record_quality_gate(&QualityGateInput {
                branch: "feat/854-legion-commit",
                commit_hash: sha,
                skill,
                result,
                findings_count: if result == GateResult::Clean { 0 } else { 3 },
                details: None,
                provenance: GateProvenance::Validated,
                base: None,
            })
            .expect("recording a gate row must succeed");
    }

    #[test]
    fn gate_state_distinguishes_clean_issues_and_absent() {
        let database = db::testutil::test_db();
        let sha = "0123456789abcdef0123456789abcdef01234567";
        record_gate(&database, sha, "legion-simplify", GateResult::Clean);
        record_gate(&database, sha, "legion-pr-write", GateResult::Issues);

        let gates = gate_state(&database, Some(sha));
        assert_eq!(gates["simplify"], "clean");
        // A gate that recorded ISSUES is neither clean nor absent -- the
        // whole reason this is three-valued rather than the two the issue
        // sketched.
        assert_eq!(gates["pr_write"], "issues");
    }

    #[test]
    fn gate_state_reports_absent_for_an_ungated_commit() {
        let database = db::testutil::test_db();
        let gates = gate_state(&database, Some("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"));
        assert_eq!(gates["simplify"], "absent");
        assert_eq!(gates["pr_write"], "absent");
    }

    #[test]
    fn gate_state_reports_absent_when_head_is_unborn() {
        // The first commit in a repo has no pre-HEAD to key gates on, so
        // there is nothing to look up rather than nothing recorded.
        let database = db::testutil::test_db();
        let gates = gate_state(&database, None);
        assert_eq!(gates["simplify"], "absent");
        assert_eq!(gates["pr_write"], "absent");
    }

    #[test]
    fn short_sha_truncates_and_tolerates_short_input() {
        assert_eq!(short_sha("0123456789abcdef"), "01234567");
        assert_eq!(short_sha("abc"), "abc");
    }
}
