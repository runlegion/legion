//! `legion issue`/`sub-issue`/`comment` handlers (carved from main.rs, #610).

use clap::Subcommand;

use crate::cli::util::{audit, open_db};
use crate::{card_parse, db, error, worksource};

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
                println!("Acceptance criteria:");
                for item in &parsed.acceptance {
                    println!("  - {}", item);
                }
                println!();
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
        IssueAction::Close {
            repo,
            number,
            comment,
        } => {
            let (plugin_name, source_repo, _workdir) = worksource::require_worksource(&repo)?;
            let database = open_db()?;

            worksource::close_issue(&plugin_name, &source_repo, number, comment.as_deref())?;

            let details = serde_json::json!({ "comment": comment });
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
                    outcome: "success",
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
