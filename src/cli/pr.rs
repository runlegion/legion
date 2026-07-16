//! `legion pr` handlers (carved from main.rs, #610).

use clap::Subcommand;

use crate::cli::datadir::data_dir;
use crate::cli::util::{audit, git_head_commit_and_branch, open_db, read_file_or_stdin};
use crate::db::quality_gates::QualityGateInput;
use crate::verify::{GateProvenance, GateResult};
use crate::{board, card_parse, db, error, kanban, pr_view, pr_write, search, worksource};

#[derive(Subcommand, Debug)]
pub(crate) enum PrAction {
    /// Create a pull request via the configured work source
    Create {
        /// Repository name (resolves work source config from watch.toml)
        #[arg(long)]
        repo: String,

        /// PR title
        #[arg(long)]
        title: String,

        /// PR body
        #[arg(long)]
        body: Option<String>,

        /// Base branch (default: repo default branch)
        #[arg(long)]
        base: Option<String>,

        /// Head branch (default: current branch)
        #[arg(long)]
        head: Option<String>,

        /// Create as draft
        #[arg(long)]
        draft: bool,

        /// Comma-separated labels
        #[arg(long)]
        labels: Option<String>,

        /// Reviewer login (e.g., the review agent's GitHub account)
        #[arg(long)]
        reviewer: Option<String>,

        /// Kanban card ID to link (stores PR URL on the card)
        #[arg(long)]
        task: Option<String>,

        /// Skip the legion-simplify quality gate check.
        /// FOR BOOTSTRAP ONLY -- use when the branch IS the simplify skill itself.
        /// An audit entry is written so this cannot be done silently.
        #[arg(long)]
        skip_gates: bool,

        /// Issue this PR closes on merge (repeatable). Accepts a same-repo
        /// issue number ("751") or a cross-repo reference
        /// ("owner/repo#751" -- quote it in the shell, `#` starts a
        /// comment). Appends a `Closes #N` line to the body unless a
        /// recognized closing keyword for that issue is already present;
        /// idempotent across repeated invocations.
        #[arg(long)]
        closes: Vec<String>,
    },

    /// List open pull requests with review status
    List {
        /// Repository name (resolves work source config from watch.toml)
        #[arg(long)]
        repo: String,
    },

    /// Show CI check status for a pull request.
    /// Exits non-zero if any check is not in a known passing or in-flight
    /// state (see `ExternalPRCheck::is_failing` -- fail-closed on unknown
    /// states so a future gh release with a new failure variant cannot
    /// silently render as green to the merge gate).
    Checks {
        /// Repository name (resolves work source config from watch.toml)
        #[arg(long)]
        repo: String,

        /// PR number
        #[arg(long)]
        number: u64,

        /// Emit raw JSON instead of human-formatted output
        #[arg(long)]
        json: bool,

        /// Additionally stream the raw CI log for every failing check.
        /// Each log is preceded by a `===== <name> (<job-id>) =====` header.
        /// Per-job log fetch failures print a marker and do not abort the
        /// run -- the exit code still reflects the overall check status.
        #[arg(long)]
        log_failed: bool,
    },

    /// Show a pull request's body and metadata.
    View {
        /// Repository name (resolves work source config from watch.toml)
        #[arg(long)]
        repo: String,

        /// PR number
        #[arg(long)]
        number: u64,

        /// Emit raw JSON instead of human-formatted output
        #[arg(long)]
        json: bool,
    },

    /// Validate a drafted PR body against an issue's acceptance criteria
    /// (#519 PR-write forcing function). Reads the body from --body-file (or
    /// stdin), loads the issue's acceptance criteria, and refuses an empty or
    /// boilerplate criterion-to-work mapping. On success it records a clean
    /// `legion-pr-write` quality gate for HEAD so `legion pr create` can gate
    /// on it; on failure it records issues and exits non-zero with the gaps.
    WriteCheck {
        /// Repository name (resolves work source config from watch.toml)
        #[arg(long)]
        repo: String,

        /// Issue number whose acceptance criteria the body must map.
        #[arg(long)]
        issue: u64,

        /// Path to the drafted PR body (markdown). Reads stdin when omitted.
        #[arg(long)]
        body_file: Option<String>,
    },

    /// Edit a live pull request's title and/or body in place via the
    /// configured work source (#776).
    ///
    /// The honest alternative to close+recreate when a re-review finds a PR
    /// body that misdescribes its own diff: close+recreate loses the review
    /// thread and forces re-recording the legion-simplify and legion-pr-write
    /// gates on the new HEAD. `pr edit` corrects the live PR instead.
    ///
    /// When `--body-file` is given, the new body runs the same structural
    /// pr-write validation `pr write-check` runs (against `--issue`'s
    /// acceptance criteria) before it is ever sent to the work source, and a
    /// clean result is recorded as the `legion-pr-write` gate for local HEAD.
    /// That gate is refused -- not recorded -- when local HEAD is not the
    /// PR's own head commit, so the recorded gate never attests a commit this
    /// checkout was not actually on.
    Edit {
        /// Repository name (resolves work source config from watch.toml)
        #[arg(long)]
        repo: String,

        /// PR number
        #[arg(long)]
        number: u64,

        /// New PR title. At least one of --title / --body-file is required.
        #[arg(long)]
        title: Option<String>,

        /// Path to the corrected PR body (markdown). Runs the same
        /// pr-write validation as `pr write-check` and re-records the
        /// legion-pr-write gate for HEAD. Requires --issue.
        #[arg(long)]
        body_file: Option<String>,

        /// Issue number whose acceptance criteria the body must map.
        /// Required when --body-file is given.
        #[arg(long)]
        issue: Option<u64>,
    },

    /// List comments on a pull request (top-level + inline review comments)
    /// in chronological order.
    Comments {
        /// Repository name (resolves work source config from watch.toml)
        #[arg(long)]
        repo: String,

        /// PR number
        #[arg(long)]
        number: u64,

        /// Emit raw JSON instead of human-formatted output
        #[arg(long)]
        json: bool,
    },

    /// List submitted reviews on a pull request with their bodies and
    /// inline comments.
    Reviews {
        /// Repository name (resolves work source config from watch.toml)
        #[arg(long)]
        repo: String,

        /// PR number
        #[arg(long)]
        number: u64,

        /// Emit raw JSON instead of human-formatted output
        #[arg(long)]
        json: bool,
    },

    /// Post a review on a pull request
    Review {
        /// Repository name (resolves work source config from watch.toml)
        #[arg(long)]
        repo: String,

        /// PR number
        #[arg(long)]
        number: u64,

        /// Approve the PR
        #[arg(long, group = "verdict")]
        approve: bool,

        /// Request changes on the PR
        #[arg(long, group = "verdict")]
        request_changes: bool,

        /// Review body text
        #[arg(long)]
        body: Option<String>,

        /// Path to JSON file with inline comments (GitHub API format)
        #[arg(long)]
        comments: Option<String>,
    },

    /// Merge an approved pull request
    Merge {
        /// Repository name (resolves work source config from watch.toml)
        #[arg(long)]
        repo: String,

        /// PR number
        #[arg(long)]
        number: u64,

        /// Merge strategy: squash (default), merge, rebase
        #[arg(long, default_value = "squash", value_parser = ["squash", "merge", "rebase"])]
        strategy: String,

        /// Keep the branch after merging (default: delete)
        #[arg(long)]
        keep_branch: bool,

        /// Kanban card ID to transition to done
        #[arg(long)]
        task: Option<String>,

        /// Operator override: merge despite failing check-runs on the head
        /// SHA. Writes an audit entry naming the failing checks (mirrors
        /// `pr create --skip-gates`). Does not affect the #736 zero-runs
        /// refusal, which still fires first -- this only overrides the
        /// present-and-failing case.
        #[arg(long)]
        merge_despite_failures: bool,
    },

    /// Close a pull request without merging
    Close {
        /// Repository name (resolves work source config from watch.toml)
        #[arg(long)]
        repo: String,

        /// PR number
        #[arg(long)]
        number: u64,

        /// Comment to post before closing
        #[arg(long)]
        reason: Option<String>,

        /// Delete the remote branch after closing
        #[arg(long)]
        delete_branch: bool,
    },
}

/// Shared refusal for a PR head with zero check runs (#736). `legion pr
/// checks` and `legion pr merge` both hit this state and must present
/// identical wording so the merge gate's failure reads the same everywhere
/// it can fire.
fn no_runs_for_head_error(head_sha: &str, number: u64, source_repo: &str) -> error::LegionError {
    error::LegionError::WorkSource(format!(
        "no runs for head {head_sha}: PR #{number} on {source_repo} has zero check runs \
         for its head commit (not the branch's last suite)"
    ))
}

/// Shared refusal for a PR head with one or more check-runs in a failing
/// state (see `ExternalPRCheck::is_failing`). `legion pr checks` and `legion
/// pr merge` both classify with the same predicate and must name the same
/// failing checks the same way.
fn failing_checks_error(failed: &[&str], number: u64) -> error::LegionError {
    error::LegionError::WorkSource(format!(
        "{} check(s) failed on PR #{}: {}",
        failed.len(),
        number,
        failed.join(", ")
    ))
}

/// Load `issue`'s acceptance criteria, validate `body` against them, and
/// record the result as the `legion-pr-write` quality gate for
/// `(branch, commit_hash)`. Shared by `PrAction::WriteCheck` and
/// `PrAction::Edit`'s `--body-file` path -- both load the issue, validate,
/// and record identically; only what surrounds this differs (Edit
/// cross-checks the PR's head SHA first and phrases its own advice; the
/// closing-keyword warning stays with each caller since the fix differs --
/// `--closes` exists for `pr create`, not for editing a live PR).
///
/// This IS `legion-pr-write`'s check validator (#780): `pr_write::validate_pr_body`
/// plays the same role `simplify_check::validate_articulation` plays for
/// `legion-simplify` -- a structural gate the result must pass before the
/// row is written -- so the row it records always carries VALIDATED
/// provenance, mirroring `cli::verify`'s `Check` action.
#[allow(clippy::too_many_arguments)]
fn validate_and_record_pr_write_gate(
    plugin_name: &str,
    source_repo: &str,
    issue: u64,
    body: &str,
    branch: &str,
    commit_hash: &str,
    database: &db::Database,
    via: &str,
) -> error::Result<pr_write::PrWriteReport> {
    let ext = worksource::view_issue(plugin_name, source_repo, issue)?;
    let parsed = card_parse::parse_issue_body(ext.body.as_deref().unwrap_or(""));
    let report = pr_write::validate_pr_body(&parsed.acceptance, body);

    let gate_result = if report.ok {
        GateResult::Clean
    } else {
        GateResult::Issues
    };
    let details = serde_json::json!({
        "skill": "legion-pr-write",
        "issue": issue,
        "mapping_entries": report.mapping_entries,
        "findings": report.findings,
        "via": via,
    })
    .to_string();
    let row = database.record_quality_gate(&QualityGateInput {
        branch,
        commit_hash,
        skill: "legion-pr-write",
        result: gate_result,
        findings_count: report.findings.len() as u64,
        details: Some(&details),
        provenance: GateProvenance::Validated,
        base: None,
    })?;
    crate::gate_trust::emit_gate_trust(database, &row);

    Ok(report)
}

pub(crate) fn handle(action: PrAction) -> error::Result<()> {
    match action {
        PrAction::Create {
            repo,
            title,
            body,
            base,
            head,
            draft,
            labels,
            reviewer,
            task,
            skip_gates,
            closes,
        } => {
            // One database handle for the whole arm (#610): the gate
            // check, the skip-gates audit entry, the card link, the
            // create-pr audit row, and the review auto-signal all share
            // it instead of opening up to four separate connections, two
            // of which silently swallowed open failures.
            let database = open_db()?;

            // Quality gates: verify legion-simplify AND legion-pr-write ran
            // clean on HEAD before calling the work source. This runs before
            // worksource resolution so the gate error is always surfaced,
            // even if the worksource is not configured. --skip-gates is a
            // bootstrap escape hatch for the branch that ships the skills
            // themselves; it writes an audit entry so it cannot be done silently.
            if skip_gates {
                audit(
                    &database,
                    &db::AuditInput {
                        agent: &repo,
                        action: "skip-gate-bootstrap",
                        target_type: "pr",
                        target_ref: "pending",
                        task_id: task.as_deref(),
                        source_type: "unknown",
                        details: Some(
                            r#"{"skills":["legion-simplify","legion-pr-write"],"reason":"bootstrap"}"#,
                        ),
                        outcome: "skipped",
                    },
                );
                eprintln!("[legion] warning: quality gate skipped (--skip-gates bootstrap)");
            } else {
                let (commit_hash, _branch) = git_head_commit_and_branch()?;
                let short_hash: &str = &commit_hash[..commit_hash.len().min(8)];

                // Both forcing-function gates must be clean on HEAD before the
                // PR opens: simplify (structural quality) and pr-write (the
                // articulated criterion-to-work mapping the verify gate later
                // audits). Each is recorded by its own skill on the current
                // commit, so a post-gate commit invalidates them (re-run).
                for (skill, run_hint) in [
                    ("legion-simplify", "/legion-simplify"),
                    ("legion-pr-write", "/legion-pr-write"),
                ] {
                    match database.get_quality_gate(&commit_hash, skill)? {
                        None => {
                            eprintln!(
                                "[legion] error: no clean {skill} gate on HEAD ({short_hash}). \
                                 Run {run_hint} before creating the PR."
                            );
                            return Err(error::LegionError::ExitWith(1));
                        }
                        Some(gate) if gate.result != GateResult::Clean => {
                            eprintln!(
                                "[legion] error: {skill} recorded issues on HEAD ({short_hash}), \
                                 {} findings. Fix them and re-run the skill before creating the PR.",
                                gate.findings_count
                            );
                            return Err(error::LegionError::ExitWith(1));
                        }
                        Some(_) => {
                            // Gate is clean -- continue to the next.
                        }
                    }
                }
            }

            // Parse --closes eagerly, before worksource resolution, so a
            // malformed value fails fast without needing a live PR round
            // trip (mirrors the gate checks above).
            let mut close_refs: Vec<pr_write::CloseRef> = Vec::with_capacity(closes.len());
            for spec in &closes {
                match pr_write::CloseRef::parse(spec) {
                    Ok(cr) => close_refs.push(cr),
                    Err(e) => {
                        eprintln!("[legion] error: {e}");
                        return Err(error::LegionError::ExitWith(1));
                    }
                }
            }

            let (plugin_name, source_repo, _workdir) = worksource::require_worksource(&repo)?;

            // Append a `Closes #N` line per --closes unless the body already
            // carries a recognized closing keyword for that issue (#751).
            let body = if close_refs.is_empty() {
                body
            } else {
                Some(pr_write::append_closing_lines(
                    body.as_deref().unwrap_or(""),
                    &close_refs,
                ))
            };

            let created = worksource::create_pr(
                &plugin_name,
                &source_repo,
                &title,
                body.as_deref(),
                base.as_deref(),
                head.as_deref(),
                draft,
                labels.as_deref(),
                reviewer.as_deref(),
            )?;

            // Store PR URL on the kanban card if linked
            if let Some(ref task_id) = task {
                // Update the card's context with the PR URL
                if let Some(_card) = database.get_card_by_id(task_id)? {
                    eprintln!("[legion] linked PR #{} to card {}", created.number, task_id);
                }
            }

            let details = serde_json::json!({
                "title": title, "base": base, "head": head, "draft": draft,
            });
            let details_str = details.to_string();
            audit(
                &database,
                &db::AuditInput {
                    agent: &repo,
                    action: "create-pr",
                    target_type: "pr",
                    target_ref: &created.number.to_string(),
                    task_id: task.as_deref(),
                    source_type: &plugin_name,
                    details: Some(&details_str),
                    outcome: "success",
                },
            );

            // Auto-signal review agent if configured
            if let Some(review_cfg) = worksource::resolve_review_config()
                && review_cfg.auto_signal
            {
                let signal_text = format!(
                    "@{} review:ready -- repo:{},pr:{},source:{}",
                    review_cfg.agent, repo, created.number, source_repo
                );
                // The PR already exists at this point, so a broken index
                // must not fail the command -- but it must be loud (#610).
                // The previous if-let-Ok chain swallowed open failures
                // without a trace.
                let signal_result = data_dir()
                    .and_then(|b| search::SearchIndex::open(&b.join("index")))
                    .and_then(|index| {
                        let meta = db::ReflectionMeta::default();
                        board::post_from_text_with_meta(
                            &database,
                            &index,
                            &repo,
                            &signal_text,
                            &meta,
                        )
                    });
                match signal_result {
                    Ok(_) => {
                        eprintln!("[legion] signaled @{} for review", review_cfg.agent)
                    }
                    Err(e) => {
                        eprintln!("[legion] warning: review signal failed: {}", e)
                    }
                }
            }

            println!("{}", created.url);
            eprintln!("[legion] created PR #{} on {}", created.number, source_repo);
        }
        PrAction::List { repo } => {
            let (plugin_name, source_repo, _workdir) = worksource::require_worksource(&repo)?;

            let prs = worksource::list_prs(&plugin_name, &source_repo)?;
            if prs.is_empty() {
                eprintln!("[legion] no open PRs on {}", source_repo);
            } else {
                for pr in &prs {
                    let review = pr.review_decision.as_deref().unwrap_or("PENDING");
                    let draft = if pr.is_draft { " [draft]" } else { "" };
                    println!(
                        "#{} {} ({}) {}{}",
                        pr.number, pr.title, pr.head_ref_name, review, draft
                    );
                }
            }
        }
        PrAction::Checks {
            repo,
            number,
            json,
            log_failed,
        } => {
            let (plugin_name, source_repo, _workdir) = worksource::require_worksource(&repo)?;

            let result = worksource::pr_checks(&plugin_name, &source_repo, number)?;
            let checks = &result.checks;

            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(checks)
                        .expect("ExternalPRCheck serializes infallibly")
                );
            } else if !checks.is_empty() {
                let mut name_w = 0usize;
                let mut wf_w = 0usize;
                for c in checks {
                    name_w = name_w.max(c.name.len());
                    wf_w = wf_w.max(c.workflow.len());
                }
                for c in checks {
                    println!(
                        "{:<8} {:<name_w$}  {:<wf_w$}  {}",
                        c.state,
                        c.name,
                        c.workflow,
                        c.link,
                        name_w = name_w,
                        wf_w = wf_w,
                    );
                }
            }

            if log_failed && !json {
                for c in checks.iter().filter(|c| c.is_failing()) {
                    match worksource::job_id_from_link(&c.link) {
                        Some(job_id) => {
                            println!("\n===== {} ({}) =====", c.name, job_id);
                            match worksource::fetch_check_log(&plugin_name, &source_repo, job_id) {
                                Ok(log) => print!("{log}"),
                                Err(e) => {
                                    println!("(log unavailable: {e})");
                                }
                            }
                        }
                        None => {
                            println!(
                                "\n===== {} =====\n(log unavailable: non-Actions check link: {})",
                                c.name, c.link
                            );
                        }
                    }
                }
            }

            // Zero runs for the pinned head SHA is a distinct non-passing
            // state, not a vacuous pass (#736): a fresh push whose CI has not
            // started yet, or gh's checks view lagging the head commit, must
            // not read as "nothing failing" to a merge gate.
            if checks.is_empty() {
                return Err(no_runs_for_head_error(
                    &result.head_sha,
                    number,
                    &source_repo,
                ));
            }

            let failed: Vec<&str> = checks
                .iter()
                .filter(|c| c.is_failing())
                .map(|c| c.name.as_str())
                .collect();
            if !failed.is_empty() {
                return Err(failing_checks_error(&failed, number));
            }
        }
        PrAction::View { repo, number, json } => {
            let (plugin_name, source_repo, _workdir) = worksource::require_worksource(&repo)?;

            let pr = worksource::view_pr(&plugin_name, &source_repo, number)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&pr)
                        .expect("ExternalPRDetails serializes infallibly")
                );
            } else {
                pr_view::render_pr(&pr);
            }
        }
        PrAction::WriteCheck {
            repo,
            issue,
            body_file,
        } => {
            let (plugin_name, source_repo, _workdir) = worksource::require_worksource(&repo)?;

            // Read the drafted body from --body-file, or stdin when omitted.
            let body = read_file_or_stdin(body_file.as_deref(), "--body-file")?;

            // Warn (never fail, v1 -- #751) when the body lacks a recognized
            // GitHub closing keyword for this issue. Printed unconditionally,
            // ahead of the ok/fail branch below, so a body that also fails
            // the mapping check still surfaces the missing linkage.
            if !pr_write::has_closing_keyword(&body, &pr_write::CloseRef::same_repo(issue)) {
                eprintln!(
                    "[legion] warning: body has no closing keyword for issue #{issue} -- add \
                     'Closes #{issue}' (or pass `--closes {issue}` to `pr create`, which appends \
                     it automatically) so the merge auto-closes it."
                );
            }

            // Load the issue's acceptance criteria, validate the body, and
            // record the gate on HEAD so `legion pr create` can gate on it,
            // exactly as it does for legion-simplify.
            let (commit_hash, branch) = git_head_commit_and_branch()?;
            let database = open_db()?;
            let report = validate_and_record_pr_write_gate(
                &plugin_name,
                &source_repo,
                issue,
                &body,
                &branch,
                &commit_hash,
                &database,
                "pr write-check",
            )?;

            if report.ok {
                println!(
                    "[legion] pr-write gate clean for issue #{issue} ({} mapping entries). \
                     Open the PR with this body.",
                    report.mapping_entries
                );
            } else {
                eprintln!(
                    "[legion] pr-write gate FAILED for issue #{issue} -- {} gap(s):",
                    report.findings.len()
                );
                for f in &report.findings {
                    eprintln!("  - {f}");
                }
                eprintln!(
                    "\nThe mapping must be composed prose -- one entry per acceptance \
                     criterion, each citing evidence (a test, a file:line, an observable \
                     behavior) -- plus a 'Not done' section. Fix the body and re-run."
                );
                return Err(error::LegionError::ExitWith(1));
            }
        }
        PrAction::Edit {
            repo,
            number,
            title,
            body_file,
            issue,
        } => {
            if title.is_none() && body_file.is_none() {
                eprintln!("[legion] error: at least one of --title or --body-file is required");
                return Err(error::LegionError::ExitWith(1));
            }

            let (plugin_name, source_repo, _workdir) = worksource::require_worksource(&repo)?;
            let database = open_db()?;

            // Validate + gate the new body BEFORE touching the live PR: a
            // boilerplate/unmapped body must never reach `gh pr edit`, and a
            // half-applied edit (title changed, body rejected) is worse than
            // no edit at all.
            let body: Option<String> = match &body_file {
                None => None,
                Some(bf) => {
                    let issue = issue.ok_or_else(|| {
                        error::LegionError::WorkSource(
                            "--issue is required when --body-file is given (loads the \
                             acceptance criteria the body must map, same as `pr write-check`)"
                                .to_owned(),
                        )
                    })?;

                    let new_body = read_file_or_stdin(Some(bf), "--body-file")?;

                    if !pr_write::has_closing_keyword(
                        &new_body,
                        &pr_write::CloseRef::same_repo(issue),
                    ) {
                        eprintln!(
                            "[legion] warning: body has no closing keyword for issue #{issue} \
                             -- add 'Closes #{issue}' so the merge auto-closes it."
                        );
                    }

                    // Gate-keying corner (#776 build notes): `record_quality_gate`
                    // keys off the LOCAL checkout HEAD, not the PR's remote head
                    // SHA. A pr-write gate re-recorded here must attest the commit
                    // the live PR actually carries -- otherwise running this from
                    // a stale or unrelated checkout would record a clean gate for
                    // a commit the PR was never built from. Refuse before ever
                    // loading the issue or recording anything when the two
                    // disagree.
                    let (commit_hash, branch) = git_head_commit_and_branch()?;
                    let pr_checks = worksource::pr_checks(&plugin_name, &source_repo, number)?;
                    if pr_checks.head_sha != commit_hash {
                        eprintln!(
                            "[legion] error: local HEAD ({}) does not match PR #{number}'s head \
                             commit ({}) -- refusing to record a pr-write gate for a commit this \
                             checkout is not on. Check out the PR's branch before editing its body.",
                            &commit_hash[..commit_hash.len().min(8)],
                            &pr_checks.head_sha[..pr_checks.head_sha.len().min(8)],
                        );
                        return Err(error::LegionError::ExitWith(1));
                    }

                    let report = validate_and_record_pr_write_gate(
                        &plugin_name,
                        &source_repo,
                        issue,
                        &new_body,
                        &branch,
                        &commit_hash,
                        &database,
                        "pr edit",
                    )?;

                    if !report.ok {
                        eprintln!(
                            "[legion] pr-write gate FAILED for issue #{issue} -- {} gap(s):",
                            report.findings.len()
                        );
                        for f in &report.findings {
                            eprintln!("  - {f}");
                        }
                        return Err(error::LegionError::ExitWith(1));
                    }

                    Some(new_body)
                }
            };

            worksource::edit_pr(
                &plugin_name,
                &source_repo,
                number,
                title.as_deref(),
                body.as_deref(),
            )?;

            let details = serde_json::json!({
                "title_changed": title.is_some(),
                "body_changed": body.is_some(),
            });
            let details_str = details.to_string();
            audit(
                &database,
                &db::AuditInput {
                    agent: &repo,
                    action: "edit-pr",
                    target_type: "pr",
                    target_ref: &number.to_string(),
                    task_id: None,
                    source_type: &plugin_name,
                    details: Some(&details_str),
                    outcome: "success",
                },
            );

            eprintln!("[legion] edited PR #{} on {}", number, source_repo);
        }
        PrAction::Comments { repo, number, json } => {
            let (plugin_name, source_repo, _workdir) = worksource::require_worksource(&repo)?;

            let comments = worksource::list_pr_comments(&plugin_name, &source_repo, number)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&comments)
                        .expect("ExternalPRComment serializes infallibly")
                );
            } else if comments.is_empty() {
                eprintln!("[legion] no comments on PR #{} on {}", number, source_repo);
            } else {
                pr_view::render_comments(&comments);
            }
        }
        PrAction::Reviews { repo, number, json } => {
            let (plugin_name, source_repo, _workdir) = worksource::require_worksource(&repo)?;

            let reviews = worksource::list_pr_reviews(&plugin_name, &source_repo, number)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&reviews)
                        .expect("ExternalPRReview serializes infallibly")
                );
            } else if reviews.is_empty() {
                eprintln!("[legion] no reviews on PR #{} on {}", number, source_repo);
            } else {
                pr_view::render_reviews(&reviews);
            }
        }
        PrAction::Review {
            repo,
            number,
            approve,
            request_changes,
            body,
            comments,
        } => {
            let (plugin_name, source_repo, _workdir) = worksource::require_worksource(&repo)?;

            let event = if approve {
                "APPROVE"
            } else if request_changes {
                "REQUEST_CHANGES"
            } else {
                // COMMENT requires a body or inline comments
                if body.is_none() && comments.is_none() {
                    return Err(error::LegionError::WorkSource(
                        "comment review requires --body or --comments".to_string(),
                    ));
                }
                "COMMENT"
            };

            let database = open_db()?;
            worksource::review_pr(
                &plugin_name,
                &source_repo,
                number,
                event,
                body.as_deref(),
                comments.as_deref(),
            )?;

            let details = serde_json::json!({ "event": event });
            let details_str = details.to_string();
            audit(
                &database,
                &db::AuditInput {
                    agent: &repo,
                    action: "review",
                    target_type: "pr",
                    target_ref: &number.to_string(),
                    task_id: None,
                    source_type: &plugin_name,
                    details: Some(&details_str),
                    outcome: "success",
                },
            );

            eprintln!(
                "[legion] posted {} review on PR #{} on {}",
                event, number, source_repo
            );
        }
        PrAction::Merge {
            repo,
            number,
            strategy,
            keep_branch,
            task,
            merge_despite_failures,
        } => {
            let (plugin_name, source_repo, _workdir) = worksource::require_worksource(&repo)?;
            let database = open_db()?;

            // Same fail-closed gate as `legion pr checks` (#736): a head SHA
            // with zero check runs must refuse the merge, not inherit the
            // branch's last-green suite through some other path. Checked
            // here (not left to branch protection) because a repo without a
            // required-status-checks rule would otherwise let an untested
            // head merge silently.
            let checks_result = worksource::pr_checks(&plugin_name, &source_repo, number)?;
            if checks_result.checks.is_empty() {
                return Err(no_runs_for_head_error(
                    &checks_result.head_sha,
                    number,
                    &source_repo,
                ));
            }

            // #761: gate on PRESENT-AND-FAILING check-runs too, not just
            // absence. Same classification `legion pr checks` uses
            // (`ExternalPRCheck::is_failing`) so a red required check-run
            // cannot be merged through legion without branch protection
            // noticing -- this matters on repos with admin-bypass merge
            // rules, where branch protection alone would not stop it.
            // `--merge-despite-failures` is the operator's audited escape
            // hatch (mirrors `pr create --skip-gates`): it does not touch
            // the zero-runs refusal above, which still fires first.
            let failed: Vec<&str> = checks_result
                .checks
                .iter()
                .filter(|c| c.is_failing())
                .map(|c| c.name.as_str())
                .collect();
            if !failed.is_empty() {
                if !merge_despite_failures {
                    return Err(failing_checks_error(&failed, number));
                }
                let details = serde_json::json!({ "failing_checks": failed }).to_string();
                audit(
                    &database,
                    &db::AuditInput {
                        agent: &repo,
                        action: "merge-despite-failures",
                        target_type: "pr",
                        target_ref: &number.to_string(),
                        task_id: task.as_deref(),
                        source_type: &plugin_name,
                        details: Some(&details),
                        outcome: "overridden",
                    },
                );
                eprintln!(
                    "[legion] warning: merging PR #{} despite {} failing check(s): {} \
                     (--merge-despite-failures override, audit-logged)",
                    number,
                    failed.len(),
                    failed.join(", ")
                );
            }

            let merge_outcome =
                worksource::merge_pr(&plugin_name, &source_repo, number, &strategy, !keep_branch)?;

            // A queued PR (#630) has not actually merged yet -- the base
            // branch's merge queue completes it asynchronously, possibly
            // after re-running CI. Firing the kanban-done transition and
            // issue-close side effects here would mark the card/issue done
            // before the merge has happened, so both are skipped when
            // queued and left for the queue's own completion to be
            // reconciled later.
            if !merge_outcome.queued {
                // Transition kanban card to done if linked
                if let Some(ref task_id) = task {
                    match kanban::transition_card(
                        &database,
                        task_id,
                        kanban::Action::Done,
                        Some(&format!("PR #{} merged", number)),
                    ) {
                        Ok(_) => eprintln!("[legion] card {} marked done", task_id),
                        Err(e) => eprintln!(
                            "[legion] warning: could not complete card {}: {}",
                            task_id, e
                        ),
                    }

                    // Close the linked issue if the card has a source URL,
                    // using a generated merge comment so the GitHub issue
                    // records which PR merge closed it.
                    if let Some(card) = database.get_card_by_id(task_id)?
                        && let Some(ref url) = card.source_url
                        && let Some(issue_num) = worksource::extract_issue_number(url)
                        && let Some(ref source) = card.source_type
                    {
                        let merge_comment =
                            format!("Closed by PR #{number} merge via legion kanban propagation.");
                        if let Err(e) = worksource::close_issue(
                            source,
                            &source_repo,
                            issue_num,
                            Some(&merge_comment),
                        ) {
                            eprintln!(
                                "[legion] warning: could not close issue #{}: {}",
                                issue_num, e
                            );
                        } else {
                            eprintln!("[legion] closed issue #{}", issue_num);
                        }
                    }
                }
            }

            // delete_branch records the effective outcome, not just the
            // request: the queue path never passes --delete-branch (branch
            // deletion after a queue merge is the queue/repo's own setting,
            // not something this CLI flag controls), so recording
            // `!keep_branch` there would claim an effect that did not
            // happen.
            let details = serde_json::json!({
                "strategy": strategy,
                "delete_branch": !keep_branch && !merge_outcome.queued,
                "queued": merge_outcome.queued,
            });
            let details_str = details.to_string();
            audit(
                &database,
                &db::AuditInput {
                    agent: &repo,
                    action: "merge",
                    target_type: "pr",
                    target_ref: &number.to_string(),
                    task_id: task.as_deref(),
                    source_type: &plugin_name,
                    details: Some(&details_str),
                    outcome: "success",
                },
            );

            if merge_outcome.queued {
                println!(
                    "PR #{} enqueued on {} merge queue -- the queue will complete the merge",
                    number, source_repo
                );
            } else {
                println!("PR #{} merged on {}", number, source_repo);
            }
        }
        PrAction::Close {
            repo,
            number,
            reason,
            delete_branch,
        } => {
            let (plugin_name, source_repo, _workdir) = worksource::require_worksource(&repo)?;
            let database = open_db()?;

            worksource::close_pr(
                &plugin_name,
                &source_repo,
                number,
                reason.as_deref(),
                delete_branch,
            )?;

            let details = serde_json::json!({
                "reason": reason, "delete_branch": delete_branch,
            });
            let details_str = details.to_string();
            audit(
                &database,
                &db::AuditInput {
                    agent: &repo,
                    action: "close-pr",
                    target_type: "pr",
                    target_ref: &number.to_string(),
                    task_id: None,
                    source_type: &plugin_name,
                    details: Some(&details_str),
                    outcome: "success",
                },
            );

            println!("closed PR #{} on {}", number, source_repo);
        }
    }
    Ok(())
}
