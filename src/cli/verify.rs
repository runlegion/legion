//! `legion verify` and `legion quality-gate` handlers (carved from main.rs, #610).

use std::str::FromStr;

use clap::Subcommand;

use crate::cli::util::{
    current_repo_name, git_head_commit_and_branch, open_db, read_file_or_stdin,
};
use crate::db::findings::{FindingFilter, NewFindingInput, QualityGateFinding};
use crate::db::quality_gates::{
    QualityGateFilter, QualityGateInput, QualityGateRow, QualityGateStats,
};
use crate::finding_gate::{self, FindingSeverity, FindingStatus};
use crate::gate_trust::emit_gate_trust;
use crate::verify::{GateProvenance, GateResult};
use crate::{card_parse, db, documents, error, gate_registry, verify, worksource};

#[derive(Subcommand, Debug)]
pub(crate) enum QualityGateAction {
    /// Record a quality gate result for the current HEAD commit.
    ///
    /// Reads git HEAD and branch automatically. The skill runner calls this
    /// after inspecting the diff. `legion pr create` checks the gate before
    /// calling the work source so the result cannot be faked via a file flag.
    ///
    /// The row is recorded with ASSERTED provenance (#780): no validator
    /// backs it. For a skill with a check validator
    /// (`gate_registry::has_check_validator` -- `legion-pr-write`),
    /// `--result clean` is REFUSED here -- a clean verdict for it can only be
    /// earned via `legion pr write-check`, which validates the PR body before
    /// recording. Skills with no check validator (`legion-review`, a
    /// `legion-verify:<card_id>` verdict) are unaffected: `record` is their
    /// only, legitimate path.
    Record {
        /// Skill name (e.g., "legion-review")
        #[arg(long)]
        skill: String,

        /// Gate result: "clean" or "issues"
        #[arg(long, value_parser = ["clean", "issues"])]
        result: String,

        /// Number of findings (default 0)
        #[arg(long, default_value = "0")]
        findings_count: u64,

        /// Raw JSON details from the skill (full findings array)
        #[arg(long)]
        details_json: Option<String>,
    },

    /// List recorded quality gate rows, newest first.
    ///
    /// Filterable by skill, result, branch, and a since timestamp.
    /// Default output is a human-readable table; --json emits an array
    /// of objects that includes the details field.
    List {
        /// Restrict to rows for this skill name.
        #[arg(long)]
        skill: Option<String>,

        /// Restrict to rows with this result value: "clean" or "issues".
        #[arg(long, value_parser = ["clean", "issues"])]
        result: Option<String>,

        /// Restrict to rows on this branch.
        #[arg(long)]
        branch: Option<String>,

        /// Restrict to rows recorded at or after this RFC3339 timestamp.
        #[arg(long)]
        since: Option<String>,

        /// Emit JSON array instead of a human table.
        #[arg(long)]
        json: bool,
    },

    /// Show per-skill aggregate statistics.
    ///
    /// Prints runs, clean count, issues count, catch rate (issues/runs),
    /// total findings, and max findings for each skill. The catch rate is
    /// the rubberstamp tripwire: a rate near zero means the gate is not
    /// catching anything. --json emits structured rows.
    Stats {
        /// Restrict to this skill name.
        #[arg(long)]
        skill: Option<String>,

        /// Restrict to rows recorded at or after this RFC3339 timestamp.
        #[arg(long)]
        since: Option<String>,

        /// Emit JSON array instead of a human table.
        #[arg(long)]
        json: bool,
    },

    /// Disposition a single PENDING finding: mark it DISPOSITIONED with an
    /// explicit reason (#773). A disposition is not a fix -- it is a
    /// conscious "we are not fixing this, and here is why" -- so `--reason`
    /// is required. Refused when the finding does not exist or is already
    /// RESOLVED (a resolved finding needs no disposition).
    FindingDisposition {
        /// Id of the finding (from `quality-gate finding-list`).
        #[arg(long)]
        id: String,

        /// Why this finding is not being fixed (required).
        #[arg(long)]
        reason: String,
    },

    /// Batch-acknowledge every PENDING LOW-severity finding on a
    /// (branch, skill) pair in the current repo (plus rows that carry no
    /// repo) with one shared reason (#773 AC3): the
    /// "conscious sweep, not per-nit ceremony" carve-out for cosmetic
    /// findings. Each finding is still dispositioned as its own row (own
    /// `updated_at`, individually queryable), so the audit trail stays
    /// per-finding even though the reason is shared across the sweep.
    FindingAck {
        /// Branch the findings were raised on.
        #[arg(long)]
        branch: String,

        /// Skill the findings were raised under (e.g. "legion-review").
        #[arg(long)]
        skill: String,

        /// Why these LOW findings are being waived as a batch (required).
        #[arg(long)]
        reason: String,
    },

    /// List findings for the audit surface (#773 AC4): which findings were
    /// fixed (RESOLVED), waived (DISPOSITIONED), or are still PENDING, over
    /// time. Filterable by repo, branch, skill, and status; unfiltered lists
    /// everything, newest first.
    FindingList {
        /// Repo the findings were recorded in. Rows written before findings
        /// carried a repo (NULL) match every repo.
        #[arg(long)]
        repo: Option<String>,

        #[arg(long)]
        branch: Option<String>,

        #[arg(long)]
        skill: Option<String>,

        #[arg(long, value_parser = ["pending", "resolved", "dispositioned"])]
        status: Option<String>,

        /// Emit JSON array instead of a human table.
        #[arg(long)]
        json: bool,
    },

    /// Void a gate row: retire a known-false verdict without deleting it
    /// from history (#780 tombstone pattern, mirroring `deleted_at` on
    /// tasks/reflections/schedules).
    ///
    /// A voided row drops out of `get_quality_gate` /
    /// `get_latest_quality_gate_by_skill` (so `pr create`'s gate check and
    /// the ->Done gate can never resolve to it again) and out of
    /// `quality-gate stats`, but stays visible in `quality-gate list` --
    /// voiding annotates the ledger, it never erases it.
    ///
    /// Use `--superseded-by` once the genuine replacement row exists (e.g.
    /// after re-running the gate on the same commit) to link the
    /// voided row to what replaced it.
    Void {
        /// Id of the gate row to void (from `quality-gate list` or the id
        /// printed by `record`).
        #[arg(long)]
        id: String,

        /// Why this row is known-false (required -- a void with no reason
        /// is not an audit trail).
        #[arg(long)]
        reason: String,

        /// Id of the row that supersedes this one, if a re-laid genuine row
        /// already exists.
        #[arg(long)]
        superseded_by: Option<String>,
    },
}

pub(crate) fn handle_quality_gate(action: QualityGateAction) -> error::Result<()> {
    match action {
        QualityGateAction::Record {
            skill,
            result,
            findings_count,
            details_json,
        } => {
            let gate_result = GateResult::from_str(&result)?;

            // #780: a "clean" verdict for a skill with a check validator can
            // only be earned by passing that validator. Refusing here closes
            // the exact loophole a manufactured-clean row exploits -- self-
            // reporting "clean" via `record` for a skill whose real gate is
            // its validator. Skills with no validator (legion-review, a
            // legion-verify:<card_id> verdict) are asserted by necessity and
            // unaffected.
            if gate_result == GateResult::Clean && gate_registry::has_check_validator(&skill) {
                eprintln!(
                    "[legion] error: '{skill}' has a check validator -- a clean gate cannot be \
                     recorded via 'quality-gate record'. Run 'legion pr write-check' instead, \
                     which validates the PR body before recording."
                );
                return Err(error::LegionError::ExitWith(1));
            }

            let (commit_hash, branch) = git_head_commit_and_branch()?;
            refuse_detached_head(&branch)?;
            let repo = current_repo_name()?;

            let database = open_db()?;

            // #773: extract THIS call's own structured findings BEFORE the
            // refusal check runs. legion-review's `approved` decision records
            // `--result clean` in the SAME call as any surviving non-blocking
            // findings (SKILL.md: "surviving MEDs named in the sign-off") --
            // a clean call that itself carries findings must be refused by
            // that call, not merely by some future one. Best-effort: a
            // missing `findings` key (the common case for most skills, and
            // for a genuinely clean legion-review run) yields zero findings
            // here without complaint. A malformed `--details-json` payload
            // (present but not valid JSON) ALSO yields zero findings -- this
            // is a fail-open gap review flagged (a corrupted skill invocation
            // could theoretically mask real findings) -- but is now loud on
            // stderr rather than silent, so the caller sees it instead of the
            // clean gate silently passing with no trace of why nothing was
            // extracted.
            let raw_findings: Vec<finding_gate::RawFinding> = match details_json.as_deref() {
                Some(d) => match serde_json::from_str::<serde_json::Value>(d) {
                    Ok(v) => finding_gate::extract_findings_from_value(&v),
                    Err(e) => {
                        eprintln!(
                            "[legion] warning: --details-json present but failed to parse as \
                             JSON ({e}) -- 0 findings extracted from it. If this call intended \
                             to report findings, they will NOT be tracked by the \
                             finding-resolution ledger (#773); fix the JSON and re-run."
                        );
                        Vec::new()
                    }
                },
                None => Vec::new(),
            };

            // #773/#1008: a self-contradiction check that closes the
            // remaining fail-open gap without imposing hard-refusal on every
            // malformed `--details-json` (most skills never set a `findings`
            // key at all, and that legitimate case must keep passing). Fires
            // for a `clean` OR an `issues` request alike -- see
            // `findings_count_contradicts_extraction`'s doc comment.
            if findings_count_contradicts_extraction(findings_count, &raw_findings) {
                eprintln!(
                    "[legion] error: cannot record a gate for skill '{skill}' -- \
                     --findings-count {findings_count} but 0 findings were extracted from \
                     --details-json (#773/#1008). This means either --details-json is \
                     missing/malformed for a run that claims real findings, or its `findings` \
                     array does not match the {{file, line, severity, summary}} schema. Fix \
                     --details-json so the findings it claims are actually tracked, or pass \
                     --findings-count 0 if there truly are none."
                );
                return Err(error::LegionError::ExitWith(1));
            }

            // Reconcile the PENDING finding ledger against this commit first
            // (a fix landed in an earlier commit must not still read as
            // pending), then -- only when this run claims `clean` -- refuse
            // unless every PRIOR non-trivial finding is resolved/dispositioned,
            // every prior LOW finding is batch-acked, AND this run itself
            // reports zero findings. Runs for every skill, not just
            // legion-review: a skill with no findings ever recorded simply has
            // an empty pending set, so this is a no-op for it.
            reconcile_and_refuse_if_findings_pending(
                &database,
                &repo,
                &branch,
                &skill,
                &commit_hash,
                gate_result == GateResult::Clean,
                &raw_findings,
            )?;

            let row = database.record_quality_gate(&QualityGateInput {
                branch: &branch,
                commit_hash: &commit_hash,
                skill: &skill,
                result: gate_result,
                findings_count,
                details: details_json.as_deref(),
                provenance: GateProvenance::Asserted,
                base: None,
            })?;
            // No issue: a quality-gate row names only its branch, and a
            // branch name is not a mapping to an issue (#1279).
            emit_gate_trust(&database, &row, None);

            // #773: persist the findings extracted above, now that the gate
            // row (and its id) exists. Only reached once the refusal check
            // above has already passed -- a refused clean call never
            // persists its findings; the caller re-runs with `--result
            // issues` to persist them, then dispositions/acks.
            persist_raw_findings(&database, &repo, &row, &raw_findings);

            println!("{}", row.id);
        }

        QualityGateAction::List {
            skill,
            result,
            branch,
            since,
            json,
        } => {
            // Parse the optional --result flag into a typed GateResult so an
            // invalid value surfaces a descriptive error before we touch the DB.
            let gate_result: Option<GateResult> = match result.as_deref() {
                Some(r) => Some(GateResult::from_str(r)?),
                None => None,
            };

            let database = open_db()?;
            let rows: Vec<QualityGateRow> = database.list_quality_gates(&QualityGateFilter {
                skill,
                result: gate_result,
                branch,
                since,
            })?;

            if json {
                println!("{}", serde_json::to_string(&rows)?);
            } else {
                print_gate_table(&rows);
            }
        }

        QualityGateAction::Stats { skill, since, json } => {
            let database = open_db()?;
            let stats: Vec<QualityGateStats> =
                database.quality_gate_stats(skill.as_deref(), since.as_deref())?;

            if json {
                println!("{}", serde_json::to_string(&stats)?);
            } else {
                print_stats_table(&stats);
            }
        }

        QualityGateAction::FindingDisposition { id, reason } => {
            let database = open_db()?;
            let full_id = resolve_finding_id(&database, &id)?;
            let finding = database.dispose_finding(&full_id, &reason)?;
            println!(
                "[legion] dispositioned finding {} ({}): {}",
                finding.id,
                file_loc(&finding.file, finding.line),
                reason
            );
        }

        QualityGateAction::FindingAck {
            branch,
            skill,
            reason,
        } => {
            let repo = current_repo_name()?;
            let database = open_db()?;
            let acked = database.batch_ack_low_findings(&repo, &branch, &skill, &reason)?;
            println!(
                "[legion] batch-acked {} LOW finding(s) in repo '{repo}' on branch '{branch}' \
                 skill '{skill}': {reason}",
                acked.len()
            );
            for f in &acked {
                println!("  - {} ({})", f.id, file_loc(&f.file, f.line));
            }
        }

        QualityGateAction::FindingList {
            repo,
            branch,
            skill,
            status,
            json,
        } => {
            let status_typed: Option<FindingStatus> = match status.as_deref() {
                Some(s) => Some(s.parse()?),
                None => None,
            };
            let database = open_db()?;
            let rows: Vec<QualityGateFinding> = database.list_findings(&FindingFilter {
                repo,
                branch,
                skill,
                status: status_typed,
            })?;

            if json {
                println!("{}", serde_json::to_string(&rows)?);
            } else {
                print_findings_table(&rows);
            }
        }

        QualityGateAction::Void {
            id,
            reason,
            superseded_by,
        } => {
            let database = open_db()?;
            let full_id = resolve_gate_id(&database, &id)?;
            let row = database.void_quality_gate(&full_id, &reason, superseded_by.as_deref())?;
            println!(
                "[legion] voided gate {} (skill '{}', commit {}): {}",
                row.id, row.skill, row.commit_hash, reason
            );
            if let Some(sup) = &row.superseded_by {
                println!("  superseded by: {sup}");
            }
        }
    }
    Ok(())
}

/// True when a run's own asserted `--findings-count` contradicts what was
/// actually extracted into `raw_findings` -- the caller claims N>0 findings
/// but ledger extraction produced none. Applies to ANY gate result, not only
/// `clean`: an `--result issues` call with a malformed or schema-mismatched
/// `--details-json` must be refused exactly like a `clean`
/// call is today, closing the fail-open gap where `findings_count` on the
/// recorded gate row diverges from the persisted `quality_gate_findings`
/// ledger with no error at record time (#1008). Refusing on this closes the
/// fail-open gap a totally-malformed or entirely-mismatched-schema
/// `--details-json` payload would otherwise leave open
/// (extraction degrades to an empty vec rather than erroring, since a
/// missing `findings` key is the legitimate common case for most skills)
/// WITHOUT imposing hard-refusal on every malformed payload -- only when the
/// skill's own count says findings should exist and none were found is
/// there no ambiguity that something was dropped versus never existed.
/// Pure and unit-testable apart from the `Record` arm.
fn findings_count_contradicts_extraction(
    findings_count: u64,
    raw_findings: &[finding_gate::RawFinding],
) -> bool {
    findings_count > 0 && raw_findings.is_empty()
}

/// Refuse `quality-gate record` on a detached HEAD (#1403):
/// findings are keyed by branch, and `git rev-parse --abbrev-ref HEAD`
/// prints the literal `HEAD` when there is none.
fn refuse_detached_head(branch: &str) -> error::Result<()> {
    if branch == "HEAD" {
        eprintln!(
            "[legion] error: HEAD is detached -- quality-gate keys findings by branch, and a \
             detached HEAD has none. Run 'git switch <branch>' and re-run."
        );
        return Err(error::LegionError::ExitWith(1));
    }
    Ok(())
}

/// Format a finding's location as `<file>` or `<file>:<line>` when a line is
/// present. Shared by every finding print site (#773) so the `:<line>`
/// formatting has one source instead of repeating
/// `line.map(|l| format!(":{l}")).unwrap_or_default()` at each call site.
/// Generic over the line type so both the persisted `i64` (`QualityGateFinding`)
/// and the not-yet-persisted `u32` (`finding_gate::RawFinding`) share it.
fn file_loc(file: &str, line: Option<impl std::fmt::Display>) -> String {
    match line {
        Some(l) => format!("{file}:{l}"),
        None => file.to_owned(),
    }
}

/// Resolve a finding id given in full OR as an unambiguous prefix (#840).
///
/// Convenience, not the fix: the fix is that `finding-list` prints the whole
/// id, so a copied id resolves on the exact-match path below and never
/// reaches the prefix query. This layer only exists so a hand-typed short id
/// still works when it happens to be unique.
///
/// Exact match is tried FIRST, so a full id that happens to be a prefix of a
/// longer one resolves to itself. Not reachable with uniform-length UUIDv7
/// ids today, but the rule must not depend on that staying true.
///
/// More than one match is an error naming every candidate with its
/// `file:line`, never a silent pick -- disposition is a state change, and
/// retiring the wrong finding is worse than making the caller disambiguate.
/// Ambiguity must NOT collapse into `FindingNotFound`: telling a caller that
/// an id which does exist was not found is the exact defect #840 closes, and
/// reproducing it one layer up would not be a fix.
fn resolve_finding_id(database: &db::Database, id: &str) -> error::Result<String> {
    if database.get_finding_by_id(id)?.is_some() {
        return Ok(id.to_owned());
    }
    let mut matches = database.find_findings_by_id_prefix(id)?;
    match matches.len() {
        0 => Err(error::LegionError::FindingNotFound(id.to_owned())),
        1 => Ok(matches.remove(0).id),
        _ => Err(error::LegionError::FindingIdAmbiguous {
            prefix: id.to_owned(),
            candidates: matches
                .iter()
                .map(|f| format!("{}  {}", f.id, file_loc(&f.file, f.line)))
                .collect(),
        }),
    }
}

/// Resolve a gate id given in full OR as an unambiguous prefix (#840).
/// Mirrors `resolve_finding_id`; candidates are named with the skill and
/// commit `quality-gate list` already shows, since gate ids collide the same
/// way finding ids do -- plus `created_at`, because re-running a skill on one
/// commit is routine and leaves rows whose skill AND commit are identical.
/// A candidate list nobody can choose from is the same dead end as no list.
fn resolve_gate_id(database: &db::Database, id: &str) -> error::Result<String> {
    if database.get_quality_gate_by_id(id)?.is_some() {
        return Ok(id.to_owned());
    }
    let mut matches = database.find_quality_gates_by_id_prefix(id)?;
    match matches.len() {
        0 => Err(error::LegionError::QualityGateNotFound(id.to_owned())),
        1 => Ok(matches.remove(0).id),
        _ => Err(error::LegionError::QualityGateIdAmbiguous {
            prefix: id.to_owned(),
            candidates: matches
                .iter()
                .map(|g| {
                    let commit: String = g.commit_hash.chars().take(8).collect();
                    format!("{}  {}  {}  {}", g.id, g.skill, commit, g.created_at)
                })
                .collect(),
        }),
    }
}

/// The finding-resolution gate (#773), run by the Record arm.
///
/// Always reconciles the PENDING set for `branch`/`skill` against
/// `head_commit` first (a fix landed in an earlier commit resolves the
/// finding before it can block anything). When `requesting_clean` is true,
/// refuses the caller with a non-zero exit unless BOTH:
///   - the post-reconcile PENDING set (findings from PRIOR gate runs on this
///     branch+skill) is empty -- no HIGH/MED left unresolved/undispositioned,
///     no LOW left un-acked, and
///   - `current_raw_findings` (THIS call's own structured findings, parsed by
///     the caller from `--details-json` before this
///     function runs) is empty.
///
/// The second half is not optional bookkeeping: legion-review's `approved`
/// decision records `--result clean` in the SAME call as any surviving
/// non-blocking findings (its SKILL.md: "surviving MEDs named in the
/// sign-off"), so a same-run reading of the predicate would let exactly the
/// finding this issue exists to catch sail through on its very first gate
/// call. A finding just extracted in this call has by definition not yet
/// been through resolve/disposition/ack, so its mere presence blocks --
/// there is no severity carve-out here the way there is for the PENDING set
/// (LOW still blocks a same-call clean; it becomes ack-able only once
/// persisted, which a refused call never does).
fn reconcile_and_refuse_if_findings_pending(
    database: &db::Database,
    repo: &str,
    branch: &str,
    skill: &str,
    head_commit: &str,
    requesting_clean: bool,
    current_raw_findings: &[finding_gate::RawFinding],
) -> error::Result<()> {
    if let Err(e) =
        finding_gate::reconcile_pending_findings(database, None, repo, branch, skill, head_commit)
    {
        eprintln!("[legion] warning: finding-resolution reconcile failed (non-fatal): {e}");
    }
    if !requesting_clean {
        return Ok(());
    }
    let pending = database.list_pending_findings(repo, branch, skill)?;
    let refusal = finding_gate::evaluate_refusal(&pending);
    if refusal.blocks() || !current_raw_findings.is_empty() {
        eprintln!(
            "[legion] error: cannot record a clean gate for skill '{skill}' on branch '{branch}' \
             -- {} pending finding(s) from prior run(s) and {} finding(s) reported by THIS run \
             remain unresolved/undispositioned (#773):",
            refusal.blocking.len() + refusal.trivial_unacked.len(),
            current_raw_findings.len(),
        );
        for f in refusal
            .blocking
            .iter()
            .chain(refusal.trivial_unacked.iter())
        {
            let legacy = if f.repo.is_none() {
                ", legacy, no repo recorded"
            } else {
                ""
            };
            eprintln!(
                "  - [prior run, {}{legacy}] {} {} (id {})",
                f.severity.as_str(),
                file_loc(&f.file, f.line),
                f.summary,
                f.id,
            );
        }
        for f in current_raw_findings {
            eprintln!(
                "  - [this run, {}] {} {}",
                f.severity,
                file_loc(&f.file, f.line),
                f.summary,
            );
        }
        eprintln!(
            "\nA clean verdict cannot carry its own findings, nor leave a prior finding \
             unresolved. Re-run with '--result issues' (same findings payload) to persist them, \
             then disposition/ack them -- 'legion quality-gate finding-disposition --id <id> \
             --reason \"...\"' for one, or 'legion quality-gate finding-ack --branch {branch} \
             --skill {skill} --reason \"...\"' to batch-clear LOW findings -- or wait for a fix \
             commit to resolve them automatically, then re-run '--result clean'."
        );
        return Err(error::LegionError::ExitWith(1));
    }
    Ok(())
}

/// Persist structured findings extracted from a gate run, tied to the just-
/// recorded `gate` row (#773). An unparseable severity is treated as MED
/// (fail closed, with a warning) rather than dropped -- dropping a finding
/// here because its severity string was unexpected would reopen the exact
/// evaporation hole this ledger exists to close. A per-finding insert
/// failure is logged and does not abort the rest -- this is additive audit
/// substrate alongside the gate row, not the gate's own success/failure
/// path.
fn persist_raw_findings(
    database: &db::Database,
    repo: &str,
    gate: &QualityGateRow,
    raw_findings: &[finding_gate::RawFinding],
) {
    if raw_findings.is_empty() {
        return;
    }
    // #773: a re-run that reports the same still-open OR already-waived
    // finding (identical file+severity+summary) must not pile up a fresh
    // duplicate PENDING row every time. Two dedup targets, deliberately not
    // just one:
    //   - PENDING: two review passes over an unfixed MED would otherwise
    //     leave two rows to disposition instead of one.
    //   - DISPOSITIONED: a finding explicitly waived ("won't fix:
    //     intentional") must STAY waived if the same run keeps reporting it
    //     -- checking PENDING alone would resurrect a fresh PENDING row the
    //     moment the reviewer honestly re-lists the same MED they already
    //     agreed not to fix, silently undoing the disposition and re-blocking
    //     clean on a decision that was already made. RESOLVED is
    //     deliberately excluded: a finding that recurs identically after
    //     being fix-resolved is a regression worth a fresh PENDING row, not
    //     something to suppress.
    // A query failure degrades to "no known duplicates" rather than blocking
    // the insert below on this best-effort dedup check -- but unlike every
    // other degrade-and-continue path in this function, that failure is now
    // logged (previously silent), since a silent dedup-lookup failure could
    // otherwise resurrect an already-DISPOSITIONED finding with no trace of
    // why. `seen_this_call` extends the same key to duplicates WITHIN one
    // `raw_findings` batch (a single malformed/duplicated `--findings-json`
    // payload listing the same triple twice), not just across separate calls
    // -- `existing_open` alone cannot catch that, since neither copy is in
    // the DB yet when the loop checks it.
    let existing_open: Vec<QualityGateFinding> = database
        .list_findings(&FindingFilter {
            repo: Some(repo.to_owned()),
            branch: Some(gate.branch.clone()),
            skill: Some(gate.skill.clone()),
            status: None,
        })
        .unwrap_or_else(|e| {
            eprintln!(
                "[legion] warning: dedup lookup failed for branch '{}' skill '{}' ({e}) -- \
                 proceeding as if no existing findings are open; a re-reported finding may insert \
                 a duplicate PENDING row this call instead of being recognized as already \
                 tracked (#773).",
                gate.branch, gate.skill
            );
            Vec::new()
        })
        .into_iter()
        .filter(|f| f.status != FindingStatus::Resolved)
        .collect();
    let mut seen_this_call: std::collections::HashSet<(String, FindingSeverity, String)> =
        std::collections::HashSet::new();
    for rf in raw_findings {
        let severity: FindingSeverity = rf.severity.parse().unwrap_or_else(|_| {
            eprintln!(
                "[legion] warning: unknown finding severity '{}' for {} -- treating as MED \
                 (fail closed, #773)",
                rf.severity, rf.file
            );
            FindingSeverity::Med
        });
        let key = (rf.file.clone(), severity, rf.summary.clone());
        let already_open = existing_open
            .iter()
            .any(|f| f.file == rf.file && f.severity == severity && f.summary == rf.summary);
        if already_open || !seen_this_call.insert(key) {
            continue;
        }
        if let Err(e) = database.insert_finding(&NewFindingInput {
            gate_id: &gate.id,
            repo,
            branch: &gate.branch,
            skill: &gate.skill,
            origin_commit: &gate.commit_hash,
            file: &rf.file,
            line: rf.line.map(i64::from),
            severity,
            summary: &rf.summary,
        }) {
            eprintln!(
                "[legion] warning: failed to persist finding for {}: {e}",
                rf.file
            );
        }
    }
}

/// Print finding rows as a human-readable table to stdout (#773 AC4 audit
/// surface). Columns: id (full), repo (`-` when NULL), branch, skill,
/// file:line, severity, status, created (date). An empty slice prints nothing.
///
/// THE ID IS PRINTED IN FULL, and no truncation width may be reintroduced
/// (#840). legion ids are UUIDv7: the leading 48 bits are a millisecond
/// timestamp and, for ids minted inside one millisecond, the random block is
/// held fixed too -- a live gate run put 7 findings behind a shared 24-char
/// prefix. So 12, 16 and 20 are all still ambiguous, and how deep the
/// collision runs is set by insert speed, meaning it gets WORSE on faster
/// hardware, not better. Computing a width from the displayed rows fails for
/// a subtler reason: `finding-list` filters by branch/skill/status while
/// `resolve_finding_id` matches the WHOLE table, so a prefix unique among
/// the rows shown can still be ambiguous against a row the filter hid.
/// Printing all 36 characters is the only width that is correct by
/// construction. CREATED gives up its wall-clock time to pay for it -- rows
/// are already newest-first and `--json` carries the full timestamp.
fn print_findings_table(rows: &[QualityGateFinding]) {
    if rows.is_empty() {
        return;
    }
    println!(
        "{:<36}  {:<16}  {:<20}  {:<16}  {:<30}  {:<4}  {:<14}  CREATED",
        "ID", "REPO", "BRANCH", "SKILL", "FILE", "SEV", "STATUS"
    );
    println!("{}", "-".repeat(160));
    for row in rows {
        let repo_trunc: String = row
            .repo
            .as_deref()
            .unwrap_or("-")
            .chars()
            .take(16)
            .collect();
        let branch_trunc: String = row.branch.chars().take(20).collect();
        let skill_trunc: String = row.skill.chars().take(16).collect();
        let file_trunc: String = file_loc(&row.file, row.line).chars().take(30).collect();
        let created_date: String = row.created_at.chars().take(10).collect();
        println!(
            "{:<36}  {:<16}  {:<20}  {:<16}  {:<30}  {:<4}  {:<14}  {}",
            row.id,
            repo_trunc,
            branch_trunc,
            skill_trunc,
            file_trunc,
            row.severity.as_str(),
            row.status.as_str(),
            created_date,
        );
    }
}

/// Print gate rows as a human-readable table to stdout.
///
/// Columns: id (full), branch, commit (first 8 chars), skill, result,
/// findings, provenance, void, created (date). An empty slice prints
/// nothing. The id is printed in full for the same reason as
/// `print_findings_table` -- `void --id` consumes what this prints.
///
/// PROVENANCE and VOID surface #780's audit distinction on the table a human
/// actually reads by default: PROVENANCE separates a structurally VALIDATED
/// clean from a merely ASSERTED one, and VOID marks a row retired as
/// known-false ("-" for a live row, "VOID" for a voided one) so a retired
/// row never visually blends in with a live one. `--json` (see
/// `QualityGateRow`'s `Serialize`) already carries the full
/// `voided_at`/`void_reason`/`superseded_by` detail for tooling; this table
/// is the quick-glance surface.
fn print_gate_table(rows: &[QualityGateRow]) {
    if rows.is_empty() {
        return;
    }
    println!(
        "{:<36}  {:<20}  {:<8}  {:<22}  {:<6}  {:>8}  {:<9}  {:<4}  CREATED",
        "ID", "BRANCH", "COMMIT", "SKILL", "RESULT", "FINDINGS", "PROVENANCE", "VOID"
    );
    println!("{}", "-".repeat(139));
    for row in rows {
        let branch_trunc: String = row.branch.chars().take(20).collect();
        let commit_short: String = row.commit_hash.chars().take(8).collect();
        let skill_trunc: String = row.skill.chars().take(22).collect();
        let void_marker = if row.voided_at.is_some() { "VOID" } else { "-" };
        let created_date: String = row.created_at.chars().take(10).collect();
        println!(
            "{:<36}  {:<20}  {:<8}  {:<22}  {:<6}  {:>8}  {:<9}  {:<4}  {}",
            row.id,
            branch_trunc,
            commit_short,
            skill_trunc,
            row.result.as_str(),
            row.findings_count,
            row.provenance.as_str(),
            void_marker,
            created_date,
        );
    }
}

/// Print per-skill stats as a human-readable table to stdout.
///
/// Columns: skill, runs, clean, issues, catch_rate (%), total_findings,
/// max_findings. An empty slice prints nothing.
fn print_stats_table(stats: &[QualityGateStats]) {
    if stats.is_empty() {
        return;
    }
    println!(
        "{:<25}  {:>5}  {:>5}  {:>6}  {:>10}  {:>14}  {:>12}",
        "SKILL", "RUNS", "CLEAN", "ISSUES", "CATCH_RATE", "TOTAL_FINDINGS", "MAX_FINDINGS"
    );
    println!("{}", "-".repeat(88));
    for s in stats {
        println!(
            "{:<25}  {:>5}  {:>5}  {:>6}  {:>9.1}%  {:>14}  {:>12}",
            s.skill,
            s.runs,
            s.clean,
            s.issues,
            s.catch_rate * 100.0,
            s.total_findings,
            s.max_findings,
        );
    }
}

/// Read the id-carrying `verification.criteria` array off a document's
/// payload (#882 step 1). Returns `Ok(None)` when the array is absent or
/// empty (a corrupt/unparseable payload also degrades to `Ok(None)`),
/// signaling the caller should fall back to the legacy prose-evidence
/// format. When the array is present and non-empty, every entry must
/// resolve to a usable `id`+`text` pair or the call REFUSES (HIGH-2 review
/// fix): silently dropping a malformed entry would shrink the id set
/// `verify::decide_spec` checks citations against, letting a verdict that
/// cites a dropped id read as "unknown" instead of the real problem
/// (a malformed spec), and letting coverage close over fewer criteria
/// than the document actually declares.
pub(crate) fn resolve_spec_criteria(
    doc: &documents::Document,
) -> error::Result<Option<Vec<verify::SpecCriterion>>> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&doc.payload) else {
        return Ok(None);
    };
    let Some(arr) = value
        .get("verification")
        .and_then(|v| v.get("criteria"))
        .and_then(|c| c.as_array())
    else {
        return Ok(None);
    };
    if arr.is_empty() {
        return Ok(None);
    }

    let mut criteria: Vec<verify::SpecCriterion> = Vec::with_capacity(arr.len());
    for (idx, entry) in arr.iter().enumerate() {
        let id = entry.get("id").and_then(|v| v.as_str()).map(str::trim);
        let text = entry.get("text").and_then(|v| v.as_str()).map(str::trim);
        match (id, text) {
            (Some(id), Some(text)) if !id.is_empty() && !text.is_empty() => {
                criteria.push(verify::SpecCriterion {
                    id: id.to_string(),
                    text: text.to_string(),
                });
            }
            _ => {
                return Err(error::LegionError::WorkSource(format!(
                    "document '{}' verification.criteria[{idx}] is malformed -- both 'id' \
                     and 'text' must be non-empty strings; a malformed spec entry is \
                     refused, not silently dropped",
                    doc.id
                )));
            }
        }
    }
    Ok(Some(criteria))
}

/// `legion verify` (#913): issues are the work source of record. #931
/// removed the legacy `--card` path this used to dispatch to alongside
/// `--issue`.
pub(crate) fn handle_verify(
    repo: String,
    issue: u64,
    verdicts_file: Option<String>,
    deviation: Option<String>,
) -> error::Result<()> {
    handle_verify_issue(&repo, issue, verdicts_file, deviation)
}

/// Resolve an issue's `## Traces to` requirement bullets against live
/// document state (#933), one `TracedRequirement` per bullet. `- None`
/// bullets and an absent section resolve to an empty vec -- untraced.
/// Shared by `handle_verify_issue` and `cli::pr`'s traced-criteria
/// rendering, so the pr-write gate and the verify gate cannot drift in how
/// they read the same trace.
///
/// Fails closed on the conditions `cli::issue::validate_trace` enforces at
/// create time, re-checked live rather than trusted (a body edit can bypass
/// the create-time validator): a requirement that does not exist, one whose
/// status is `cancelled`, one with no id-carrying `verification.criteria`,
/// and a `[criteria: ...]` bracket citing an id the requirement no longer
/// contains. An unevaluated check is not a passed check, so none of these
/// degrade to rendering or verifying nothing.
pub(crate) fn resolve_traced_requirements(
    database: &db::Database,
    trace: &[card_parse::TraceBullet],
) -> error::Result<Vec<verify::TracedRequirement>> {
    let mut requirements = Vec::new();
    for bullet in trace {
        // Same bracket-defect refusal `cli::issue::validate_trace` applies
        // at create time (#945 review) -- re-checked here because a body
        // edit bypasses the create-time validator, and a degraded bracket
        // (empty, unclosed, repeated) silently mis-scopes what this issue
        // is judged against.
        if let Some(defect) = card_parse::trace_bullet_bracket_defect(bullet) {
            return Err(error::LegionError::WorkSource(format!(
                "## Traces to: {defect}"
            )));
        }
        let card_parse::TraceBullet::Requirement {
            document_id,
            criteria,
            ..
        } = bullet
        else {
            continue;
        };
        let doc = database.get_document(document_id)?.ok_or_else(|| {
            error::LegionError::WorkSource(format!(
                "issue traces to requirement '{document_id}', which does not exist"
            ))
        })?;
        if doc.status == "cancelled" {
            return Err(error::LegionError::WorkSource(format!(
                "issue traces to requirement '{document_id}', which is cancelled -- a \
                 cancelled requirement is not a trace"
            )));
        }
        let spec_criteria = resolve_spec_criteria(&doc)?.ok_or_else(|| {
            error::LegionError::WorkSource(format!(
                "requirement '{document_id}' has no id-carrying verification.criteria to \
                 verify against"
            ))
        })?;
        let scoped: Vec<verify::SpecCriterion> = match criteria {
            Some(ids) => {
                let mut out = Vec::with_capacity(ids.len());
                for id in ids {
                    let found = spec_criteria.iter().find(|c| &c.id == id).ok_or_else(|| {
                        error::LegionError::WorkSource(format!(
                            "issue cites criterion '{id}' for requirement '{document_id}', \
                             which no longer contains it"
                        ))
                    })?;
                    out.push(found.clone());
                }
                out
            }
            None => spec_criteria,
        };
        // Propagated, not defaulted: a fabricated fallback revision would
        // corrupt the staleness check `decide_spec_multi` performs with
        // this value -- wrongly refusing fresh verdicts or accepting stale
        // ones whenever the read fails.
        let revision = database.document_revision(&doc.id)?;
        requirements.push(verify::TracedRequirement {
            doc_id: doc.id.clone(),
            revision,
            criteria: scoped,
        });
    }
    Ok(requirements)
}

/// Verify a work-source issue's acceptance criteria, with no card involved
/// (#913).
///
/// Criteria come from the issue body via `card_parse::parse_issue_body`, the
/// same reader `pr write-check --issue` uses. That sharing is the point: the
/// gate that lets a PR open and the gate that closes the work now read one
/// text, so they cannot disagree about what was promised.
///
/// #933: when the issue body carries a `## Traces to` section naming at
/// least one requirement, criteria resolve from THAT requirement's
/// `verification.criteria` for the serviced ids, not from the issue's own
/// restated criteria -- the issue's acceptance criteria define the slice's
/// scope, but the requirement's criteria are what the verdict is judged
/// against, so spec fidelity survives the issue the same way it survives a
/// card. The trace is re-resolved against LIVE document state here (not
/// trusted from `cli::issue`'s create-time validation, since `legion issue
/// edit` can change the body after creation): a requirement that no longer
/// exists, has gone `cancelled`, or no longer contains a cited criterion id
/// refuses the run, matching the create-time rule's exact conditions.
/// Verdicts take the id-carrying `SpecAcResult` shape in this case, pinning
/// the document id and revision each criterion belongs to
/// (`verify::decide_spec_multi`, which generalizes the card path's
/// `decide_spec` to more than one traced requirement).
///
/// An untraced issue (no `## Traces to` section, or only the explicit
/// `- None` no-requirement bullet) keeps the pre-#933 behavior exactly:
/// free-text `AcResult` verdicts against the issue's own `acceptance` list.
///
/// What this path deliberately does NOT do, versus the card path:
///
/// - No card-bound-document precedence. Binding a document via `kanban
///   bind` is a card operation; an issue-shaped repo resolves its spec
///   through the trace instead (see above), or has none.
/// - No status transition. There is no card to move to Done or NeedsInput;
///   the verdict IS the recorded gate row plus the exit code.
/// - No `--deviation`. That gate is adjudicated against a card's
///   `ReplanRecord`, and with no card there is nothing to ratify against --
///   so it refuses rather than silently accepting an assertion nothing checks.
fn handle_verify_issue(
    repo: &str,
    issue: u64,
    verdicts_file: Option<String>,
    deviation: Option<String>,
) -> error::Result<()> {
    if deviation.is_some() {
        return Err(error::LegionError::WorkSource(
            "--deviation is not supported for issue verification (#931 retired the card \
             it was adjudicated against, and no issue-shaped replacement exists). Revise \
             the issue's acceptance criteria instead, then verify against them."
                .into(),
        ));
    }

    let database = open_db()?;
    let (plugin, source_repo, _workdir) = worksource::require_worksource(repo)?;

    let ext = worksource::view_issue(&plugin, &source_repo, issue)?;
    let parsed = card_parse::parse_issue_body(ext.body.as_deref().unwrap_or(""));

    let target = VerifyTarget::issue(&source_repo, issue);

    // #933: one resolution, two gates -- the same shared resolver
    // `cli::pr`'s traced-criteria rendering uses, so pr-write and verify
    // cannot drift in how they read a trace. Empty means untraced: no
    // `## Traces to` section, or only the explicit `- None` spelling.
    let requirements = resolve_traced_requirements(&database, &parsed.trace)?;

    let raw = read_file_or_stdin(verdicts_file.as_deref(), "--verdicts-file")?;

    if requirements.is_empty() {
        let acceptance = parsed.acceptance;
        let ac_source = format!("issue:{source_repo}#{issue}");
        let results: Vec<verify::AcResult> = serde_json::from_str(&raw).map_err(|e| {
            error::LegionError::WorkSource(format!(
                "failed to parse verdicts JSON (expected a list of \
                 {{criterion, verdict, evidence}}): {e}"
            ))
        })?;
        let decision = verify::decide(&acceptance, &results);
        let results_value = serde_json::to_value(&results)?;

        return finish_verify(
            &database,
            &target,
            &ac_source,
            acceptance.len(),
            decision,
            results_value,
        );
    }

    let criteria_count: usize = requirements.iter().map(|r| r.criteria.len()).sum();
    let doc_ids: Vec<&str> = requirements.iter().map(|r| r.doc_id.as_str()).collect();
    let ac_source = format!("trace:{}", doc_ids.join(","));

    let results: Vec<verify::SpecAcResult> = serde_json::from_str(&raw).map_err(|e| {
        error::LegionError::WorkSource(format!(
            "failed to parse spec verdicts JSON (expected a list of \
             {{spec_doc_id, spec_revision, criterion_id, verdict, artifacts}}): {e}"
        ))
    })?;
    let decision = verify::decide_spec_multi(&requirements, &results);
    let results_value = serde_json::to_value(&results)?;

    finish_verify(
        &database,
        &target,
        &ac_source,
        criteria_count,
        decision,
        results_value,
    )
}

/// What a verify verdict is bound to: a work-source issue (#913). #931
/// removed the card-bound target this used to be an enum over
/// (`VerifyTarget::Card`) -- there is only one shape now, so this is a
/// plain struct rather than a single-variant enum.
struct VerifyTarget {
    gate_key: String,
    label: String,
}

impl VerifyTarget {
    fn issue(source_repo: &str, issue: u64) -> Self {
        Self {
            gate_key: verify::verify_gate_key_for_issue(source_repo, issue),
            label: format!("issue {source_repo}#{issue}"),
        }
    }
}

fn finish_verify(
    database: &crate::db::Database,
    target: &VerifyTarget,
    ac_source: &str,
    criteria_count: usize,
    decision: verify::VerifyDecision,
    results: serde_json::Value,
) -> error::Result<()> {
    // Record the verdict as a target-keyed gate so `legion done` can gate
    // on it regardless of which commit it runs on (e.g. post-merge).
    let skill = target.gate_key.clone();
    let card = target.label.clone();
    let (commit_hash, branch) = git_head_commit_and_branch()?;
    let details = serde_json::json!({
        "skill": "legion-verify",
        "issue": target.label,
        "ac_source": ac_source,
        "decision": format!("{decision:?}"),
        "results": results,
    })
    .to_string();
    let findings = match &decision {
        verify::VerifyDecision::Block { failed } => failed.len() as u64,
        verify::VerifyDecision::NeedsInput { uncertain } => uncertain.len() as u64,
        verify::VerifyDecision::Incomplete { unaddressed } => *unaddressed as u64,
        verify::VerifyDecision::NoCheckableAc => 1,
        verify::VerifyDecision::InvalidCriterionReference { details } => details.len() as u64,
        verify::VerifyDecision::Proceed => 0,
    };
    let gate_result = if decision.allows_done() {
        GateResult::Clean
    } else {
        GateResult::Issues
    };
    database.record_quality_gate(&QualityGateInput {
        branch: &branch,
        commit_hash: &commit_hash,
        skill: &skill,
        result: gate_result,
        findings_count: findings,
        details: Some(&details),
        // legion-verify has no check validator -- asserted by necessity (#780).
        provenance: GateProvenance::Asserted,
        base: None,
    })?;

    match decision {
        verify::VerifyDecision::Proceed => {
            println!(
                "[legion] verify PASS for {card} ({criteria_count} criteria, source: {ac_source}). ->Done is unblocked.",
            );
        }
        verify::VerifyDecision::NoCheckableAc => {
            eprintln!(
                "[legion] verify BLOCKED for {card}: no acceptance criteria to check. \
                 Work cannot reach Done without checkable criteria -- add them upstream."
            );
            return Err(error::LegionError::ExitWith(1));
        }
        verify::VerifyDecision::Incomplete { unaddressed } => {
            eprintln!(
                "[legion] verify BLOCKED for {card}: {unaddressed} of {criteria_count} \
                 criteria have no verdict. Emit one verdict per criterion.",
            );
            return Err(error::LegionError::ExitWith(1));
        }
        verify::VerifyDecision::Block { failed } => {
            eprintln!(
                "[legion] verify FAIL for {card} -- {} criterion(s) not satisfied:",
                failed.len()
            );
            for c in &failed {
                eprintln!("  - {c}");
            }
            eprintln!("\n->Done is blocked. Finish the work and re-verify.");
            return Err(error::LegionError::ExitWith(1));
        }
        verify::VerifyDecision::NeedsInput { uncertain } => {
            eprintln!(
                "[legion] verify UNCERTAIN for {card} -- {} criterion(s) cannot be \
                 mechanically confirmed:",
                uncertain.len()
            );
            for c in &uncertain {
                eprintln!("  - {c}");
            }
            // Route to a human rather than rubber-stamp ->Done. The gate is
            // already recorded non-clean, so ->Done stays blocked regardless.
            // There is no card to transition (#931): the non-clean gate row
            // and the exit code ARE the block, and the human adjudication
            // happens on the issue.
            eprintln!(
                "\nRecorded as a non-clean verify gate. Adjudicate on the issue, then re-verify."
            );
            return Err(error::LegionError::ExitWith(1));
        }
        verify::VerifyDecision::InvalidCriterionReference { details: bad_refs } => {
            eprintln!(
                "[legion] verify BLOCKED for {card} -- {} verdict(s) cite a spec \
                 reference that does not resolve:",
                bad_refs.len()
            );
            for d in &bad_refs {
                eprintln!("  - {d}");
            }
            eprintln!(
                "\n->Done is blocked. A verdict must name the exact spec document, \
                 revision, and criterion id it was formed against."
            );
            return Err(error::LegionError::ExitWith(1));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil::test_db;
    use crate::documents::DocumentMeta;

    // --- #840: the printed id must be usable by the consuming verb --------

    fn finding_input<'a>(gate_id: &'a str, file: &'a str) -> NewFindingInput<'a> {
        NewFindingInput {
            gate_id,
            repo: "legion",
            branch: "feat/x",
            skill: "legion-simplify",
            origin_commit: "commit-a",
            file,
            line: Some(42),
            severity: FindingSeverity::Med,
            summary: "duplicate logic in two match arms",
        }
    }

    fn gate_input<'a>(skill: &'a str, commit: &'a str) -> QualityGateInput<'a> {
        QualityGateInput {
            branch: "feat/x",
            commit_hash: commit,
            skill,
            result: GateResult::Issues,
            findings_count: 1,
            details: None,
            provenance: GateProvenance::Validated,
            base: None,
        }
    }

    #[test]
    fn resolve_finding_id_takes_a_full_id_and_an_unambiguous_prefix() {
        let db = test_db();
        let row = db
            .insert_finding(&finding_input("gate-1", "src/foo.rs"))
            .unwrap();

        // The full id -- what the table now prints -- is the exact path.
        assert_eq!(resolve_finding_id(&db, &row.id).unwrap(), row.id);
        // A hand-typed short id still works while it is unique.
        let prefix: String = row.id.chars().take(8).collect();
        assert_eq!(resolve_finding_id(&db, &prefix).unwrap(), row.id);
    }

    #[test]
    fn resolve_finding_id_refuses_a_shared_prefix_naming_choosable_candidates() {
        let db = test_db();
        let a = db
            .insert_finding(&finding_input("gate-1", "src/foo.rs"))
            .unwrap();
        let b = db
            .insert_finding(&finding_input("gate-1", "src/bar.rs"))
            .unwrap();

        // The shingle case: two findings persisted by one gate run. UUIDv7's
        // leading hex is a millisecond timestamp, so the 8 characters the
        // table used to print collide.
        let prefix: String = a.id.chars().take(8).collect();
        assert_eq!(
            prefix,
            b.id.chars().take(8).collect::<String>(),
            "two findings from one gate run must share their leading 8 chars"
        );

        let err = resolve_finding_id(&db, &prefix).unwrap_err();
        assert!(
            matches!(err, error::LegionError::FindingIdAmbiguous { .. }),
            "ambiguity must not collapse into not-found: got {err:?}"
        );
        // The candidate list has to be CHOOSABLE, not just present: full ids
        // plus the file:line the caller already knows. Ids alone would be
        // near-identical UUIDs and no help at all.
        let msg = err.to_string();
        assert!(
            msg.contains(&a.id) && msg.contains(&b.id),
            "missing full ids: {msg}"
        );
        assert!(
            msg.contains("src/foo.rs:42") && msg.contains("src/bar.rs:42"),
            "candidates must carry file:line: {msg}"
        );

        // The refused call changed nothing, and each full id still works.
        assert_eq!(
            db.get_finding_by_id(&a.id).unwrap().unwrap().status,
            FindingStatus::Pending
        );
        assert_eq!(
            db.get_finding_by_id(&b.id).unwrap().unwrap().status,
            FindingStatus::Pending
        );
        for id in [&a.id, &b.id] {
            let full = resolve_finding_id(&db, id).unwrap();
            let disposed = db.dispose_finding(&full, "won't fix: intentional").unwrap();
            assert_eq!(disposed.status, FindingStatus::Dispositioned);
        }
    }

    #[test]
    fn resolve_finding_id_unknown_reports_not_found_naming_json() {
        let db = test_db();
        db.insert_finding(&finding_input("gate-1", "src/foo.rs"))
            .unwrap();
        let err = resolve_finding_id(&db, "deadbeef").unwrap_err();
        assert!(
            matches!(err, error::LegionError::FindingNotFound(_)),
            "expected FindingNotFound, got {err:?}"
        );
        assert!(
            err.to_string().contains("--json"),
            "the escape hatch has to be named where the caller is stuck: {err}"
        );
    }

    #[test]
    fn resolve_gate_id_takes_a_prefix_and_refuses_ambiguity() {
        let db = test_db();
        let a = db
            .record_quality_gate(&gate_input("legion-simplify", "commit-a"))
            .unwrap();
        let b = db
            .record_quality_gate(&gate_input("legion-review", "commit-b"))
            .unwrap();

        assert_eq!(resolve_gate_id(&db, &a.id).unwrap(), a.id);

        let prefix: String = a.id.chars().take(8).collect();
        let err = resolve_gate_id(&db, &prefix).unwrap_err();
        assert!(
            matches!(err, error::LegionError::QualityGateIdAmbiguous { .. }),
            "expected QualityGateIdAmbiguous, got {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains(&a.id) && msg.contains(&b.id),
            "missing full ids: {msg}"
        );
        assert!(
            msg.contains("legion-simplify") && msg.contains("legion-review"),
            "gate candidates must carry their skill: {msg}"
        );
        assert!(
            msg.contains("commit-a") && msg.contains("commit-b") && msg.contains(&a.created_at),
            "gate candidates must carry commit and created_at: {msg}"
        );

        // The shortest prefix that separates the two rows is unique by
        // construction, so it must resolve rather than error.
        let unique_len =
            a.id.chars()
                .zip(b.id.chars())
                .take_while(|(x, y)| x == y)
                .count()
                + 1;
        let unique: String = a.id.chars().take(unique_len).collect();
        assert_eq!(resolve_gate_id(&db, &unique).unwrap(), a.id);

        // And the resolved id is what void consumes.
        let voided = db
            .void_quality_gate(
                &resolve_gate_id(&db, &unique).unwrap(),
                "false verdict",
                None,
            )
            .unwrap();
        assert!(voided.voided_at.is_some());
    }

    #[test]
    fn resolve_gate_id_unknown_reports_not_found_naming_json() {
        let db = test_db();
        db.record_quality_gate(&gate_input("legion-simplify", "commit-a"))
            .unwrap();
        let err = resolve_gate_id(&db, "deadbeef").unwrap_err();
        assert!(
            matches!(err, error::LegionError::QualityGateNotFound(_)),
            "expected QualityGateNotFound, got {err:?}"
        );
        assert!(err.to_string().contains("--json"), "{err}");
    }

    /// resolve_spec_criteria reads the id-carrying array; returns None when
    /// the document only carries the legacy string-array shape, so
    /// handle_verify's format dispatch falls back to the free-text path.
    #[test]
    fn resolve_spec_criteria_reads_ids_and_texts() {
        let db = test_db();
        crate::db::testutil::seed_type_schema(&db, "requirement");
        let meta = DocumentMeta {
            id: Some("doc-spec-crit"),
            doc_type: "requirement",
            surface: None,
            status: None,
            priority: None,
            owner: "legion",
        };
        let payload = serde_json::json!({
            "verification": {"criteria": [{"id": "crit-1", "text": "alpha"}]}
        })
        .to_string();
        let doc = db.insert_document(&meta, &payload).expect("insert");
        let criteria = resolve_spec_criteria(&doc)
            .expect("resolve")
            .expect("criteria present");
        assert_eq!(criteria.len(), 1);
        assert_eq!(criteria[0].id, "crit-1");
        assert_eq!(criteria[0].text, "alpha");
    }

    #[test]
    fn resolve_spec_criteria_none_for_legacy_acceptance_shape() {
        let db = test_db();
        crate::db::testutil::seed_type_schema(&db, "requirement");
        let meta = DocumentMeta {
            id: Some("doc-legacy"),
            doc_type: "requirement",
            surface: None,
            status: None,
            priority: None,
            owner: "legion",
        };
        let payload = serde_json::json!({
            "verification": {"acceptance": ["plain string criterion"]}
        })
        .to_string();
        let doc = db.insert_document(&meta, &payload).expect("insert");
        assert!(resolve_spec_criteria(&doc).expect("resolve").is_none());
    }

    /// HIGH-2 (#882 review): `resolve_spec_criteria` must REFUSE an entry
    /// whose `id` or `text` is missing, blank, or non-string, rather than
    /// silently dropping it -- the same fail-open hole as
    /// `resolve_acceptance_criteria`, one layer deeper (the id-carrying
    /// format `handle_verify` validates `SpecAcResult` citations against).
    ///
    /// A missing `id` alone cannot reach this code path: `insert_document`
    /// normalizes it by assigning a fresh UUIDv7 before the row is ever
    /// written (`normalize_criteria`). Missing `text` gets no such
    /// backfill, so it is what a genuinely malformed stored entry looks
    /// like.
    #[test]
    fn resolve_spec_criteria_refuses_entry_missing_text() {
        let db = test_db();
        crate::db::testutil::seed_type_schema(&db, "requirement");
        let meta = DocumentMeta {
            id: Some("doc-spec-malformed"),
            doc_type: "requirement",
            surface: None,
            status: None,
            priority: None,
            owner: "legion",
        };
        let payload = serde_json::json!({
            "verification": {
                "criteria": [
                    {"id": "crit-1", "text": "good one"},
                    {"id": "crit-2"}
                ]
            }
        })
        .to_string();
        let doc = db.insert_document(&meta, &payload).expect("insert");
        let err = resolve_spec_criteria(&doc)
            .expect_err("a criterion entry missing 'text' must be refused, not dropped");
        assert!(
            err.to_string().contains("criteria[1] is malformed"),
            "error must name the offending index, got: {err}"
        );
    }

    // --- finding-resolution gate wiring tests (#773) ---

    // -- findings_count_contradicts_extraction (pure predicate) --

    #[test]
    fn findings_count_contradicts_extraction_true_when_count_positive_and_extraction_empty() {
        assert!(findings_count_contradicts_extraction(3, &[]));
    }

    #[test]
    fn findings_count_contradicts_extraction_false_when_count_zero() {
        assert!(!findings_count_contradicts_extraction(0, &[]));
    }

    #[test]
    fn findings_count_contradicts_extraction_false_when_extraction_non_empty() {
        let raw = vec![finding_gate::RawFinding {
            file: "src/foo.rs".to_string(),
            line: None,
            severity: "MED".to_string(),
            summary: "x".to_string(),
        }];
        assert!(!findings_count_contradicts_extraction(1, &raw));
    }

    #[test]
    fn findings_count_contradicts_extraction_true_for_issues_result_regression_1008() {
        // #1008 regression guard: this exact input (an `issues`-shaped call
        // whose asserted findings_count contradicts a zero extraction) used
        // to assert FALSE here, with a comment claiming an `issues` result
        // with a mismatched count was "a separate (unenforced,
        // informational-field) concern, not this gate" -- that was the bug.
        // The predicate no longer takes a `GateResult` at all, so this is no
        // longer distinguishable from the general case at the unit level;
        // it is kept as a named regression test so the historical bug this
        // issue closed stays documented and provable by name.
        assert!(findings_count_contradicts_extraction(3, &[]));
    }

    /// `reconcile_and_refuse_if_findings_pending` refuses a `clean` request
    /// when a HIGH finding from a prior run on the same branch+skill is
    /// still PENDING. `origin_commit == head_commit` short-circuits the git
    /// reconcile call (no real git repo state needed for this test).
    #[test]
    fn reconcile_and_refuse_blocks_clean_when_high_finding_pending() {
        let db = test_db();
        db.insert_finding(&NewFindingInput {
            gate_id: "gate-1",
            repo: "legion",
            branch: "feat/x",
            skill: "legion-simplify",
            origin_commit: "commit-a",
            file: "src/foo.rs",
            line: Some(10),
            severity: FindingSeverity::High,
            summary: "unchecked input",
        })
        .unwrap();

        let err = reconcile_and_refuse_if_findings_pending(
            &db,
            "legion",
            "feat/x",
            "legion-simplify",
            "commit-a",
            true,
            &[],
        )
        .unwrap_err();
        assert!(matches!(err, error::LegionError::ExitWith(1)));
    }

    /// A LOW finding also blocks `clean` until batch-acked (#773 AC3) --
    /// regression guard against silently treating LOW as non-blocking.
    #[test]
    fn reconcile_and_refuse_blocks_clean_when_low_finding_unacked() {
        let db = test_db();
        db.insert_finding(&NewFindingInput {
            gate_id: "gate-1",
            repo: "legion",
            branch: "feat/x",
            skill: "legion-simplify",
            origin_commit: "commit-a",
            file: "src/foo.rs",
            line: Some(10),
            severity: FindingSeverity::Low,
            summary: "naming nit",
        })
        .unwrap();

        let err = reconcile_and_refuse_if_findings_pending(
            &db,
            "legion",
            "feat/x",
            "legion-simplify",
            "commit-a",
            true,
            &[],
        )
        .unwrap_err();
        assert!(matches!(err, error::LegionError::ExitWith(1)));
    }

    /// Once the pending finding is dispositioned, the same (branch, skill,
    /// head_commit) request no longer refuses.
    #[test]
    fn reconcile_and_refuse_allows_clean_after_disposition() {
        let db = test_db();
        let finding = db
            .insert_finding(&NewFindingInput {
                gate_id: "gate-1",
                repo: "legion",
                branch: "feat/x",
                skill: "legion-simplify",
                origin_commit: "commit-a",
                file: "src/foo.rs",
                line: Some(10),
                severity: FindingSeverity::High,
                summary: "unchecked input",
            })
            .unwrap();
        db.dispose_finding(&finding.id, "won't fix: intentional")
            .unwrap();

        reconcile_and_refuse_if_findings_pending(
            &db,
            "legion",
            "feat/x",
            "legion-simplify",
            "commit-a",
            true,
            &[],
        )
        .expect("clean should be allowed once the pending finding is dispositioned");
    }

    /// An empty pending set never blocks -- the common case (no findings
    /// ever raised for this branch+skill).
    #[test]
    fn reconcile_and_refuse_allows_clean_with_no_pending_findings() {
        let db = test_db();
        reconcile_and_refuse_if_findings_pending(
            &db,
            "legion",
            "feat/x",
            "legion-simplify",
            "commit-a",
            true,
            &[],
        )
        .expect("no pending findings should never refuse");
    }

    /// A pending HIGH finding does NOT block an `issues` request -- the
    /// refusal only ever applies to a `clean` claim (#773 AC1).
    #[test]
    fn reconcile_and_refuse_does_not_block_issues_result() {
        let db = test_db();
        db.insert_finding(&NewFindingInput {
            gate_id: "gate-1",
            repo: "legion",
            branch: "feat/x",
            skill: "legion-simplify",
            origin_commit: "commit-a",
            file: "src/foo.rs",
            line: Some(10),
            severity: FindingSeverity::High,
            summary: "unchecked input",
        })
        .unwrap();

        reconcile_and_refuse_if_findings_pending(
            &db,
            "legion",
            "feat/x",
            "legion-simplify",
            "commit-a",
            false,
            &[],
        )
        .expect("an issues (non-clean) request must never be refused by the pending set");
    }

    /// Findings on a different branch or skill never leak into this
    /// branch+skill's refusal decision.
    #[test]
    fn reconcile_and_refuse_scoped_to_branch_and_skill() {
        let db = test_db();
        db.insert_finding(&NewFindingInput {
            gate_id: "gate-1",
            repo: "legion",
            branch: "feat/other",
            skill: "legion-simplify",
            origin_commit: "commit-a",
            file: "src/foo.rs",
            line: Some(10),
            severity: FindingSeverity::High,
            summary: "unchecked input",
        })
        .unwrap();
        db.insert_finding(&NewFindingInput {
            gate_id: "gate-2",
            repo: "legion",
            branch: "feat/x",
            skill: "legion-review",
            origin_commit: "commit-a",
            file: "src/foo.rs",
            line: Some(10),
            severity: FindingSeverity::High,
            summary: "unchecked input",
        })
        .unwrap();

        reconcile_and_refuse_if_findings_pending(
            &db,
            "legion",
            "feat/x",
            "legion-simplify",
            "commit-a",
            true,
            &[],
        )
        .expect("findings on another branch/skill must not block this one");
    }

    /// THE central case (#773): a clean request that itself carries a
    /// finding must be refused by that SAME call, not merely a future one.
    /// This is legion-review's `approved` decision recording `--result
    /// clean` in the same invocation as any surviving non-blocking findings
    /// (its SKILL.md: "surviving MEDs named in the sign-off") -- the pending
    /// set is empty (nothing persisted yet), so only checking the pending
    /// set would let this sail through.
    #[test]
    fn reconcile_and_refuse_blocks_clean_when_current_call_carries_a_finding() {
        let db = test_db();
        let current = vec![finding_gate::RawFinding {
            file: "src/foo.rs".to_string(),
            line: Some(10),
            severity: "MED".to_string(),
            summary: "unchecked input".to_string(),
        }];

        let err = reconcile_and_refuse_if_findings_pending(
            &db,
            "legion",
            "feat/x",
            "legion-review",
            "commit-a",
            true,
            &current,
        )
        .unwrap_err();
        assert!(matches!(err, error::LegionError::ExitWith(1)));
    }

    /// The same-call refusal blocks on a LOW finding too -- a freshly
    /// extracted finding has never been through batch-ack, so its severity
    /// does not exempt it (mirrors `evaluate_refusal_low_blocks_until_acked`
    /// for the PENDING-set case).
    #[test]
    fn reconcile_and_refuse_blocks_clean_when_current_call_carries_a_low_finding() {
        let db = test_db();
        let current = vec![finding_gate::RawFinding {
            file: "src/foo.rs".to_string(),
            line: None,
            severity: "LOW".to_string(),
            summary: "naming nit".to_string(),
        }];

        let err = reconcile_and_refuse_if_findings_pending(
            &db,
            "legion",
            "feat/x",
            "legion-review",
            "commit-a",
            true,
            &current,
        )
        .unwrap_err();
        assert!(matches!(err, error::LegionError::ExitWith(1)));
    }

    /// A same-call finding does NOT block an `issues` request -- only a
    /// `clean` claim triggers the refusal (mirrors
    /// `reconcile_and_refuse_does_not_block_issues_result` for the
    /// PENDING-set case).
    #[test]
    fn reconcile_and_refuse_current_call_findings_do_not_block_issues_result() {
        let db = test_db();
        let current = vec![finding_gate::RawFinding {
            file: "src/foo.rs".to_string(),
            line: Some(10),
            severity: "HIGH".to_string(),
            summary: "unchecked input".to_string(),
        }];

        reconcile_and_refuse_if_findings_pending(
            &db,
            "legion",
            "feat/x",
            "legion-review",
            "commit-a",
            false,
            &current,
        )
        .expect("an issues (non-clean) request must never be refused by this run's findings");
    }

    /// A clean call with an empty pending set AND an empty current-findings
    /// slice is allowed -- the ordinary case (a genuinely clean run, or a
    /// prior-commit fix already reconciled away everything).
    #[test]
    fn reconcile_and_refuse_allows_clean_with_no_pending_and_no_current_findings() {
        let db = test_db();
        reconcile_and_refuse_if_findings_pending(
            &db,
            "legion",
            "feat/x",
            "legion-review",
            "commit-a",
            true,
            &[],
        )
        .expect("no pending and no current findings should never refuse");
    }

    /// The regression end to end: simplify reports MED on commit A
    /// (`--result issues`, persisted), fixes it and commits B, then requests
    /// `--result clean` with no `--findings-json` on B -- reconcile resolves
    /// the commit-A finding via the real git fixture, and the second call is
    /// allowed. Mirrors the scenario named in review.
    #[test]
    fn reconcile_and_refuse_issues_then_fix_then_clean_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        // Minimal isolated git fixture -- mirrors finding_gate's own test
        // helper (duplicated here for the same reason that file documents:
        // a `#[cfg(test)]`-private helper in a sibling module is not
        // importable across module boundaries without a shared export this
        // one extra call site does not justify).
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .current_dir(dir.path())
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .args(
                    [
                        "-c",
                        "user.name=Legion Test Fixture",
                        "-c",
                        "user.email=legion-test-fixture@example.invalid",
                        "-c",
                        "commit.gpgsign=false",
                    ]
                    .iter()
                    .chain(args.iter()),
                )
                .output()
                .unwrap()
        };
        git(&["init", "-q"]);
        std::fs::write(dir.path().join("foo.rs"), "fn foo() {}\n").unwrap();
        git(&["add", "foo.rs"]);
        git(&["commit", "-q", "-m", "initial"]);
        let commit_a = String::from_utf8_lossy(&git(&["rev-parse", "HEAD"]).stdout)
            .trim()
            .to_string();

        let db = test_db();
        db.insert_finding(&NewFindingInput {
            gate_id: "gate-a",
            repo: "legion",
            branch: "feat/x",
            skill: "legion-simplify",
            origin_commit: &commit_a,
            file: "foo.rs",
            line: Some(1),
            severity: FindingSeverity::Med,
            summary: "duplicate logic",
        })
        .unwrap();

        std::fs::write(dir.path().join("foo.rs"), "fn foo() { /* fixed */ }\n").unwrap();
        git(&["add", "foo.rs"]);
        git(&["commit", "-q", "-m", "fix foo"]);
        let commit_b = String::from_utf8_lossy(&git(&["rev-parse", "HEAD"]).stdout)
            .trim()
            .to_string();

        // reconcile_pending_findings shells to plain `git log` against the
        // process cwd; point it at the fixture explicitly instead of
        // mutating the real process cwd (a parallel-test hazard).
        finding_gate::reconcile_pending_findings(
            &db,
            Some(dir.path()),
            "legion",
            "feat/x",
            "legion-simplify",
            &commit_b,
        )
        .unwrap();

        // Now the CLI-level helper (process-cwd git) sees an already-resolved
        // finding and allows clean with no current findings.
        reconcile_and_refuse_if_findings_pending(
            &db,
            "legion",
            "feat/x",
            "legion-simplify",
            &commit_b,
            true,
            &[],
        )
        .expect("the commit-A finding should already be resolved by the fixture reconcile above");
    }

    /// `persist_raw_findings` deduplicates: a re-run reporting the exact
    /// same still-open finding (identical file+severity+summary) does not
    /// pile up a second PENDING row.
    #[test]
    fn persist_raw_findings_dedupes_identical_pending_finding_across_reruns() {
        let db = test_db();
        let gate = db
            .record_quality_gate(&QualityGateInput {
                branch: "feat/x",
                commit_hash: "commit-a",
                skill: "legion-review",
                result: GateResult::Issues,
                findings_count: 1,
                details: None,
                provenance: GateProvenance::Asserted,
                base: None,
            })
            .unwrap();
        let raw = vec![finding_gate::RawFinding {
            file: "src/foo.rs".to_string(),
            line: Some(12),
            severity: "MED".to_string(),
            summary: "unchecked input".to_string(),
        }];

        persist_raw_findings(&db, "legion", &gate, &raw);
        persist_raw_findings(&db, "legion", &gate, &raw);

        let pending = db
            .list_pending_findings("legion", "feat/x", "legion-review")
            .unwrap();
        assert_eq!(
            pending.len(),
            1,
            "re-persisting an identical finding must not create a duplicate row"
        );
    }

    /// A genuinely different finding (different summary) on the same file
    /// is NOT deduplicated away -- only an exact file+severity+summary match
    /// is treated as "the same still-open finding".
    #[test]
    fn persist_raw_findings_does_not_dedupe_distinct_findings_on_the_same_file() {
        let db = test_db();
        let gate = db
            .record_quality_gate(&QualityGateInput {
                branch: "feat/x",
                commit_hash: "commit-a",
                skill: "legion-review",
                result: GateResult::Issues,
                findings_count: 2,
                details: None,
                provenance: GateProvenance::Asserted,
                base: None,
            })
            .unwrap();
        let raw = vec![
            finding_gate::RawFinding {
                file: "src/foo.rs".to_string(),
                line: Some(12),
                severity: "MED".to_string(),
                summary: "unchecked input".to_string(),
            },
            finding_gate::RawFinding {
                file: "src/foo.rs".to_string(),
                line: Some(40),
                severity: "MED".to_string(),
                summary: "duplicate WHERE-clause construction".to_string(),
            },
        ];

        persist_raw_findings(&db, "legion", &gate, &raw);

        let pending = db
            .list_pending_findings("legion", "feat/x", "legion-review")
            .unwrap();
        assert_eq!(pending.len(), 2);
    }

    /// The dedup guard also catches a duplicate WITHIN a single batch, not
    /// only across separate calls -- a malformed `--findings-json` payload
    /// listing the identical triple twice must still land exactly one row.
    #[test]
    fn persist_raw_findings_dedupes_within_the_same_batch() {
        let db = test_db();
        let gate = db
            .record_quality_gate(&QualityGateInput {
                branch: "feat/x",
                commit_hash: "commit-a",
                skill: "legion-review",
                result: GateResult::Issues,
                findings_count: 2,
                details: None,
                provenance: GateProvenance::Asserted,
                base: None,
            })
            .unwrap();
        let duplicate = finding_gate::RawFinding {
            file: "src/foo.rs".to_string(),
            line: Some(12),
            severity: "MED".to_string(),
            summary: "unchecked input".to_string(),
        };
        let raw = vec![duplicate.clone(), duplicate];

        persist_raw_findings(&db, "legion", &gate, &raw);

        let pending = db
            .list_pending_findings("legion", "feat/x", "legion-review")
            .unwrap();
        assert_eq!(
            pending.len(),
            1,
            "a duplicated entry within one batch must not double-insert"
        );
    }

    /// Regression guard (pre-push review HIGH): a finding explicitly
    /// dispositioned ("won't fix: intentional") must STAY dispositioned when
    /// a later run reports the identical (file, severity, summary) again --
    /// dedup must check DISPOSITIONED rows too, not only PENDING ones, or a
    /// waiver silently resurrects as a fresh blocking PENDING row.
    #[test]
    fn persist_raw_findings_does_not_resurrect_a_dispositioned_finding() {
        let db = test_db();
        let gate = db
            .record_quality_gate(&QualityGateInput {
                branch: "feat/x",
                commit_hash: "commit-a",
                skill: "legion-review",
                result: GateResult::Issues,
                findings_count: 1,
                details: None,
                provenance: GateProvenance::Asserted,
                base: None,
            })
            .unwrap();
        let raw = vec![finding_gate::RawFinding {
            file: "src/foo.rs".to_string(),
            line: Some(12),
            severity: "LOW".to_string(),
            summary: "naming nit".to_string(),
        }];
        persist_raw_findings(&db, "legion", &gate, &raw);
        let pending = db
            .list_pending_findings("legion", "feat/x", "legion-review")
            .unwrap();
        assert_eq!(pending.len(), 1);
        db.dispose_finding(&pending[0].id, "won't fix: intentional")
            .unwrap();

        // A later run re-reports the exact same finding (e.g. an honest
        // reviewer re-listing the same LOW they already agreed to waive).
        persist_raw_findings(&db, "legion", &gate, &raw);

        let still_pending = db
            .list_pending_findings("legion", "feat/x", "legion-review")
            .unwrap();
        assert!(
            still_pending.is_empty(),
            "re-reporting an already-dispositioned finding must not resurrect it as PENDING, \
             got {still_pending:?}"
        );
        let all = db
            .list_findings(&FindingFilter {
                repo: None,
                branch: Some("feat/x".to_string()),
                skill: Some("legion-review".to_string()),
                status: None,
            })
            .unwrap();
        assert_eq!(
            all.len(),
            1,
            "the disposition must still be the only row for this finding"
        );
        assert_eq!(all[0].status, FindingStatus::Dispositioned);
    }

    /// A finding that recurs identically AFTER being RESOLVED (fix landed,
    /// then the same problem reappears) is treated as a fresh finding, not
    /// deduped away -- a genuine regression deserves its own PENDING row,
    /// unlike the dispositioned case above.
    #[test]
    fn persist_raw_findings_does_not_dedupe_against_a_resolved_finding() {
        let db = test_db();
        let gate = db
            .record_quality_gate(&QualityGateInput {
                branch: "feat/x",
                commit_hash: "commit-a",
                skill: "legion-simplify",
                result: GateResult::Issues,
                findings_count: 1,
                details: None,
                provenance: GateProvenance::Validated,
                base: None,
            })
            .unwrap();
        let raw = vec![finding_gate::RawFinding {
            file: "src/foo.rs".to_string(),
            line: Some(12),
            severity: "MED".to_string(),
            summary: "duplicate WHERE-clause construction".to_string(),
        }];
        persist_raw_findings(&db, "legion", &gate, &raw);
        let pending = db
            .list_pending_findings("legion", "feat/x", "legion-simplify")
            .unwrap();
        assert_eq!(pending.len(), 1);
        db.mark_finding_resolved(&pending[0].id, "commit-fix")
            .unwrap();

        // The identical problem recurs -- a regression, not the same
        // still-open finding.
        persist_raw_findings(&db, "legion", &gate, &raw);

        let now_pending = db
            .list_pending_findings("legion", "feat/x", "legion-simplify")
            .unwrap();
        assert_eq!(
            now_pending.len(),
            1,
            "a recurrence after RESOLVED must be tracked as a new PENDING finding"
        );
    }

    /// `persist_raw_findings` inserts one row per raw finding, tied to the
    /// gate row's id/branch/skill/commit, with parsed severity.
    #[test]
    fn persist_raw_findings_inserts_rows_tied_to_gate() {
        let db = test_db();
        let gate = db
            .record_quality_gate(&QualityGateInput {
                branch: "feat/x",
                commit_hash: "commit-a",
                skill: "legion-review",
                result: GateResult::Issues,
                findings_count: 1,
                details: None,
                provenance: GateProvenance::Asserted,
                base: None,
            })
            .unwrap();
        let raw = vec![finding_gate::RawFinding {
            file: "src/foo.rs".to_string(),
            line: Some(12),
            severity: "HIGH".to_string(),
            summary: "unchecked input".to_string(),
        }];

        persist_raw_findings(&db, "legion", &gate, &raw);

        let pending = db
            .list_pending_findings("legion", "feat/x", "legion-review")
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].gate_id, gate.id);
        assert_eq!(pending[0].origin_commit, "commit-a");
        assert_eq!(pending[0].severity, FindingSeverity::High);
        assert_eq!(pending[0].file, "src/foo.rs");
    }

    /// An unparseable severity string is treated as MED (fail closed), not
    /// dropped -- dropping a structured finding here would reopen the
    /// evaporation hole this ledger closes.
    #[test]
    fn persist_raw_findings_treats_unknown_severity_as_med() {
        let db = test_db();
        let gate = db
            .record_quality_gate(&QualityGateInput {
                branch: "feat/x",
                commit_hash: "commit-a",
                skill: "legion-review",
                result: GateResult::Issues,
                findings_count: 1,
                details: None,
                provenance: GateProvenance::Asserted,
                base: None,
            })
            .unwrap();
        let raw = vec![finding_gate::RawFinding {
            file: "src/foo.rs".to_string(),
            line: None,
            severity: "URGENT".to_string(),
            summary: "weird severity string".to_string(),
        }];

        persist_raw_findings(&db, "legion", &gate, &raw);

        let pending = db
            .list_pending_findings("legion", "feat/x", "legion-review")
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].severity, FindingSeverity::Med);
    }

    // --- #1403: repo keying ------------------------------------------------

    /// A legacy NULL-repo PENDING MED on branch `HEAD` (the shape of
    /// `019fb06d`) does not block a clean gate on a named branch: once `HEAD`
    /// can no longer be recorded under, those rows stop matching anything.
    #[test]
    fn reconcile_and_refuse_ignores_legacy_head_row_on_a_named_branch() {
        let db = test_db();
        db.conn
            .execute(
                "INSERT INTO quality_gate_findings \
                 (id, gate_id, branch, skill, origin_commit, file, line, severity, summary, \
                  status, created_at, updated_at) \
                 VALUES ('019fb06d-850a-7742-8220-289f5c8cf39d', 'gate-legacy', 'HEAD', \
                  'legion-review', 'commit-a', 'PR body', NULL, 'MED', 'legacy', 'pending', \
                  '2026-07-30T00:00:00Z', '2026-07-30T00:00:00Z')",
                [],
            )
            .unwrap();
        reconcile_and_refuse_if_findings_pending(
            &db,
            "smugglr",
            "feat/x",
            "legion-review",
            "commit-b",
            true,
            &[],
        )
        .expect("a legacy HEAD row must not block a clean gate on feat/x");
    }

    /// The dedup lookup is scoped to the gate's repo: an open smugglr
    /// finding with the same file, severity and summary does not stop a
    /// legion finding from being inserted.
    #[test]
    fn persist_raw_findings_dedup_is_scoped_to_repo() {
        let db = test_db();
        let gate = db
            .record_quality_gate(&QualityGateInput {
                branch: "feat/x",
                commit_hash: "commit-a",
                skill: "legion-review",
                result: GateResult::Issues,
                findings_count: 1,
                details: None,
                provenance: GateProvenance::Asserted,
                base: None,
            })
            .unwrap();
        let raw = vec![finding_gate::RawFinding {
            file: "src/foo.rs".to_string(),
            line: Some(12),
            severity: "MED".to_string(),
            summary: "unchecked input".to_string(),
        }];

        persist_raw_findings(&db, "smugglr", &gate, &raw);
        persist_raw_findings(&db, "legion", &gate, &raw);

        let legion = db
            .list_pending_findings("legion", "feat/x", "legion-review")
            .unwrap();
        assert_eq!(legion.len(), 1);
        assert_eq!(legion[0].repo.as_deref(), Some("legion"));
        let smugglr = db
            .list_pending_findings("smugglr", "feat/x", "legion-review")
            .unwrap();
        assert_eq!(smugglr.len(), 1);
        assert_eq!(smugglr[0].repo.as_deref(), Some("smugglr"));
    }

    #[test]
    fn refuse_detached_head_refuses_only_the_literal_head() {
        assert!(matches!(
            refuse_detached_head("HEAD"),
            Err(error::LegionError::ExitWith(1))
        ));
        refuse_detached_head("main").unwrap();
    }

    // --- #1402: verify --help documents both verdict shapes ---------------

    #[test]
    fn verify_help_documents_both_verdict_shapes() {
        use clap::CommandFactory;

        let mut cli = crate::cli::Cli::command();
        let help = cli
            .find_subcommand_mut("verify")
            .expect("verify subcommand")
            .render_long_help()
            .to_string();

        let issue = serde_json::to_value(verify::AcResult {
            criterion: "c".into(),
            verdict: verify::AcVerdict::Pass,
            evidence: "e".into(),
        })
        .unwrap();
        let artifact = verify::VerdictArtifact {
            kind: verify::ArtifactKind::Test,
            reference: "r".into(),
            outcome: "o".into(),
        };
        let artifact_value = serde_json::to_value(&artifact).unwrap();
        let spec = serde_json::to_value(verify::SpecAcResult {
            spec_doc_id: "d".into(),
            spec_revision: 1,
            criterion_id: "i".into(),
            verdict: verify::AcVerdict::Pass,
            artifacts: vec![artifact],
        })
        .unwrap();

        for value in [&issue, &spec, &artifact_value] {
            for key in value
                .as_object()
                .expect("verdict serializes as an object")
                .keys()
            {
                let quoted = format!("\"{key}\"");
                assert!(
                    help.contains(&quoted),
                    "verify --help is missing {quoted}:\n{help}"
                );
            }
        }
        assert!(
            help.contains("test|diff|command"),
            "verify --help is missing the artifact kinds token:\n{help}"
        );
    }
}
