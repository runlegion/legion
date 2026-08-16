//! `legion issue`/`sub-issue`/`comment` handlers (carved from main.rs, #610).

use clap::Subcommand;

use crate::cli::util::{audit, open_db};
use crate::db::card_criteria;
use crate::{card_parse, db, error, worksource};

/// Validate an issue body's `## Traces to` section before the issue is
/// created on the work source (#933 create-time refusals).
///
/// Untraced is legal and the common case: only new work traces to a
/// requirement, so an empty `trace` (no `## Traces to` section at all)
/// returns `Ok(())` immediately -- defect work is anchored by its stated
/// premise instead, not a requirement.
///
/// Refusals, matching the trace format contract exactly:
/// - `None` alongside a requirement bullet -- an issue either traces to at
///   least one requirement or explicitly declares it has none, not both.
/// - `- None` with no reason: only new work has a spec, so the no-spec case
///   must say why, not just assert it.
/// - A requirement id that does not resolve to a document.
/// - A requirement whose document status is `cancelled` -- a cancelled
///   requirement is not a trace.
/// - A `[criteria: ...]` bracket citing an id the requirement's current
///   `verification.criteria` does not contain.
fn validate_trace(database: &db::Database, trace: &[card_parse::TraceBullet]) -> error::Result<()> {
    if trace.is_empty() {
        return Ok(());
    }

    let has_requirement = trace
        .iter()
        .any(|t| matches!(t, card_parse::TraceBullet::Requirement { .. }));
    let has_none = trace
        .iter()
        .any(|t| matches!(t, card_parse::TraceBullet::NoRequirement { .. }));
    if has_requirement && has_none {
        return Err(error::LegionError::WorkSource(
            "## Traces to: '- None' cannot appear alongside a requirement bullet -- an issue \
             either traces to at least one requirement, or explicitly declares it has none, \
             not both."
                .to_string(),
        ));
    }

    for bullet in trace {
        // Bracket defects the tolerant parser degrades instead of refusing
        // (#945 review): unclosed/repeated brackets buried in prose, empty
        // brackets scoping zero criteria. One definition, shared with the
        // live re-check in cli::verify::resolve_traced_requirements.
        if let Some(defect) = card_parse::trace_bullet_bracket_defect(bullet) {
            return Err(error::LegionError::WorkSource(format!(
                "## Traces to: {defect}"
            )));
        }
        match bullet {
            card_parse::TraceBullet::NoRequirement { reason } => {
                if reason.as_deref().unwrap_or("").trim().is_empty() {
                    return Err(error::LegionError::WorkSource(
                        "## Traces to: '- None' requires a reason ('- None -- <reason>') -- \
                         only new work has a spec; state why this issue has none."
                            .to_string(),
                    ));
                }
            }
            card_parse::TraceBullet::Requirement {
                document_id,
                criteria,
                ..
            } => {
                let doc = database.get_document(document_id)?.ok_or_else(|| {
                    error::LegionError::WorkSource(format!(
                        "## Traces to cites requirement '{document_id}', which does not exist"
                    ))
                })?;
                if doc.status == "cancelled" {
                    return Err(error::LegionError::WorkSource(format!(
                        "## Traces to cites requirement '{document_id}', which is cancelled -- \
                         a cancelled requirement is not a trace"
                    )));
                }
                if let Some(ids) = criteria {
                    let valid = card_criteria::valid_criterion_ids(&doc.payload);
                    for id in ids {
                        if !valid.contains(id) {
                            return Err(error::LegionError::WorkSource(format!(
                                "## Traces to cites criterion '{id}' [criteria: ...] for \
                                 requirement '{document_id}', which does not contain it"
                            )));
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// Gate `legion issue close` on a clean verify verdict (#930).
///
/// Returns a short label for the audit row describing which path was taken.
///
/// Why this lives here rather than only on the card path: `verify` is the only
/// card-keyed gate, and until now the ONLY place it was enforced was
/// `handle_done`'s card-keyed lookup (`cli/kanban.rs`). `legion issue close`
/// checked nothing, and the issue-keyed verdict added for card-free repos is
/// exit-code-only -- the row is recorded and nothing ever reads it back. So for
/// any repo working from issues rather than cards, which is now most of them,
/// verify was advisory. smugglr ran an entire epic in which simplify, pr-write
/// and review all fired, verify never did, and nothing reported the absence.
///
/// The card path does NOT pass through here: `legion done` closes its linked
/// issue via `propagate_card_close_to_worksource`, which resolves the work
/// source itself and calls `close_issue` directly. That path is gated by
/// `handle_done`'s own check, so the two do not double-gate and neither leaves
/// a hole.
///
/// An issue declaring no acceptance criteria AND no trace is not gated --
/// matching the card rule, where a chore with no criteria can reach Done.
/// But the ungated close SAYS SO on stdout rather than passing silently: an
/// unchecked close and a checked one must not look identical, which is the
/// failure this whole gate exists to close.
///
/// #933: a traced issue's own `## Acceptance criteria` section is
/// legitimately empty -- the requirement's criteria are what verify judges,
/// not a restatement on the issue -- so a `## Traces to` section naming at
/// least one requirement counts as "has criteria" here even when
/// `acceptance` is empty. Without this, a traced issue with no restated
/// criteria would close ungated: #930's hole, reopened through the door
/// #933 opened.
fn check_verify_before_close(
    database: &db::Database,
    plugin_name: &str,
    source_repo: &str,
    number: u64,
    force: bool,
    force_reason: Option<&str>,
) -> error::Result<&'static str> {
    use crate::verify::{self, GateResult};

    // Fail closed when the issue cannot be read: an unreadable work source
    // means the gate could not be EVALUATED, which is not the same as passing
    // it. This makes `view-issue` a hard requirement of the close path -- a
    // plugin implementing only `close` now needs it too -- so the error says
    // that plainly rather than surfacing as an opaque close failure.
    //
    // `--force` is honoured here rather than after, so the escape hatch the
    // error advertises actually exists on this branch.
    let ext = match worksource::view_issue(plugin_name, source_repo, number) {
        Ok(ext) => ext,
        Err(e) if force => {
            eprintln!(
                "[legion] OVERRIDE: could not read #{number} to evaluate the verify gate ({e}); \
                 closing anyway."
            );
            eprintln!(
                "[legion] reason recorded: {}",
                force_reason.unwrap_or("(none)")
            );
            return Ok("overridden: issue unreadable");
        }
        Err(e) => {
            return Err(error::LegionError::WorkSource(format!(
                "cannot evaluate the verify gate for #{number}: reading the issue failed ({e}). \
                 The close path needs the work source's view-issue verb. Use --force with a \
                 reason to close without the check."
            )));
        }
    };
    let parsed = card_parse::parse_issue_body(ext.body.as_deref().unwrap_or(""));
    let acceptance = parsed.acceptance;
    let has_trace = parsed
        .trace
        .iter()
        .any(|t| matches!(t, card_parse::TraceBullet::Requirement { .. }));

    if acceptance.is_empty() && !has_trace {
        println!(
            "[legion] note: #{number} declares no acceptance criteria, so no verify verdict was required."
        );
        return Ok("ungated: no acceptance criteria");
    }

    let skill = verify::verify_gate_key_for_issue(source_repo, number);
    let latest = database.get_latest_quality_gate_by_skill(&skill)?;

    // What the issue is judged against, for the refusal message: its own
    // restated criteria, or (#933) the requirement its trace resolves to
    // when it declared none of its own.
    let criteria_desc = if acceptance.is_empty() {
        "criteria from its traced requirement".to_string()
    } else {
        format!("{} acceptance criteria", acceptance.len())
    };

    // Absent and failed are different problems with different remedies, so
    // they get different refusals rather than one "not clean" message.
    let refusal = match &latest {
        Some(gate) if gate.result == GateResult::Clean => None,
        Some(_) => Some(format!(
            "verify verdict for #{number} is not clean ({criteria_desc}). \
             Resolve the failing or uncertain criteria and re-run \
             `legion verify --repo <r> --issue {number}`.",
        )),
        None => Some(format!(
            "#{number} declares {criteria_desc} but no verify verdict exists. \
             Run `legion verify --repo <r> --issue {number}` before closing.",
        )),
    };

    match (refusal, force) {
        (None, _) => Ok("clean"),
        (Some(reason), false) => {
            eprintln!("[legion] refusing to close: {reason}");
            eprintln!(
                "[legion] override with --force --force-reason \"...\" if this is deliberate; \
                 the reason is recorded in the audit log."
            );
            Err(error::LegionError::ExitWith(1))
        }
        (Some(reason), true) => {
            // Loud on the way through. A recorded override that nobody sees at
            // the moment of use is only half a control.
            eprintln!("[legion] OVERRIDE: closing #{number} despite the verify gate. {reason}");
            eprintln!(
                "[legion] reason recorded: {}",
                force_reason.unwrap_or("(none)")
            );
            Ok("overridden")
        }
    }
}

/// Render the acceptance-criteria section of `legion issue view` in the form
/// `card_parse::parse_issue_body` can read BACK (#907).
///
/// This is a round-trip contract, not a formatting preference. Agents author
/// new issues by mirroring what this viewer prints, so when it emitted
/// `Acceptance criteria:` -- bare line, trailing colon, no `## ` -- it taught a
/// shape the parser scores as ZERO criteria, which then silently relaxed the
/// pr-write gate to a one-entry bar. The viewer's output format and the
/// parser's input format have to be the same format, and
/// `rendered_acceptance_round_trips` is what keeps them that way.
fn render_acceptance_block(items: &[String]) -> String {
    let mut out = String::from("## Acceptance criteria\n");
    for item in items {
        out.push_str("- ");
        out.push_str(item);
        out.push('\n');
    }
    out
}

/// Render the `## Traces to` section in the exact form
/// `card_parse::parse_issue_body` reads BACK (#933), mirroring
/// `render_acceptance_block`'s #907 round-trip contract for the same
/// reason: `legion issue view` is what agents (including `legion-verify`,
/// which reads the issue to find the trace) look at, and a shape that does
/// not survive re-parsing teaches the wrong format silently.
fn render_trace_block(trace: &[card_parse::TraceBullet]) -> String {
    let mut out = String::from("## Traces to\n");
    for bullet in trace {
        match bullet {
            card_parse::TraceBullet::Requirement {
                document_id,
                criteria,
                prose,
            } => {
                out.push_str("- ");
                out.push_str(document_id);
                if let Some(ids) = criteria {
                    out.push_str(" [criteria: ");
                    out.push_str(&ids.join(", "));
                    out.push(']');
                }
                if let Some(p) = prose {
                    out.push_str(" -- ");
                    out.push_str(p);
                }
                out.push('\n');
            }
            card_parse::TraceBullet::NoRequirement { reason } => {
                out.push_str("- None");
                if let Some(r) = reason {
                    out.push_str(" -- ");
                    out.push_str(r);
                }
                out.push('\n');
            }
        }
    }
    out
}

#[derive(Subcommand)]
pub(crate) enum SubIssueAction {
    /// Create a child issue linked to a parent via GitHub's native
    /// sub-issue relationship (#462). The plugin looks up the parent
    /// node id first, errors if the parent does not exist, then
    /// creates the child and links via the addSubIssue mutation.
    Create {
        /// Repo containing the parent issue (used to resolve the work
        /// source plugin).
        #[arg(long)]
        repo: String,
        /// Parent issue number.
        #[arg(long)]
        parent: u64,
        /// Title of the new child issue.
        #[arg(long)]
        title: String,
        /// Optional body for the child issue. Reads from stdin when
        /// --body is omitted AND stdin is not a TTY.
        #[arg(long)]
        body: Option<String>,
    },
    /// List sub-issues of a parent (#462).
    List {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        parent: u64,
        /// State filter: open (default) | closed | all.
        #[arg(long, default_value = "open")]
        state: String,
        /// Emit as JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum IssueAction {
    /// Create an issue via the configured work source
    Create {
        /// Repository name (used to resolve work source config from watch.toml)
        #[arg(long)]
        repo: String,

        /// Issue title
        #[arg(long)]
        title: String,

        /// Issue body
        #[arg(long)]
        body: Option<String>,

        /// Comma-separated labels
        #[arg(long)]
        labels: Option<String>,

        /// Assignee login
        #[arg(long)]
        assignee: Option<String>,
    },
    /// View an issue (local card data + live GitHub state)
    View {
        /// Repository name
        #[arg(long)]
        repo: String,

        /// Issue number
        #[arg(long)]
        number: u64,
    },
    /// List work-source issues: number, title, state, updated-at (#750).
    ///
    /// Reads the work source's live state via the same plugin path as
    /// `issue view` -- not the local kanban cache, which `sync` can miss
    /// entirely (see #750's motivating groom, where #711 sat open although
    /// shipped because it had no local card at all).
    List {
        /// Repository name
        #[arg(long)]
        repo: String,

        /// State filter: open (default) | closed | all.
        #[arg(long, default_value = "open")]
        state: String,

        /// Filter by label (forwarded to the work source as-is).
        #[arg(long)]
        label: Option<String>,

        /// Emit as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Close an issue via the configured work source
    ///
    /// Used to reconcile a shipped kanban card with its public GitHub state
    /// when the card was closed through a path other than `pr merge --task`
    /// (which already auto-closes the linked issue). The optional `--comment`
    /// is posted on the issue before it transitions to closed.
    Close {
        /// Repository name (resolves work source config from watch.toml)
        #[arg(long)]
        repo: String,

        /// Issue number
        #[arg(long)]
        number: u64,

        /// Optional closing comment posted before the close
        #[arg(long)]
        comment: Option<String>,

        /// Close despite a failed or missing verify verdict (#930).
        ///
        /// The override is recorded in the audit log with the reason, so a
        /// bypass is visible afterwards rather than indistinguishable from a
        /// clean close. Requires a reason: an unexplained override is the
        /// thing this gate exists to stop.
        #[arg(long, requires = "force_reason")]
        force: bool,

        /// Why the verify verdict is being overridden. Required with --force.
        #[arg(long)]
        force_reason: Option<String>,
    },
    /// Reopen a previously closed issue via the configured work source
    ///
    /// Symmetrical with `close` for reverting a kanban transition that
    /// already propagated to GitHub.
    Reopen {
        /// Repository name (resolves work source config from watch.toml)
        #[arg(long)]
        repo: String,

        /// Issue number
        #[arg(long)]
        number: u64,

        /// Optional reopening comment posted after the reopen
        #[arg(long)]
        comment: Option<String>,
    },
    /// Edit the title and/or body of an existing issue
    ///
    /// At least one of `--title` or `--body` must be provided. Used for
    /// scope amendments and stale-content fixes after a sync, so agents
    /// do not have to drop scope addenda into comment threads where they
    /// are buried below fold on the public GitHub view.
    Edit {
        /// Repository name (resolves work source config from watch.toml)
        #[arg(long)]
        repo: String,

        /// Issue number
        #[arg(long)]
        number: u64,

        /// Replace the issue title
        #[arg(long)]
        title: Option<String>,

        /// Replace the issue body
        #[arg(long)]
        body: Option<String>,
    },
}

pub(crate) fn handle_sub_issue(action: SubIssueAction) -> error::Result<()> {
    match action {
        SubIssueAction::Create {
            repo,
            parent,
            title,
            body,
        } => {
            let (plugin, github_repo, _workdir) = worksource::require_worksource(&repo)?;

            // #945 review: child issues are issues -- the same create-time
            // trace refusals as `legion issue create`, or a malformed trace
            // rides in through the second creation entry point.
            let database = open_db()?;
            let parsed = card_parse::parse_issue_body(body.as_deref().unwrap_or(""));
            validate_trace(&database, &parsed.trace)?;

            let created = worksource::create_sub_issue(
                &plugin,
                &github_repo,
                parent,
                &title,
                body.as_deref(),
            )?;
            println!("{}", created.url);
        }
        SubIssueAction::List {
            repo,
            parent,
            state,
            json,
        } => {
            let (plugin, github_repo, _workdir) = worksource::require_worksource(&repo)?;
            let issues = worksource::list_sub_issues(&plugin, &github_repo, parent, Some(&state))?;
            if json {
                println!("{}", serde_json::to_string(&issues)?);
            } else if issues.is_empty() {
                println!("[legion] no sub-issues of #{parent} in {github_repo}");
            } else {
                for i in &issues {
                    println!("#{}\t{}\t{}", i.number, i.state, i.title);
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn handle(action: IssueAction) -> error::Result<()> {
    match action {
        IssueAction::Create {
            repo,
            title,
            body,
            labels,
            assignee,
        } => {
            let (plugin_name, source_repo, _workdir) = worksource::require_worksource(&repo)?;
            let database = open_db()?;

            // #933: validate the `## Traces to` section before the issue
            // ever reaches the work source -- an unresolvable, cancelled, or
            // over-cited trace is refused here, not discovered later at
            // verify or pr-write time.
            let parsed = card_parse::parse_issue_body(body.as_deref().unwrap_or(""));
            validate_trace(&database, &parsed.trace)?;

            let created = worksource::create_issue(
                &plugin_name,
                &source_repo,
                &title,
                body.as_deref(),
                labels.as_deref(),
                assignee.as_deref(),
            )?;

            let details = serde_json::json!({
                "title": title, "labels": labels, "assignee": assignee,
            });
            let details_str = details.to_string();
            audit(
                &database,
                &db::AuditInput {
                    agent: &repo,
                    action: "create-issue",
                    target_type: "issue",
                    target_ref: &created.number.to_string(),
                    task_id: None,
                    source_type: &plugin_name,
                    details: Some(&details_str),
                    outcome: "success",
                },
            );

            println!("{}", created.url);
            eprintln!(
                "[legion] created issue #{} on {}",
                created.number, source_repo
            );
        }
        IssueAction::View { repo, number } => {
            let (plugin_name, source_repo, _workdir) = worksource::require_worksource(&repo)?;

            let issue = worksource::view_issue(&plugin_name, &source_repo, number)?;
            let parsed = card_parse::parse_issue_body(issue.body.as_deref().unwrap_or(""));

            // Structured output
            println!("# {} #{}\n", issue.title, issue.number);

            if let Some(ref problem) = parsed.problem {
                println!("Problem: {}\n", problem);
            }
            if let Some(ref solution) = parsed.solution {
                println!("Solution: {}\n", solution);
            }
            if !parsed.acceptance.is_empty() {
                println!("{}", render_acceptance_block(&parsed.acceptance));
            }
            if !parsed.trace.is_empty() {
                println!("{}", render_trace_block(&parsed.trace));
            }
            for (heading, content) in &parsed.sections {
                println!("{}:\n{}\n", heading, content);
            }
            if let Some(ref body) = parsed.body {
                println!("{}\n", body);
            }

            println!("State: {}", issue.state);
            println!("URL: {}", issue.url);
        }
        IssueAction::List {
            repo,
            state,
            label,
            json,
        } => {
            if !matches!(state.as_str(), "open" | "closed" | "all") {
                return Err(error::LegionError::WorkSource(format!(
                    "legion issue list: --state must be one of open|closed|all, got '{state}'"
                )));
            }

            let (plugin_name, source_repo, _workdir) = worksource::require_worksource(&repo)?;
            let database = open_db()?;

            let issues =
                worksource::list_all_issues(&plugin_name, &source_repo, &state, label.as_deref())?;

            let details = serde_json::json!({ "state": state, "label": label });
            let details_str = details.to_string();
            audit(
                &database,
                &db::AuditInput {
                    agent: &repo,
                    action: "list-issues",
                    target_type: "issue",
                    target_ref: &source_repo,
                    task_id: None,
                    source_type: &plugin_name,
                    details: Some(&details_str),
                    outcome: "success",
                },
            );

            if json {
                println!("{}", serde_json::to_string(&issues)?);
            } else if issues.is_empty() {
                println!("[legion] no issues on {} (state={})", source_repo, state);
            } else {
                for i in &issues {
                    println!(
                        "#{}\t{}\t{}\t{}",
                        i.number,
                        i.state,
                        i.updated_at.as_deref().unwrap_or("-"),
                        i.title
                    );
                }
            }
        }
        IssueAction::Close {
            repo,
            number,
            comment,
            force,
            force_reason,
        } => {
            let (plugin_name, source_repo, _workdir) = worksource::require_worksource(&repo)?;
            let database = open_db()?;

            let gate = check_verify_before_close(
                &database,
                &plugin_name,
                &source_repo,
                number,
                force,
                force_reason.as_deref(),
            )?;

            worksource::close_issue(&plugin_name, &source_repo, number, comment.as_deref())?;

            // The override reason rides the audit row, not just stderr: a
            // bypass that is only visible in the terminal of whoever ran it
            // is not recorded at all.
            let details = serde_json::json!({
                "comment": comment,
                "verify": gate,
                "force_reason": force_reason,
            });
            let details_str = details.to_string();
            audit(
                &database,
                &db::AuditInput {
                    agent: &repo,
                    action: "close-issue",
                    target_type: "issue",
                    target_ref: &number.to_string(),
                    task_id: None,
                    source_type: &plugin_name,
                    details: Some(&details_str),
                    // A bypassed gate must be visible in the DEFAULT audit
                    // listing, which prints the outcome and not the details
                    // JSON. Recording the override only in details would put
                    // it where nobody looks unless they already suspect it --
                    // which is the failure this gate exists to close, one
                    // level up.
                    outcome: if gate.starts_with("overridden") {
                        "override"
                    } else {
                        "success"
                    },
                },
            );

            println!("closed issue #{} on {}", number, source_repo);
        }
        IssueAction::Reopen {
            repo,
            number,
            comment,
        } => {
            let (plugin_name, source_repo, _workdir) = worksource::require_worksource(&repo)?;
            let database = open_db()?;

            worksource::reopen_issue(&plugin_name, &source_repo, number, comment.as_deref())?;

            let details = serde_json::json!({ "comment": comment });
            let details_str = details.to_string();
            audit(
                &database,
                &db::AuditInput {
                    agent: &repo,
                    action: "reopen-issue",
                    target_type: "issue",
                    target_ref: &number.to_string(),
                    task_id: None,
                    source_type: &plugin_name,
                    details: Some(&details_str),
                    outcome: "success",
                },
            );

            println!("reopened issue #{} on {}", number, source_repo);
        }
        IssueAction::Edit {
            repo,
            number,
            title,
            body,
        } => {
            if title.is_none() && body.is_none() {
                return Err(error::LegionError::WorkSource(
                    "legion issue edit requires at least one of --title or --body".to_string(),
                ));
            }

            let (plugin_name, source_repo, _workdir) = worksource::require_worksource(&repo)?;
            let database = open_db()?;

            worksource::edit_issue(
                &plugin_name,
                &source_repo,
                number,
                title.as_deref(),
                body.as_deref(),
            )?;

            let details = serde_json::json!({
                "title_set": title.is_some(),
                "body_set": body.is_some(),
            });
            let details_str = details.to_string();
            audit(
                &database,
                &db::AuditInput {
                    agent: &repo,
                    action: "edit-issue",
                    target_type: "issue",
                    target_ref: &number.to_string(),
                    task_id: None,
                    source_type: &plugin_name,
                    details: Some(&details_str),
                    outcome: "success",
                },
            );

            println!("edited issue #{} on {}", number, source_repo);
        }
    }
    Ok(())
}

pub(crate) fn handle_comment(repo: String, number: u64, body: String) -> error::Result<()> {
    let (plugin_name, source_repo, _workdir) = worksource::require_worksource(&repo)?;
    let database = open_db()?;

    worksource::comment(&plugin_name, &source_repo, number, &body)?;

    audit(
        &database,
        &db::AuditInput {
            agent: &repo,
            action: "comment",
            target_type: "comment",
            target_ref: &number.to_string(),
            task_id: None,
            source_type: &plugin_name,
            details: None,
            outcome: "success",
        },
    );

    eprintln!("[legion] commented on #{} on {}", number, source_repo);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil::test_db;
    use crate::documents::DocumentMeta;

    fn seed_requirement(db: &db::Database, id: &str, status: Option<&str>) -> Vec<String> {
        let meta = DocumentMeta {
            id: Some(id),
            doc_type: "requirement",
            surface: None,
            status,
            priority: None,
            owner: "legion",
        };
        let payload = serde_json::json!({
            "verification": {
                "criteria": [
                    {"text": "first"},
                    {"text": "second"}
                ]
            }
        })
        .to_string();
        let doc = db.insert_document(&meta, &payload).expect("insert doc");
        let value: serde_json::Value = serde_json::from_str(&doc.payload).unwrap();
        value["verification"]["criteria"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap().to_string())
            .collect()
    }

    // -- #933: `validate_trace` create-time refusals -------------------------

    #[test]
    fn validate_trace_permits_no_trace_section() {
        let db = test_db();
        validate_trace(&db, &[]).expect("untraced issue is legal");
    }

    #[test]
    fn validate_trace_permits_valid_requirement_reference() {
        let db = test_db();
        let ids = seed_requirement(&db, "FR-TRACE-001", None);
        let trace = vec![card_parse::TraceBullet::Requirement {
            document_id: "FR-TRACE-001".to_owned(),
            criteria: Some(vec![ids[0].clone()]),
            prose: None,
        }];
        validate_trace(&db, &trace).expect("valid trace must be accepted");
    }

    #[test]
    fn validate_trace_permits_valid_none_bullet() {
        let db = test_db();
        let trace = vec![card_parse::TraceBullet::NoRequirement {
            reason: Some("defect fix, nothing upstream".to_owned()),
        }];
        validate_trace(&db, &trace).expect("a reasoned None bullet is legal");
    }

    #[test]
    fn validate_trace_refuses_nonexistent_document() {
        let db = test_db();
        let trace = vec![card_parse::TraceBullet::Requirement {
            document_id: "FR-NOPE-999".to_owned(),
            criteria: None,
            prose: None,
        }];
        let err = validate_trace(&db, &trace).unwrap_err();
        assert!(err.to_string().contains("does not exist"), "got: {err}");
    }

    #[test]
    fn validate_trace_refuses_cancelled_requirement() {
        let db = test_db();
        seed_requirement(&db, "FR-TRACE-CANCELLED", Some("cancelled"));
        let trace = vec![card_parse::TraceBullet::Requirement {
            document_id: "FR-TRACE-CANCELLED".to_owned(),
            criteria: None,
            prose: None,
        }];
        let err = validate_trace(&db, &trace).unwrap_err();
        assert!(err.to_string().contains("cancelled"), "got: {err}");
    }

    #[test]
    fn validate_trace_refuses_criteria_bracket_citing_unknown_id() {
        let db = test_db();
        seed_requirement(&db, "FR-TRACE-002", None);
        let trace = vec![card_parse::TraceBullet::Requirement {
            document_id: "FR-TRACE-002".to_owned(),
            criteria: Some(vec!["bogus-id".to_owned()]),
            prose: None,
        }];
        let err = validate_trace(&db, &trace).unwrap_err();
        assert!(
            err.to_string().contains("does not contain it"),
            "got: {err}"
        );
    }

    #[test]
    fn validate_trace_refuses_none_alongside_requirement_bullet() {
        let db = test_db();
        seed_requirement(&db, "FR-TRACE-003", None);
        let trace = vec![
            card_parse::TraceBullet::Requirement {
                document_id: "FR-TRACE-003".to_owned(),
                criteria: None,
                prose: None,
            },
            card_parse::TraceBullet::NoRequirement {
                reason: Some("contradiction".to_owned()),
            },
        ];
        let err = validate_trace(&db, &trace).unwrap_err();
        assert!(
            err.to_string().contains("cannot appear alongside"),
            "got: {err}"
        );
    }

    #[test]
    fn validate_trace_refuses_empty_criteria_bracket() {
        // #945 review: `[criteria:]` parses to Some(vec![]) and every
        // validation loop over it runs zero iterations -- without this
        // refusal the citation is a permanent no-op.
        let db = test_db();
        seed_requirement(&db, "FR-TRACE-EMPTY", None);
        let parsed = card_parse::parse_issue_body("## Traces to\n\n- FR-TRACE-EMPTY [criteria:]\n");
        let err = validate_trace(&db, &parsed.trace).unwrap_err();
        assert!(err.to_string().contains("cites no ids"), "got: {err}");
    }

    #[test]
    fn validate_trace_refuses_unclosed_criteria_bracket() {
        let db = test_db();
        seed_requirement(&db, "FR-TRACE-UNCLOSED", None);
        let parsed = card_parse::parse_issue_body(
            "## Traces to\n\n- FR-TRACE-UNCLOSED [criteria: a -- never closed\n",
        );
        let err = validate_trace(&db, &parsed.trace).unwrap_err();
        assert!(
            err.to_string().contains("unparsed '[criteria:'"),
            "got: {err}"
        );
    }

    #[test]
    fn validate_trace_refuses_none_bullet_missing_reason() {
        let db = test_db();
        let trace = vec![card_parse::TraceBullet::NoRequirement { reason: None }];
        let err = validate_trace(&db, &trace).unwrap_err();
        assert!(err.to_string().contains("requires a reason"), "got: {err}");
    }

    /// #907: the viewer's output must survive re-parsing. This is the
    /// regression that let `legion issue view` teach agents an unparseable
    /// shape -- the criteria came back as ZERO, which silently relaxed the
    /// pr-write gate rather than failing anywhere visible.
    #[test]
    fn rendered_acceptance_round_trips() {
        let items = vec![
            "Heartbeat refreshes only live leases".to_owned(),
            "Daemon bootstrap releases stale leases".to_owned(),
            "`cargo test` and `cargo clippy --all-targets` are clean".to_owned(),
        ];
        let rendered = render_acceptance_block(&items);
        let reparsed = card_parse::parse_issue_body(&rendered);
        assert_eq!(
            reparsed.acceptance, items,
            "what the viewer prints must parse back to the same criteria; got {:?}",
            reparsed.acceptance
        );
    }

    /// The specific shape that caused #907 must NOT be what we emit.
    #[test]
    fn rendered_acceptance_is_not_the_bare_colon_form() {
        let rendered = render_acceptance_block(&["Something".to_owned()]);
        assert!(
            rendered.starts_with("## Acceptance criteria\n"),
            "heading must be a parseable `## ` section, got: {rendered:?}"
        );
        assert!(
            !rendered.contains("Acceptance criteria:"),
            "the trailing-colon form is the bug, not the output"
        );
    }

    /// An empty criteria list must not emit a heading with nothing under it --
    /// `parse_issue_body` skips empty sections, so it would round-trip, but the
    /// caller guards on non-empty and this pins that the block is items-only.
    #[test]
    fn rendered_acceptance_contains_one_line_per_item() {
        let rendered = render_acceptance_block(&["a".to_owned(), "b".to_owned()]);
        assert_eq!(rendered.lines().count(), 3, "heading plus two items");
    }

    // -- #933: `render_trace_block` round-trip (mirrors #907's acceptance
    // round-trip contract) -----------------------------------------------

    /// `legion issue view` must print the trace in a shape that re-parses
    /// to the exact same structure -- otherwise the viewer teaches a shape
    /// the parser cannot read back, silently blinding `legion-verify` (which
    /// reads the issue to find the trace) the same way #907 blinded the
    /// pr-write gate.
    #[test]
    fn rendered_trace_round_trips_requirement_bullets() {
        let trace = vec![
            card_parse::TraceBullet::Requirement {
                document_id: "FR-EMAIL-003".to_owned(),
                criteria: Some(vec!["crit-1".to_owned(), "crit-2".to_owned()]),
                prose: Some("adds the retry path".to_owned()),
            },
            card_parse::TraceBullet::Requirement {
                document_id: "FR-EMAIL-004".to_owned(),
                criteria: None,
                prose: None,
            },
        ];
        let rendered = render_trace_block(&trace);
        let reparsed = card_parse::parse_issue_body(&rendered);
        assert_eq!(
            reparsed.trace, trace,
            "rendered trace must parse back to the same structure, got {:?}",
            reparsed.trace
        );
    }

    #[test]
    fn rendered_trace_round_trips_none_bullet() {
        let trace = vec![card_parse::TraceBullet::NoRequirement {
            reason: Some("defect fix, no requirement above it".to_owned()),
        }];
        let rendered = render_trace_block(&trace);
        let reparsed = card_parse::parse_issue_body(&rendered);
        assert_eq!(reparsed.trace, trace);
    }

    #[test]
    fn rendered_trace_starts_with_parseable_heading() {
        let rendered = render_trace_block(&[card_parse::TraceBullet::Requirement {
            document_id: "FR-X".to_owned(),
            criteria: None,
            prose: None,
        }]);
        assert!(
            rendered.starts_with("## Traces to\n"),
            "heading must be a parseable `## ` section, got: {rendered:?}"
        );
    }
}
