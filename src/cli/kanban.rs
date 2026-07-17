//! `legion kanban` and `legion done` handlers plus worksource close/reopen
//! propagation (carved from main.rs, #610).

use clap::Subcommand;

use crate::cli::util::{audit, open_db, open_db_and_index};
use crate::cli::verify::resolve_acceptance_criteria;
use crate::verify::GateResult;
use crate::{db, error, kanban, status, verify, worksource};

#[derive(Subcommand)]
pub(crate) enum KanbanAction {
    /// Create a new card on the kanban board
    Create {
        /// Who is creating the card
        #[arg(long)]
        from: String,

        /// Which agent this card is assigned to
        #[arg(long)]
        to: String,

        /// Card description
        #[arg(long)]
        text: String,

        /// Additional context
        #[arg(long)]
        context: Option<String>,

        /// Priority (default: med)
        #[arg(long, value_enum, default_value_t = kanban::Priority::Med)]
        priority: kanban::Priority,

        /// Comma-separated labels
        #[arg(long)]
        labels: Option<String>,

        /// Parent card ID (for delegation chains)
        #[arg(long)]
        parent: Option<String>,

        /// Link to external issue (e.g., GitHub issue URL)
        #[arg(long)]
        source_url: Option<String>,

        /// Source type (e.g., "github", "jira")
        #[arg(long)]
        source_type: Option<String>,
    },

    /// View a single card by ID
    View {
        /// Card ID
        #[arg(long)]
        id: String,

        /// Output as a single JSON object instead of human-readable text
        #[arg(long)]
        json: bool,
    },

    /// Update mutable fields on an existing card
    Update {
        /// Card ID
        #[arg(long)]
        id: String,

        /// Repository name (used as the audit agent)
        #[arg(long)]
        repo: String,

        /// New title text
        #[arg(long)]
        text: Option<String>,

        /// New body (markdown); re-parsed into problem/solution/acceptance sections
        #[arg(long)]
        body: Option<String>,

        /// New priority
        #[arg(long, value_enum)]
        priority: Option<kanban::Priority>,

        /// Replace labels with this comma-separated list
        #[arg(long, conflicts_with_all = ["add_labels", "remove_labels"])]
        labels: Option<String>,

        /// Append comma-separated labels (deduplicated against existing)
        #[arg(long, conflicts_with = "labels")]
        add_labels: Option<String>,

        /// Remove comma-separated labels
        #[arg(long, conflicts_with = "labels")]
        remove_labels: Option<String>,
    },

    /// List cards for a repo
    List {
        /// Repository name
        #[arg(long)]
        repo: String,

        /// Show outbound cards (created by this repo) instead of inbound
        #[arg(long)]
        from: bool,

        /// Emit JSONL (one summary object per line) instead of human-readable text
        #[arg(long)]
        json: bool,

        /// Show all cards, including Backlog and terminal (Done/Cancelled)
        #[arg(long, conflicts_with_all = ["backlog", "deferred"])]
        all: bool,

        /// Show only the raw Backlog (the unconsented inbox)
        #[arg(long, conflicts_with = "deferred")]
        backlog: bool,

        /// Show only Deferred cards (put off until a future wake_at, #816)
        #[arg(long)]
        deferred: bool,
    },

    /// Accept a pending card (move to in-progress)
    Accept {
        /// Card ID
        #[arg(long)]
        id: String,
    },

    /// Block a card (technical blocker)
    Block {
        /// Card ID
        #[arg(long)]
        id: String,

        /// Reason for blocking
        #[arg(long)]
        reason: Option<String>,
    },

    /// Unblock a blocked card (returns to in-progress)
    Unblock {
        /// Card ID
        #[arg(long)]
        id: String,
    },

    /// Delegate an Accepted card to a live, watch-spawned wake attempt (#778).
    ///
    /// Entry is refused unless the watch daemon's heartbeat is fresh AND a
    /// live (in-flight) wake_attempts row for this card's repo exists -- a
    /// self-set label with no process behind it is exactly the bypass this
    /// state must never become. `tick_health` auto-reverts the card back to
    /// Accepted the moment that attempt finishes or dies.
    Delegate {
        /// Card ID
        #[arg(long)]
        id: String,

        /// Wake attempt id to delegate to. When omitted, the single live,
        /// not-yet-delegated wake attempt for the card's repo is used;
        /// with more than one candidate in flight this must be given
        /// explicitly to disambiguate.
        #[arg(long)]
        attempt_id: Option<String>,
    },

    /// Manually resume a delegated card (returns to in-progress).
    ///
    /// Normally unnecessary -- `tick_health` auto-reverts a delegated card
    /// once its attempt is no longer live -- but available for an agent
    /// that wants to resume the work itself before that sweep runs.
    Undelegate {
        /// Card ID
        #[arg(long)]
        id: String,
    },

    /// Defer a card to a future time (#816): put it off until `--until`,
    /// excluding it from the Stop in-progress gate and the active
    /// (WorkingSet) listing until then. Legal from Accepted or Pending;
    /// re-defer (updating `--until`) is legal from Deferred itself.
    /// `tick_health` wakes the owner and reverts the card automatically
    /// once `--until` passes; `legion kanban undefer` does the same
    /// manually, ahead of schedule.
    ///
    /// Liveness caveat (same limitation as `legion kanban delegate`, #778):
    /// the auto-wake only fires while `legion watch` (standalone or the
    /// daemon) is running for this card's repo. If no watch process is
    /// alive when `--until` passes, the card stays Deferred past its wake
    /// time until one starts and runs a health tick -- use `legion kanban
    /// undefer` to wake it manually if that matters before then.
    Defer {
        /// Card ID
        #[arg(long)]
        id: String,

        /// When to wake: `YYYY-MM-DD`, `<N>d`, `<N>w`, or `today`, reusing
        /// #786's `TimeRange` token shapes applied FORWARD from now (not
        /// the backward-from-today direction `--since`/`--until` use
        /// elsewhere). Must resolve to a future time -- a past or
        /// same-instant result is refused, naming the parsed value.
        #[arg(long)]
        until: String,
    },

    /// Manually wake a deferred card early (returns to whichever status it
    /// was deferred from -- Accepted or Pending).
    ///
    /// Normally unnecessary -- `tick_health` auto-reverts a deferred card
    /// once `wake_at` passes -- but available for an agent or operator that
    /// wants the work back before then.
    Undefer {
        /// Card ID
        #[arg(long)]
        id: String,
    },

    /// List delegated cards whose linked attempt is NOT verifiably live
    /// (#778): the watch daemon heartbeat is stale/absent, or the linked
    /// wake_attempts row is missing or terminal. Used by `stop.sh`'s
    /// delegated-liveness gate as the fail-closed last-line-of-defense for
    /// the case `tick_health`'s own auto-revert cannot reach -- the watch
    /// daemon itself being down. An empty result means every delegated
    /// card for the repo is either accounted for or genuinely still live.
    DelegatedNeedsAttention {
        /// Repository name
        #[arg(long)]
        repo: String,

        /// Emit JSONL (one summary object per line) instead of human-readable text
        #[arg(long)]
        json: bool,
    },

    /// Mark a card for review
    Review {
        /// Card ID
        #[arg(long)]
        id: String,
    },

    /// Mark a card as needing human input
    NeedInput {
        /// Card ID
        #[arg(long)]
        id: String,

        /// What input is needed
        #[arg(long)]
        reason: Option<String>,
    },

    /// Resume a card from needs-input or in-review
    Resume {
        /// Card ID
        #[arg(long)]
        id: String,
    },

    /// Request a re-plan: the agent has concluded the card's frozen
    /// acceptance criteria are wrong, incomplete, or unachievable as
    /// written, and stops instead of improvising around them (Accepted ->
    /// NeedsInput). This is the "stop, do not route around" step of the
    /// spec-revision protocol
    /// (docs/decisions/2026-05-31-spec-revision-protocol.md): the reason is
    /// required because it is what a human re-ratifies against.
    ///
    /// Once the AC are revised and ratified, record that with
    /// `legion kanban replan-record` before resuming work -- an unratified
    /// deviation from the frozen AC fails `legion verify`.
    ReplanRequest {
        /// Card ID
        #[arg(long)]
        id: String,

        /// Why the frozen acceptance criteria are wrong, incomplete, or
        /// unachievable. Required: this is the surfaced reason a human
        /// re-ratifies against.
        #[arg(long)]
        reason: String,
    },

    /// Record a ratified re-plan for a card: the frozen acceptance criteria
    /// were revised by a deliberate design act (human + agent), not
    /// improvised around mid-flight. `legion verify` consults this record to
    /// tell a sanctioned re-plan apart from an unratified deviation.
    ReplanRecord {
        /// Card ID the re-plan is bound to
        #[arg(long)]
        id: String,

        /// What changed and why
        #[arg(long)]
        reason: String,
    },

    /// Cancel a card
    ///
    /// When the card has a linked external issue (`source_url`), the
    /// corresponding GitHub issue is automatically closed as
    /// "not-planned" via the work source plugin. Pass `--no-propagate`
    /// to transition only the local card state without touching
    /// GitHub.
    Cancel {
        /// Card ID
        #[arg(long)]
        id: String,

        /// Optional cancel reason, posted as a comment on the GitHub
        /// issue before the close
        #[arg(long)]
        reason: Option<String>,

        /// Transition the card locally without closing the linked
        /// GitHub issue. Use this when the kanban state needs to
        /// diverge from the external issue state deliberately.
        #[arg(long)]
        no_propagate: bool,
    },

    /// Assign a backlog card to an agent
    Assign {
        /// Card ID
        #[arg(long)]
        id: String,

        /// Target agent/repo
        #[arg(long)]
        to: String,
    },

    /// Reopen a done or cancelled card
    ///
    /// When the card has a linked external issue that was previously
    /// closed by a kanban transition, the corresponding GitHub issue
    /// is automatically reopened via the work source plugin. Pass
    /// `--no-propagate` to transition only the local card state.
    Reopen {
        /// Card ID
        #[arg(long)]
        id: String,

        /// Optional reopen reason, posted as a comment on the GitHub
        /// issue after the reopen
        #[arg(long)]
        reason: Option<String>,

        /// Transition the card locally without reopening the linked
        /// GitHub issue
        #[arg(long)]
        no_propagate: bool,
    },

    /// Permanently delete a card from the kanban board.
    ///
    /// Unlike `cancel` (which transitions to a terminal `cancelled`
    /// state where the card still appears in `legion kanban list`),
    /// `delete` removes the row entirely. Used to hard-remove a card
    /// filed in error. Does NOT touch the linked GitHub issue; use
    /// `legion issue close` or `legion issue reopen` separately if the
    /// public state needs to change.
    Delete {
        /// Card ID
        #[arg(long)]
        id: String,
    },

    /// Bind a spec document to a card.
    ///
    /// Creates the card<->document link by setting `tasks.document_id`.
    /// Once bound, `legion verify` reads acceptance criteria from
    /// the document's `verification.acceptance` block (when present)
    /// instead of `tasks.acceptance`. Status transitions that map to
    /// spec statuses (accepted, in-review, done, cancelled) also update
    /// the document's `meta.status` transactionally.
    ///
    /// Errors when:
    /// - The card is already bound to a document.
    /// - The document does not exist or is archived.
    /// - Another live (non-cancelled) card is already bound to the document.
    Bind {
        /// Card ID to bind
        #[arg(long)]
        id: String,

        /// Document ID to bind to the card
        #[arg(long)]
        document: String,
    },

    /// Reconcile kanban cards with their linked GitHub issue state.
    ///
    /// Detects two drift directions:
    ///
    ///   1. **stale-open**: local state is `done` or `cancelled` but the
    ///      linked GitHub issue is still `OPEN`. Pass `--close-stale` to
    ///      close those issues via the work source plugin.
    ///   2. **shipped-pending**: local state is `pending` (or any other
    ///      active state) but the linked GitHub issue is `CLOSED` or
    ///      `MERGED`. Pass `--cancel-shipped` to cancel those cards
    ///      locally without touching GitHub (the issue is already closed).
    ///
    /// `--apply` is shorthand for both action flags.
    ///
    /// Default mode is read-only: the command scans and reports without
    /// changing any state. Per-card failures are logged and counted but
    /// do not abort the run.
    Reconcile {
        /// Optional repo filter -- only reconcile cards owned by this
        /// agent. Default is all cards across all repos.
        #[arg(long)]
        repo: Option<String>,

        /// Actually close the stale GitHub issues (direction 1).
        /// Without any action flag, the command is read-only and safe
        /// to run repeatedly.
        #[arg(long)]
        close_stale: bool,

        /// Actually cancel the shipped-pending cards locally with
        /// `--no-propagate` semantics (direction 2). The linked GitHub
        /// issue is already closed, so propagating again is unnecessary.
        #[arg(long)]
        cancel_shipped: bool,

        /// Convenience: apply both `--close-stale` and `--cancel-shipped`.
        #[arg(long, conflicts_with_all = ["close_stale", "cancel_shipped"])]
        apply: bool,
    },
}

/// Outcome of a kanban-to-worksource propagation attempt.
///
/// Returned by `propagate_card_close_to_worksource` and
/// `propagate_card_reopen_to_worksource` so the caller can make the
/// success/failure state visible on stdout in addition to the stderr
/// breadcrumbs emitted by the helpers. Scripts piping legion output can
/// detect `Failed` and surface partial-success errors; humans reading the
/// terminal see either the silent success or the explicit warning line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PropagateOutcome {
    /// Plugin call succeeded and the GitHub issue state changed.
    Propagated,
    /// Card had no linked external issue (pure local card), or its
    /// `to_repo` has no work source configured. Nothing to propagate.
    /// Safe to no-op without caller intervention.
    Skipped,
    /// Plugin call was attempted and failed, OR a card-level precondition
    /// was not met (e.g. source_url present but no extractable issue
    /// number, or source_type missing). The local card transition has
    /// already completed; the caller should surface a visible warning so
    /// scripted consumers can detect partial success.
    Failed,
}

/// Print one drift bucket of a `legion kanban reconcile` scan to stdout: the
/// empty-state line when nothing drifted, otherwise the count line followed by
/// one row per card. Keeps the operator-facing report in the CLI while the
/// scan/action logic lives in `kanban::reconcile`.
fn print_drift_bucket(
    drifts: &[kanban::reconcile::Drift],
    plural_label: &str,
    empty_message: &str,
) {
    if drifts.is_empty() {
        println!("[legion] reconcile: {empty_message}");
        return;
    }
    println!("[legion] reconcile: {} {plural_label} found", drifts.len());
    for drift in drifts {
        println!(
            "  card={} status={} source={}#{} to_repo={}",
            drift.card.id,
            drift.card.status.label(),
            drift.source_repo,
            drift.number,
            drift.card.to_repo
        );
    }
}

/// Close the GitHub issue linked to a kanban card via the work source
/// plugin, and write an audit entry describing the propagation. Called by
/// `legion kanban cancel` and `legion done --id` (#610 folded the Done
/// arm's previously inline copy through this helper, restoring the audit
/// row and `PropagateOutcome` reporting the inline copy had lost) to keep
/// the public GitHub state consistent with the local kanban state.
///
/// Returns a `PropagateOutcome` so the caller can emit a visible warning
/// to stdout on failure. Stderr breadcrumbs explain the failure; stdout
/// visibility is what lets scripted pipelines detect partial success.
///
/// A card with no `source_url` is a pure local card and is skipped
/// silently. A card whose `to_repo` has no work source configured in
/// `watch.toml` is also skipped, with a warning -- the agent can still
/// transition the card locally, they just have to reconcile GitHub state
/// manually. All other failure modes log to stderr and do not abort the
/// calling handler, because the card transition has already happened and
/// returning an error here would leave the agent with an inconsistent
/// understanding of local state.
///
/// `comment` is the closing comment posted on the GitHub issue before the
/// state transition. When `None`, the plugin closes the issue without a
/// comment.
fn propagate_card_close_to_worksource(
    database: &db::Database,
    card_id: &str,
    comment: Option<&str>,
) -> PropagateOutcome {
    let card = match database.get_card_by_id(card_id) {
        Ok(Some(c)) => c,
        Ok(None) => {
            eprintln!("[legion] propagate: card {card_id} not found; skipping");
            return PropagateOutcome::Failed;
        }
        Err(e) => {
            eprintln!("[legion] propagate: lookup of card {card_id} failed: {e}; skipping");
            return PropagateOutcome::Failed;
        }
    };

    let Some(ref source_url) = card.source_url else {
        // Pure local card with no linked issue -- nothing to propagate.
        return PropagateOutcome::Skipped;
    };

    let Some(number) = worksource::extract_issue_number(source_url) else {
        eprintln!(
            "[legion] propagate: card {card_id} has source_url {source_url} but no extractable issue number; skipping"
        );
        return PropagateOutcome::Failed;
    };

    let Some(source) = card.source_type.as_deref() else {
        eprintln!(
            "[legion] propagate: card {card_id} has source_url {source_url} but no source_type; skipping"
        );
        return PropagateOutcome::Failed;
    };

    let Some((_, source_repo, _)) = worksource::resolve_config(&card.to_repo) else {
        eprintln!(
            "[legion] propagate: to_repo '{}' for card {card_id} has no work source configured in watch.toml; skipping GitHub close",
            card.to_repo
        );
        // Missing watch.toml entry is treated as Skipped (not Failed)
        // because it is a configuration-level "not applicable here"
        // rather than an attempt-and-fail. Scripted consumers that
        // care about this case should rely on the stderr warning.
        return PropagateOutcome::Skipped;
    };

    match worksource::close_issue(source, &source_repo, number, comment) {
        Ok(()) => {
            eprintln!("[legion] closed {source} issue #{number}");
            let details = serde_json::json!({
                "card_id": card_id,
                "propagation": "kanban-transition",
                "comment": comment,
            });
            let details_str = details.to_string();
            audit(
                database,
                &db::AuditInput {
                    agent: &card.to_repo,
                    action: "close-issue",
                    target_type: "issue",
                    target_ref: &number.to_string(),
                    task_id: Some(card_id),
                    source_type: source,
                    details: Some(&details_str),
                    outcome: "success",
                },
            );
            PropagateOutcome::Propagated
        }
        Err(e) => {
            eprintln!("[legion] propagate: failed to close {source} issue #{number}: {e}");
            PropagateOutcome::Failed
        }
    }
}

/// Reopen the GitHub issue linked to a kanban card via the work source
/// plugin. Symmetrical with `propagate_card_close_to_worksource`. Called by
/// `legion kanban reopen` so a card being moved back to in-progress reopens
/// its public GitHub issue.
fn propagate_card_reopen_to_worksource(
    database: &db::Database,
    card_id: &str,
    comment: Option<&str>,
) -> PropagateOutcome {
    let card = match database.get_card_by_id(card_id) {
        Ok(Some(c)) => c,
        Ok(None) => {
            eprintln!("[legion] propagate: card {card_id} not found; skipping");
            return PropagateOutcome::Failed;
        }
        Err(e) => {
            eprintln!("[legion] propagate: lookup of card {card_id} failed: {e}; skipping");
            return PropagateOutcome::Failed;
        }
    };

    let Some(ref source_url) = card.source_url else {
        return PropagateOutcome::Skipped;
    };
    let Some(number) = worksource::extract_issue_number(source_url) else {
        eprintln!(
            "[legion] propagate: card {card_id} has source_url {source_url} but no extractable issue number; skipping"
        );
        return PropagateOutcome::Failed;
    };
    let Some(source) = card.source_type.as_deref() else {
        eprintln!(
            "[legion] propagate: card {card_id} has source_url {source_url} but no source_type; skipping"
        );
        return PropagateOutcome::Failed;
    };
    let Some((_, source_repo, _)) = worksource::resolve_config(&card.to_repo) else {
        eprintln!(
            "[legion] propagate: to_repo '{}' for card {card_id} has no work source configured in watch.toml; skipping GitHub reopen",
            card.to_repo
        );
        return PropagateOutcome::Skipped;
    };

    match worksource::reopen_issue(source, &source_repo, number, comment) {
        Ok(()) => {
            eprintln!("[legion] reopened {source} issue #{number}");
            let details = serde_json::json!({
                "card_id": card_id,
                "propagation": "kanban-transition",
                "comment": comment,
            });
            let details_str = details.to_string();
            audit(
                database,
                &db::AuditInput {
                    agent: &card.to_repo,
                    action: "reopen-issue",
                    target_type: "issue",
                    target_ref: &number.to_string(),
                    task_id: Some(card_id),
                    source_type: source,
                    details: Some(&details_str),
                    outcome: "success",
                },
            );
            PropagateOutcome::Propagated
        }
        Err(e) => {
            eprintln!("[legion] propagate: failed to reopen {source} issue #{number}: {e}");
            PropagateOutcome::Failed
        }
    }
}

pub(crate) fn handle_done(repo: String, text: String, id: Option<String>) -> error::Result<()> {
    let (database, index) = open_db_and_index()?;

    // Validate card transition BEFORE posting announcements
    if let Some(ref card_id) = id {
        // Verify gate (#520): a card with acceptance criteria cannot
        // reach Done until `legion verify` recorded a clean verdict for
        // it. The gate is card-keyed (legion-verify:<card_id>), so it
        // holds regardless of the commit `legion done` runs on. Cards
        // with no criteria (chores) are not gated. Done is the terminal
        // QA gate for a solo team, so there is no --skip here.
        if let Some(card) = database.get_card_by_id(card_id)? {
            // Resolve AC with spec-document precedence (#644): a spec-bound card
            // gates on the bound document's verification.acceptance, not
            // tasks.acceptance. A dangling document_id is a hard error here too,
            // matching the behaviour of handle_verify.
            let (acceptance, ac_source) = resolve_acceptance_criteria(&database, &card)?;
            if !acceptance.is_empty() {
                let skill = verify::verify_gate_key(card_id);
                match database.get_latest_quality_gate_by_skill(&skill)? {
                    Some(gate) if gate.result == GateResult::Clean => {}
                    Some(_) => {
                        eprintln!(
                            "[legion] error: verify gate is not clean for card {card_id} \
                             (ac source: {ac_source}). Resolve the failing/uncertain criteria \
                             and re-run verify before Done."
                        );
                        return Err(error::LegionError::ExitWith(1));
                    }
                    None => {
                        eprintln!(
                            "[legion] error: card {card_id} has acceptance criteria \
                             (ac source: {ac_source}) but no verify verdict. Run the verify \
                             skill before Done."
                        );
                        return Err(error::LegionError::ExitWith(1));
                    }
                }
            }
        }

        kanban::transition_card(&database, card_id, kanban::Action::Done, Some(&text))?;
        println!("{card_id}");

        // Close the linked external issue if present, using the
        // done-text as the closing comment so the GitHub issue thread
        // records why it was closed. Folded through the shared
        // propagation helper (#610) so `legion done` writes the same
        // audit row and stdout partial-failure warning as
        // `legion kanban cancel`, and resolves the work source from
        // the card's own to_repo instead of the --repo argument.
        match propagate_card_close_to_worksource(&database, card_id, Some(&text)) {
            PropagateOutcome::Propagated | PropagateOutcome::Skipped => {}
            PropagateOutcome::Failed => {
                // Same stdout-visibility pattern as KanbanAction::Cancel:
                // the card is Done locally (the println of {card_id}
                // above stands), but the linked GitHub issue was NOT
                // closed and scripted callers need to see that.
                println!(
                    "[legion] WARNING: card {card_id} completed locally but linked github issue propagation FAILED -- run `legion kanban reconcile --close-stale` to retry"
                );
            }
        }
    }

    let announcement = format!("{repo} completed: {text}");
    let reflection = database.insert_reflection_with_meta(
        &repo,
        &announcement,
        "team",
        &db::ReflectionMeta::default(),
    )?;
    if let Err(e) = index.add(
        &reflection.id,
        &reflection.repo,
        &announcement,
        &reflection.created_at,
    ) {
        eprintln!("[legion] search index add failed: {e}");
    }
    info!("[legion] done: {text}");

    let blocked_agents = status::find_blocked_agents(&database, &repo)?;
    for agent in &blocked_agents {
        let notify_text = format!(
            "@{agent} announce from {repo} -- {repo} completed: {text}. Your blocker may be cleared."
        );
        let notify_ref = database.insert_reflection_with_meta(
            &repo,
            &notify_text,
            "team",
            &db::ReflectionMeta::default(),
        )?;
        if let Err(e) = index.add(
            &notify_ref.id,
            &notify_ref.repo,
            &notify_text,
            &notify_ref.created_at,
        ) {
            eprintln!("[legion] search index add failed: {e}");
        }
        info!("[legion] notified {agent} (was blocked on {repo})");
    }

    if blocked_agents.is_empty() {
        info!("[legion] no blocked agents found");
    }
    Ok(())
}

pub(crate) fn handle(action: KanbanAction) -> error::Result<()> {
    let database = open_db()?;

    match action {
        KanbanAction::Create {
            from,
            to,
            text,
            context,
            priority,
            labels,
            parent,
            source_url,
            source_type,
        } => {
            let id = kanban::create_card(
                &database,
                &from,
                &to,
                &text,
                context.as_deref(),
                priority,
                labels.as_deref(),
                parent.as_deref(),
                source_url.as_deref(),
                source_type.as_deref(),
                None,
            )?;
            println!("{id}");
        }
        KanbanAction::View { id, json } => {
            let card = kanban::view_card(&database, &id).map_err(|e| {
                eprintln!("{e}");
                e
            })?;
            if json {
                println!("{}", kanban::format_card_json(&card)?);
            } else {
                print!("{}", kanban::format_card_view(&card));
            }
        }
        KanbanAction::Update {
            id,
            repo,
            text,
            body,
            priority,
            labels,
            add_labels,
            remove_labels,
        } => {
            let any_set = text.is_some()
                || body.is_some()
                || priority.is_some()
                || labels.is_some()
                || add_labels.is_some()
                || remove_labels.is_some();
            if !any_set {
                eprintln!(
                    "[legion] no fields to update: pass at least one of --text, --body, --priority, --labels, --add-labels, --remove-labels"
                );
                return Err(error::LegionError::ExitWith(1));
            }
            let params = kanban::CardUpdateParams {
                text,
                body,
                priority,
                labels,
                add_labels,
                remove_labels,
            };
            let card_id = kanban::update_card(&database, &id, &repo, &params)?;
            println!("{card_id}");
            audit(
                &database,
                &db::AuditInput {
                    agent: &repo,
                    action: "update-card",
                    target_type: "card",
                    target_ref: &id,
                    task_id: Some(&id),
                    source_type: "legion",
                    details: None,
                    outcome: "success",
                },
            );
        }
        KanbanAction::List {
            repo,
            from,
            json,
            all,
            backlog,
            deferred,
        } => {
            let direction = if from {
                kanban::Direction::Outbound
            } else {
                kanban::Direction::Inbound
            };
            // Default to the working set; --all, --backlog, and --deferred
            // widen/redirect. Deferred is its own scope (#816) -- excluded
            // from WorkingSet, consciously visible here rather than folded
            // into --all/--backlog.
            let scope = if all {
                kanban::CardScope::All
            } else if backlog {
                kanban::CardScope::Backlog
            } else if deferred {
                kanban::CardScope::Deferred
            } else {
                kanban::CardScope::WorkingSet
            };
            let cards = kanban::list_cards(&database, &repo, direction, scope)?;
            if json {
                let output = kanban::format_card_list_json(&cards)?;
                print!("{output}");
            } else {
                let output = kanban::format_card_list(&cards, &repo, direction);
                if output.is_empty() {
                    info!("[legion] no cards found");
                } else {
                    print!("{output}");
                }
            }
        }
        KanbanAction::Accept { id } => {
            let card = kanban::transition_card(&database, &id, kanban::Action::Accept, None)?;
            println!("{id}");
            // #525: accepting a card sets the board-derived goal -- echo
            // the just-accepted card's acceptance criteria as the
            // completion condition the agent now carries.
            if let Some(goal) = kanban::format_active_goal(std::slice::from_ref(&card)) {
                eprintln!("{goal}");
            }
        }
        KanbanAction::Block { id, reason } => {
            kanban::transition_card(&database, &id, kanban::Action::Block, reason.as_deref())?;
            println!("{id}");
        }
        KanbanAction::Unblock { id } => {
            kanban::transition_card(&database, &id, kanban::Action::Unblock, None)?;
            println!("{id}");
        }
        KanbanAction::Delegate { id, attempt_id } => {
            kanban::delegate_card(
                &database,
                &id,
                attempt_id.as_deref(),
                kanban::DELEGATION_STALE_AFTER_SECS,
            )?;
            println!("{id}");
        }
        KanbanAction::Undelegate { id } => {
            kanban::undelegate_card(&database, &id, None)?;
            println!("{id}");
        }
        KanbanAction::Defer { id, until } => {
            let wake_at = crate::timerange::parse_point_in_time(&until)?;
            let now = chrono::Utc::now().to_rfc3339();
            if wake_at <= now {
                return Err(error::LegionError::DeferWakeAtInPast {
                    input: until,
                    wake_at,
                });
            }
            kanban::defer_card(&database, &id, &wake_at, None)?;
            println!("{id}");
        }
        KanbanAction::Undefer { id } => {
            kanban::undefer_card(&database, &id, None)?;
            println!("{id}");
        }
        KanbanAction::DelegatedNeedsAttention { repo, json } => {
            let delegated = database.get_delegated_cards(Some(&repo))?;
            let mut needs_attention = Vec::new();
            for card in delegated {
                if !database
                    .delegated_card_is_live(&card.id, kanban::DELEGATION_STALE_AFTER_SECS)?
                {
                    needs_attention.push(card);
                }
            }
            if json {
                let output = kanban::format_card_list_json(&needs_attention)?;
                print!("{output}");
            } else if needs_attention.is_empty() {
                info!("[legion] no delegated cards need attention for {repo}");
            } else {
                let output =
                    kanban::format_card_list(&needs_attention, &repo, kanban::Direction::Inbound);
                print!("{output}");
            }
        }
        KanbanAction::Review { id } => {
            kanban::transition_card(&database, &id, kanban::Action::Review, None)?;
            println!("{id}");
        }
        KanbanAction::NeedInput { id, reason } => {
            kanban::transition_card(&database, &id, kanban::Action::NeedInput, reason.as_deref())?;
            println!("{id}");
        }
        KanbanAction::Resume { id } => {
            kanban::transition_card(&database, &id, kanban::Action::Resume, None)?;
            println!("{id}");
        }
        KanbanAction::ReplanRequest { id, reason } => {
            kanban::transition_card(
                &database,
                &id,
                kanban::Action::ReplanRequest,
                Some(reason.as_str()),
            )?;
            println!("{id}");
        }
        KanbanAction::ReplanRecord { id, reason } => {
            // Recording one at all is the ratification act (see the
            // `ratified` field's doc comment on `ReplanRecord`); there is no
            // CLI path to record an unratified proposal.
            let record = database.record_replan_record(&id, &reason, true)?;
            println!("{}", record.id);
        }
        KanbanAction::Cancel {
            id,
            reason,
            no_propagate,
        } => {
            kanban::transition_card(&database, &id, kanban::Action::Cancel, reason.as_deref())?;
            println!("{id}");
            if !no_propagate {
                match propagate_card_close_to_worksource(&database, &id, reason.as_deref()) {
                    PropagateOutcome::Propagated | PropagateOutcome::Skipped => {}
                    PropagateOutcome::Failed => {
                        // Emit a visible warning line on stdout so
                        // scripted callers (`legion kanban cancel |
                        // ...`) can detect partial success. The
                        // card transition has already succeeded --
                        // the first println of {id} above is still
                        // the authoritative success marker -- but
                        // the linked GitHub issue was NOT closed
                        // and the caller needs to know.
                        println!(
                            "[legion] WARNING: card {id} cancelled locally but linked github issue propagation FAILED -- run `legion kanban reconcile --close-stale` to retry"
                        );
                    }
                }
            }
        }
        KanbanAction::Bind { id, document } => {
            kanban::bind_document(&database, &id, &document)?;
            println!("{id}");
        }
        KanbanAction::Assign { id, to } => {
            database.assign_card(&id, &to)?;
            println!("{id}");
        }
        KanbanAction::Reopen {
            id,
            reason,
            no_propagate,
        } => {
            kanban::transition_card(&database, &id, kanban::Action::Reopen, reason.as_deref())?;
            println!("{id}");
            if !no_propagate {
                match propagate_card_reopen_to_worksource(&database, &id, reason.as_deref()) {
                    PropagateOutcome::Propagated | PropagateOutcome::Skipped => {}
                    PropagateOutcome::Failed => {
                        // Same stdout-visibility pattern as
                        // KanbanAction::Cancel above. The card
                        // is reopened locally but the linked
                        // GitHub issue was not reopened, and
                        // scripted callers need to see this on
                        // stdout, not buried in stderr.
                        println!(
                            "[legion] WARNING: card {id} reopened locally but linked github issue propagation FAILED -- the public issue may still be closed"
                        );
                    }
                }
            }
        }
        KanbanAction::Delete { id } => {
            // Capture the card's to_repo before the delete so the
            // audit entry records which agent owned it. A card
            // that does not exist at delete time is a hard error
            // from the DB layer (CardNotFound), matching the
            // shape of the other id-targeted kanban subcommands;
            // the lookup here therefore either returns a real
            // repo or propagates the error before we ever reach
            // the audit call, so there is no need for a fallback
            // value.
            let agent_repo = database
                .get_card_by_id(&id)?
                .ok_or_else(|| error::LegionError::CardNotFound(id.clone()))?
                .to_repo;

            database.delete_card(&id)?;
            println!("{id}");

            audit(
                &database,
                &db::AuditInput {
                    agent: &agent_repo,
                    action: "delete-card",
                    target_type: "card",
                    target_ref: &id,
                    task_id: Some(&id),
                    source_type: "legion",
                    details: None,
                    outcome: "success",
                },
            );
        }
        KanbanAction::Reconcile {
            repo,
            close_stale,
            cancel_shipped,
            apply,
        } => {
            // `--apply` is shorthand for both action flags.
            let close_stale = close_stale || apply;
            let cancel_shipped = cancel_shipped || apply;

            // One pass over the board, partitioning by drift direction. The
            // scan, the close-stale action, and the cancel-shipped action all
            // live in `kanban::reconcile` so the daemon's auto-reconcile tick
            // (#654) shares exactly this logic; this arm owns only the
            // operator-facing stdout report and the dry-run hints.
            let report = kanban::reconcile::scan_drift(&database, repo.as_deref(), "[legion]")?;

            print_drift_bucket(
                &report.stale_open,
                "stale-open issue(s)",
                "no stale-open issues found",
            );
            print_drift_bucket(
                &report.shipped_pending,
                "shipped-pending card(s)",
                "no shipped-pending cards found",
            );

            if close_stale && !report.stale_open.is_empty() {
                println!(
                    "[legion] reconcile: closing {} stale issue(s)",
                    report.stale_open.len()
                );
                let (closed, failed) =
                    kanban::reconcile::close_stale_open(&database, &report.stale_open, "[legion]");
                println!("[legion] reconcile: {closed} closed, {failed} failed");
            } else if close_stale {
                println!("[legion] reconcile: nothing to close");
            } else if !report.stale_open.is_empty() {
                println!(
                    "[legion] reconcile: dry-run -- pass --close-stale to actually close these issues"
                );
            }

            if cancel_shipped && !report.shipped_pending.is_empty() {
                println!(
                    "[legion] reconcile: cancelling {} shipped-pending card(s) locally",
                    report.shipped_pending.len()
                );
                let (cancelled, failed) = kanban::reconcile::cancel_shipped_pending(
                    &database,
                    &report.shipped_pending,
                    "[legion]",
                );
                println!("[legion] reconcile: {cancelled} cancelled, {failed} failed");
            } else if cancel_shipped {
                println!("[legion] reconcile: nothing to cancel");
            } else if !report.shipped_pending.is_empty() {
                println!(
                    "[legion] reconcile: dry-run -- pass --cancel-shipped to actually cancel these cards"
                );
            }
        }
    }
    Ok(())
}
