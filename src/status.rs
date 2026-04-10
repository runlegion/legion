use chrono::{DateTime, Utc};

use crate::db::{Database, Reflection};
use crate::error::Result;
use crate::signal;
use crate::task::Task;

/// A single item in a status section.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StatusItem {
    pub category: String,
    pub text: String,
    pub from: String,
    pub age: String,
}

/// Complete status output for an agent.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StatusOutput {
    pub repo: String,
    pub your_work: Vec<StatusItem>,
    pub team_needs: Vec<StatusItem>,
    pub what_changed: Vec<StatusItem>,
}

/// Hours to look back for team posts in status output.
const LOOKBACK_HOURS: i64 = 8;
/// Hours to look back for the focused needs query.
const NEEDS_LOOKBACK_HOURS: i64 = 24;
/// Maximum items per section in status output.
const MAX_NEEDS: usize = 10;
const MAX_CHANGED: usize = 10;
/// Maximum items in the focused needs query.
const MAX_NEEDS_FOCUSED: usize = 20;

/// Gather the full status for a repo.
pub fn get_status(db: &Database, repo: &str) -> Result<StatusOutput> {
    let your_work = get_your_work(db, repo)?;
    let posts: Vec<Reflection> = db.get_recent_board_posts(LOOKBACK_HOURS)?;
    let (team_needs, seen_ids) = get_team_needs(&posts, repo);
    let what_changed = get_what_changed(&posts, repo, &seen_ids);

    Ok(StatusOutput {
        repo: repo.to_string(),
        your_work,
        team_needs,
        what_changed,
    })
}

/// Result of a `legion done` operation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DoneResult {
    pub announcement: String,
    pub notified: Vec<String>,
}

/// Find agents who mentioned being blocked on this repo in recent bullpen posts.
pub fn find_blocked_agents(db: &Database, repo: &str) -> Result<Vec<String>> {
    let posts: Vec<Reflection> = db.get_recent_board_posts(NEEDS_LOOKBACK_HOURS)?;
    let repo_lower: String = repo.to_lowercase();
    let blocked_pattern: String = format!("blocked on {}", repo_lower);
    let waiting_pattern: String = format!("waiting on {}", repo_lower);
    let mut agents: Vec<String> = Vec::new();

    for p in &posts {
        if p.repo.to_lowercase() == repo_lower {
            continue;
        }
        let text_lower: String = p.text.to_lowercase();
        if (text_lower.contains(&blocked_pattern) || text_lower.contains(&waiting_pattern))
            && !agents.contains(&p.repo)
        {
            agents.push(p.repo.clone());
        }
    }

    Ok(agents)
}

/// Gather focused team needs for a repo (wider lookback, more items than status).
/// Used by `legion needs` when an agent is idle and looking for ways to help.
pub fn get_needs(db: &Database, repo: &str) -> Result<Vec<StatusItem>> {
    let posts: Vec<Reflection> = db.get_recent_board_posts(NEEDS_LOOKBACK_HOURS)?;
    let (items, _seen_ids) = get_team_needs_with_limit(&posts, repo, MAX_NEEDS_FOCUSED);
    Ok(items)
}

/// Format needs output for terminal display.
pub fn format_needs(repo: &str, items: &[StatusItem]) -> String {
    if items.is_empty() {
        return format!(
            "[Legion] No team needs for {repo}. Check `gh issue list` for your own backlog."
        );
    }

    let mut out = format!("[Legion] Team needs ({repo}):\n\n");
    for item in items {
        out.push_str(&format!(
            "  [{}] {}  (from: {}, {})\n",
            item.category, item.text, item.from, item.age
        ));
    }
    out
}

/// JSON-friendly summary with counts only. Used by hooks to inject minimal context.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StatusSummary {
    pub repo: String,
    pub tasks: usize,
    pub blocked: usize,
    pub team_needs: usize,
    pub what_changed: usize,
}

/// Produce a compact summary with counts only.
pub fn format_summary(output: &StatusOutput) -> StatusSummary {
    let blocked: usize = output
        .your_work
        .iter()
        .filter(|i| i.text.contains("[BLOCKED]"))
        .count();
    let tasks: usize = output
        .your_work
        .first()
        .and_then(|i| {
            if i.category == "TASKS" {
                i.text.split_whitespace().next()?.parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);

    StatusSummary {
        repo: output.repo.clone(),
        tasks,
        blocked,
        team_needs: output.team_needs.len(),
        what_changed: output.what_changed.len(),
    }
}

/// Format status output for terminal display.
pub fn format_status(output: &StatusOutput) -> String {
    if output.your_work.is_empty() && output.team_needs.is_empty() && output.what_changed.is_empty()
    {
        return String::new();
    }

    let mut out = format!("[Legion] Status for {}:\n", output.repo);
    format_section(&mut out, "YOUR WORK", &output.your_work);
    format_section(&mut out, "TEAM NEEDS YOU", &output.team_needs);
    format_section(&mut out, "WHAT CHANGED", &output.what_changed);
    out
}

fn format_section(out: &mut String, header: &str, items: &[StatusItem]) {
    if items.is_empty() {
        return;
    }
    out.push_str(&format!("\n{header}:\n"));
    for item in items {
        out.push_str(&format!(
            "  [{}] {}  (from: {}, {})\n",
            item.category, item.text, item.from, item.age
        ));
    }
}

/// YOUR WORK: summary count of active tasks assigned to this repo.
/// Shows blocked tasks individually (they need attention), everything else as a count.
fn get_your_work(db: &Database, repo: &str) -> Result<Vec<StatusItem>> {
    let tasks: Vec<Task> = db.get_active_tasks_for_repo(repo)?;
    let mut items: Vec<StatusItem> = Vec::new();

    if !tasks.is_empty() {
        items.push(StatusItem {
            category: "TASKS".to_string(),
            text: format!("{} tasks (`legion kanban list` for details)", tasks.len()),
            from: repo.to_string(),
            age: String::new(),
        });
    }

    for t in tasks.iter().filter(|t| t.status == "blocked") {
        items.push(StatusItem {
            category: format!("TASK:{}", t.priority),
            text: format!("{} [BLOCKED]", t.text),
            from: t.from_repo.clone(),
            age: relative_time(&t.created_at),
        });
    }

    Ok(items)
}

/// TEAM NEEDS YOU: recent posts with actionable requests directed at this repo.
/// Returns items and the set of post IDs included (for dedup in what_changed).
fn get_team_needs(posts: &[Reflection], repo: &str) -> (Vec<StatusItem>, Vec<String>) {
    get_team_needs_with_limit(posts, repo, MAX_NEEDS)
}

/// Shared implementation for team needs with configurable limit.
fn get_team_needs_with_limit(
    posts: &[Reflection],
    repo: &str,
    limit: usize,
) -> (Vec<StatusItem>, Vec<String>) {
    let repo_lower: String = repo.to_lowercase();
    let at_repo: String = format!("@{}", repo_lower);
    let mut items: Vec<StatusItem> = Vec::new();
    let mut seen_ids: Vec<String> = Vec::new();

    for p in posts {
        if items.len() >= limit {
            break;
        }

        if p.repo.to_lowercase() == repo_lower {
            continue;
        }

        let text_lower: String = p.text.to_lowercase();

        // Skip CLI command requests -- these are for legion-the-storage, not the agent
        if text_lower.contains("legion reflect")
            || text_lower.contains("legion boost")
            || text_lower.contains("legion consult")
        {
            continue;
        }

        // Signals directed at this repo (not @all) with actionable verbs
        if let Some(sig) = signal::parse_signal(&p.text)
            && sig.recipient.to_lowercase() == repo_lower
            && matches!(
                sig.verb.to_lowercase().as_str(),
                "review" | "question" | "request" | "blocker"
            )
        {
            items.push(StatusItem {
                category: categorize_signal(&sig.verb),
                text: truncate(&p.text, 120),
                from: p.repo.clone(),
                age: relative_time(&p.created_at),
            });
            seen_ids.push(p.id.clone());
            continue;
        }

        // Direct @mention (not just @all)
        if text_lower.contains(&at_repo) {
            items.push(StatusItem {
                category: categorize_post_text(&text_lower),
                text: truncate(&p.text, 120),
                from: p.repo.clone(),
                age: relative_time(&p.created_at),
            });
            seen_ids.push(p.id.clone());
            continue;
        }

        // @all posts: only PR review requests or blocker announcements
        if text_lower.contains("@all") && is_actionable_broadcast(&text_lower) {
            items.push(StatusItem {
                category: categorize_broadcast(&text_lower),
                text: truncate(&p.text, 120),
                from: p.repo.clone(),
                age: relative_time(&p.created_at),
            });
            seen_ids.push(p.id.clone());
        }
    }

    (items, seen_ids)
}

/// Check if an @all broadcast is actionable (PR review request or blocker).
fn is_actionable_broadcast(text: &str) -> bool {
    is_review_request(text) || text.contains("blocked") || text.contains("blocker")
}

/// Check if text is an explicit PR review request.
fn is_review_request(text: &str) -> bool {
    let has_pr: bool =
        text.contains("pr #") || text.contains("pr#") || text.contains("pull request");
    has_pr && text.contains("review")
}

/// Categorize an @all broadcast for display.
fn categorize_broadcast(text: &str) -> String {
    if is_review_request(text) {
        "REVIEW".to_string()
    } else {
        "BLOCKER".to_string()
    }
}

/// WHAT CHANGED: recent announcements and status updates, excluding items
/// already shown in team_needs.
fn get_what_changed(posts: &[Reflection], repo: &str, seen_ids: &[String]) -> Vec<StatusItem> {
    let repo_lower: String = repo.to_lowercase();
    let mut items: Vec<StatusItem> = Vec::new();

    for p in posts {
        if p.repo.to_lowercase() == repo_lower {
            continue;
        }

        if seen_ids.contains(&p.id) {
            continue;
        }

        let text_lower: String = p.text.to_lowercase();

        // Signals with announce/status verbs
        if let Some(sig) = signal::parse_signal(&p.text)
            && matches!(
                sig.verb.to_lowercase().as_str(),
                "announce" | "status" | "update"
            )
        {
            items.push(StatusItem {
                category: "UPDATE".to_string(),
                text: truncate(&p.text, 120),
                from: p.repo.clone(),
                age: relative_time(&p.created_at),
            });
            if items.len() >= MAX_CHANGED {
                break;
            }
            continue;
        }

        // Posts with update-like keywords
        if has_update_keyword(&text_lower) {
            items.push(StatusItem {
                category: "UPDATE".to_string(),
                text: truncate(&p.text, 120),
                from: p.repo.clone(),
                age: relative_time(&p.created_at),
            });
            if items.len() >= MAX_CHANGED {
                break;
            }
        }
    }

    items
}

/// Convert an ISO 8601 timestamp to a relative time string.
fn relative_time(iso_timestamp: &str) -> String {
    let parsed: std::result::Result<DateTime<Utc>, _> =
        DateTime::parse_from_rfc3339(iso_timestamp).map(|dt| dt.with_timezone(&Utc));

    let ts: DateTime<Utc> = match parsed {
        Ok(dt) => dt,
        Err(_) => return iso_timestamp.to_string(),
    };

    let now: DateTime<Utc> = Utc::now();
    let diff: chrono::TimeDelta = now.signed_duration_since(ts);

    let minutes: i64 = diff.num_minutes();
    if minutes < 1 {
        return "just now".to_string();
    }
    if minutes < 60 {
        return format!("{}m ago", minutes);
    }

    let hours: i64 = diff.num_hours();
    if hours < 24 {
        return format!("{}h ago", hours);
    }

    let days: i64 = diff.num_days();
    format!("{}d ago", days)
}

/// Categorize a signal verb into a display category.
fn categorize_signal(verb: &str) -> String {
    match verb.to_lowercase().as_str() {
        "review" => "REVIEW".to_string(),
        "question" => "QUESTION".to_string(),
        "request" => "REQUEST".to_string(),
        "blocker" => "BLOCKER".to_string(),
        _ => "SIGNAL".to_string(),
    }
}

/// Categorize a post based on text content keywords.
fn categorize_post_text(text_lower: &str) -> String {
    if text_lower.contains("review") || text_lower.contains("pr ") || text_lower.contains("pr#") {
        "REVIEW".to_string()
    } else if text_lower.contains("question") || text_lower.contains('?') {
        "QUESTION".to_string()
    } else if text_lower.contains("help") || text_lower.contains("blocked") {
        "REQUEST".to_string()
    } else {
        "MENTION".to_string()
    }
}

/// Check if text contains update/announcement keywords (tight filter).
fn has_update_keyword(text_lower: &str) -> bool {
    const KEYWORDS: &[&str] = &["shipped", "merged", "released", "deployed", "launched"];
    KEYWORDS.iter().any(|kw| text_lower.contains(kw))
}

/// Truncate text to max_chars, using first line only.
fn truncate(text: &str, max_chars: usize) -> String {
    let first_line: &str = text.lines().next().unwrap_or(text);
    if first_line.chars().count() <= max_chars {
        first_line.to_string()
    } else {
        let truncated: String = first_line.chars().take(max_chars).collect();
        format!("{}...", truncated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::test_storage;

    #[test]
    fn relative_time_just_now() {
        let now = Utc::now().to_rfc3339();
        let result = relative_time(&now);
        assert!(
            result == "just now" || result.ends_with("m ago"),
            "unexpected: {}",
            result
        );
    }

    #[test]
    fn relative_time_minutes_ago() {
        let past = (Utc::now() - chrono::Duration::minutes(15)).to_rfc3339();
        let result = relative_time(&past);
        assert!(result.contains("m ago"), "expected minutes ago: {}", result);
    }

    #[test]
    fn relative_time_hours_ago() {
        let past = (Utc::now() - chrono::Duration::hours(3)).to_rfc3339();
        let result = relative_time(&past);
        assert_eq!(result, "3h ago");
    }

    #[test]
    fn relative_time_days_ago() {
        let past = (Utc::now() - chrono::Duration::days(2)).to_rfc3339();
        let result = relative_time(&past);
        assert_eq!(result, "2d ago");
    }

    #[test]
    fn relative_time_invalid_falls_back() {
        let result = relative_time("not-a-timestamp");
        assert_eq!(result, "not-a-timestamp");
    }

    #[test]
    fn truncate_short_text() {
        assert_eq!(truncate("hello", 120), "hello");
    }

    #[test]
    fn truncate_long_text() {
        let long = "a".repeat(200);
        let result = truncate(&long, 120);
        assert!(result.ends_with("..."));
        // 120 chars + "..."
        assert_eq!(result.len(), 123);
    }

    #[test]
    fn truncate_multiline_uses_first_line() {
        let text = "first line\nsecond line\nthird line";
        assert_eq!(truncate(text, 120), "first line");
    }

    #[test]
    fn status_empty_database_returns_empty() {
        let (db, _index, _dir) = test_storage();
        let output = get_status(&db, "kelex").expect("get_status");
        assert!(output.your_work.is_empty());
        assert!(output.team_needs.is_empty());
        assert!(output.what_changed.is_empty());
    }

    #[test]
    fn format_status_empty_returns_empty_string() {
        let output = StatusOutput {
            repo: "kelex".to_string(),
            your_work: vec![],
            team_needs: vec![],
            what_changed: vec![],
        };
        assert!(format_status(&output).is_empty());
    }

    #[test]
    fn format_status_shows_sections() {
        let output = StatusOutput {
            repo: "kelex".to_string(),
            your_work: vec![StatusItem {
                category: "TASK:high".to_string(),
                text: "implement search".to_string(),
                from: "platform".to_string(),
                age: "3h ago".to_string(),
            }],
            team_needs: vec![StatusItem {
                category: "REVIEW".to_string(),
                text: "PR #36 needs review".to_string(),
                from: "mail".to_string(),
                age: "45m ago".to_string(),
            }],
            what_changed: vec![StatusItem {
                category: "UPDATE".to_string(),
                text: "shipped v1.0".to_string(),
                from: "eavesdrop".to_string(),
                age: "4h ago".to_string(),
            }],
        };
        let formatted = format_status(&output);
        assert!(formatted.contains("[Legion] Status for kelex:"));
        assert!(formatted.contains("YOUR WORK:"));
        assert!(formatted.contains("[TASK:high] implement search"));
        assert!(formatted.contains("TEAM NEEDS YOU:"));
        assert!(formatted.contains("[REVIEW] PR #36 needs review"));
        assert!(formatted.contains("WHAT CHANGED:"));
        assert!(formatted.contains("[UPDATE] shipped v1.0"));
    }

    #[test]
    fn your_work_shows_count_summary() {
        let (db, _index, _dir) = test_storage();
        crate::task::create_task(&db, "platform", "kelex", "build the thing", None, "high")
            .expect("create");
        crate::task::create_task(&db, "platform", "kelex", "another task", None, "med")
            .expect("create");

        let items = get_your_work(&db, "kelex").expect("your_work");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].category, "TASKS");
        assert!(items[0].text.contains("2 tasks"));
    }

    #[test]
    fn your_work_shows_blocked_plus_count() {
        let (db, _index, _dir) = test_storage();
        let id = crate::task::create_task(&db, "platform", "kelex", "stuck work", None, "high")
            .expect("create");
        crate::task::accept_task(&db, &id).expect("accept");
        crate::task::block_task(&db, &id, Some("waiting")).expect("block");
        crate::task::create_task(&db, "platform", "kelex", "other work", None, "med")
            .expect("create");

        let items = get_your_work(&db, "kelex").expect("your_work");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].category, "TASKS");
        assert!(items[0].text.contains("2 tasks"));
        assert_eq!(items[1].category, "TASK:high");
        assert!(items[1].text.contains("[BLOCKED]"));
    }

    #[test]
    fn your_work_shows_blocked_tasks() {
        let (db, _index, _dir) = test_storage();
        let id = crate::task::create_task(&db, "platform", "kelex", "blocked work", None, "med")
            .expect("create");
        crate::task::accept_task(&db, &id).expect("accept");
        crate::task::block_task(&db, &id, Some("waiting")).expect("block");

        let items = get_your_work(&db, "kelex").expect("your_work");
        // Count line + blocked line
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].category, "TASKS");
        assert!(items[1].text.contains("[BLOCKED]"));
    }

    #[test]
    fn your_work_excludes_done_tasks() {
        let (db, _index, _dir) = test_storage();
        let id = crate::task::create_task(&db, "platform", "kelex", "done work", None, "med")
            .expect("create");
        crate::task::accept_task(&db, &id).expect("accept");
        crate::task::complete_task(&db, &id, None).expect("complete");

        let items = get_your_work(&db, "kelex").expect("your_work");
        assert!(items.is_empty());
    }

    fn get_posts(db: &Database) -> Vec<Reflection> {
        db.get_recent_board_posts(24)
            .expect("get_recent_board_posts")
    }

    #[test]
    fn team_needs_picks_up_signals() {
        let (db, _index, _dir) = test_storage();
        db.insert_reflection("mail", "@kelex review:ready PR #36 needs your eyes", "team")
            .expect("insert");

        let (items, _ids) = get_team_needs(&get_posts(&db), "kelex");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].category, "REVIEW");
        assert_eq!(items[0].from, "mail");
    }

    #[test]
    fn team_needs_excludes_reflect_requests() {
        let (db, _index, _dir) = test_storage();
        // Realistic signal: "@legion reflect from eavesdrop -- legion reflect --repo eavesdrop"
        db.insert_reflection(
            "eavesdrop",
            "@legion reflect -- legion reflect --repo eavesdrop --text \"some reflection\"",
            "team",
        )
        .expect("insert");

        let (items, _ids) = get_team_needs(&get_posts(&db), "legion");
        assert!(items.is_empty(), "reflect requests should be filtered out");
    }

    #[test]
    fn team_needs_excludes_boost_requests() {
        let (db, _index, _dir) = test_storage();
        db.insert_reflection(
            "kelex",
            "@legion announce -- legion boost --id some-id",
            "team",
        )
        .expect("insert");

        let (items, _ids) = get_team_needs(&get_posts(&db), "legion");
        assert!(items.is_empty(), "boost requests should be filtered out");
    }

    #[test]
    fn team_needs_excludes_own_posts() {
        let (db, _index, _dir) = test_storage();
        db.insert_reflection("kelex", "@all review:ready something", "team")
            .expect("insert");

        let (items, _ids) = get_team_needs(&get_posts(&db), "kelex");
        assert!(items.is_empty());
    }

    #[test]
    fn what_changed_picks_up_announcements() {
        let (db, _index, _dir) = test_storage();
        db.insert_reflection("eavesdrop", "@all announce: shipped v1.0 pipeline", "team")
            .expect("insert");

        let items = get_what_changed(&get_posts(&db), "kelex", &[]);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].category, "UPDATE");
        assert_eq!(items[0].from, "eavesdrop");
    }

    #[test]
    fn what_changed_picks_up_keyword_posts() {
        let (db, _index, _dir) = test_storage();
        db.insert_reflection("mail", "mail agent shipped core package", "team")
            .expect("insert");

        let items = get_what_changed(&get_posts(&db), "kelex", &[]);
        assert_eq!(items.len(), 1);
        assert!(items[0].text.contains("shipped"));
    }

    #[test]
    fn what_changed_excludes_own_posts() {
        let (db, _index, _dir) = test_storage();
        db.insert_reflection("kelex", "kelex shipped something", "team")
            .expect("insert");

        let items = get_what_changed(&get_posts(&db), "kelex", &[]);
        assert!(items.is_empty());
    }

    #[test]
    fn categorize_post_text_detects_review() {
        assert_eq!(categorize_post_text("please review this pr"), "REVIEW");
        assert_eq!(categorize_post_text("pr #36 ready"), "REVIEW");
    }

    #[test]
    fn categorize_post_text_detects_question() {
        assert_eq!(categorize_post_text("how does this work?"), "QUESTION");
        assert_eq!(categorize_post_text("question about search"), "QUESTION");
    }

    #[test]
    fn categorize_post_text_detects_help_request() {
        assert_eq!(categorize_post_text("need help with embeddings"), "REQUEST");
        assert_eq!(
            categorize_post_text("we are blocked on upstream"),
            "REQUEST"
        );
    }

    #[test]
    fn categorize_post_text_fallback() {
        assert_eq!(categorize_post_text("hey just fyi"), "MENTION");
    }

    #[test]
    fn is_review_request_matches() {
        assert!(is_review_request("pr #36 needs review"));
        assert!(is_review_request("please review pr#42"));
        assert!(is_review_request("pull request ready for review"));
        assert!(!is_review_request("needs review please")); // no PR reference
        assert!(!is_review_request("pr #36 is ready")); // no review keyword
    }

    #[test]
    fn has_update_keyword_matches() {
        assert!(has_update_keyword("shipped v1.0"));
        assert!(has_update_keyword("pr merged into main"));
        assert!(!has_update_keyword("working on it"));
    }
}
