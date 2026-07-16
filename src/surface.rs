use crate::db::{self, Database, Reflection};
use crate::error::Result;
use crate::task;
use crate::timerange::TimeRange;

/// Truncate text to a maximum number of characters, adding "..." if truncated.
fn truncate_text(text: &str, max_chars: usize) -> String {
    // Take only the first line to avoid multi-line posts blowing up output
    let first_line = text.lines().next().unwrap_or(text);
    if first_line.len() <= max_chars {
        first_line.to_string()
    } else {
        let truncated: String = first_line.chars().take(max_chars).collect();
        format!("{}...", truncated)
    }
}

/// Result of a surface query, containing categorized highlights.
pub struct SurfaceResult {
    pub recent_posts: Vec<Reflection>,
    pub high_value: Vec<Reflection>,
    pub chain_extensions: Vec<Reflection>,
    pub pending_tasks: Vec<task::Task>,
}

const MAX_RECENT_POSTS: usize = 5;
const MAX_CHAIN_EXTENSIONS: usize = 3;

/// Gather cross-repo highlights for a given repo.
///
/// Returns up to 5 recent bullpen posts, 3 high-value cross-repo reflections,
/// and 3 recently extended learning chains. `range` (#786) threads
/// `--since`/`--until`/`--on` into all four underlying queries; unbounded
/// (`TimeRange::default()`) preserves the pre-#786 24h-lookback defaults
/// for the recency-windowed ones.
pub fn surface(db: &Database, repo: &str, range: &TimeRange) -> Result<SurfaceResult> {
    let mut recent_posts = db.get_recent_board_posts(24, range)?;
    recent_posts.truncate(MAX_RECENT_POSTS);
    let high_value = db.get_high_value_cross_repo(repo, 3, range)?;
    let mut chain_extensions = db.get_recent_chain_extensions(24, range)?;
    chain_extensions.truncate(MAX_CHAIN_EXTENSIONS);
    let pending_tasks = task::get_pending_inbound(db, repo, range)?;

    Ok(SurfaceResult {
        recent_posts,
        high_value,
        chain_extensions,
        pending_tasks,
    })
}

/// Format surface results for display.
///
/// Produces output like:
/// ```text
/// [Synapse] For kelex:
/// - [rafters] posted 2h ago: "insight" (domain: color-tokens)
/// - [courses] high-value: "pattern" (recalled 7x)
/// - Chain extended: kelex/color-tokens (latest: "refinement")
/// ```
///
/// Returns an empty string when there is nothing to surface.
pub fn format_surface(result: &SurfaceResult, repo: &str) -> String {
    if result.recent_posts.is_empty()
        && result.high_value.is_empty()
        && result.chain_extensions.is_empty()
        && result.pending_tasks.is_empty()
    {
        return String::new();
    }

    let mut output = format!("[Synapse] For {}:\n", repo);

    for p in &result.recent_posts {
        let date = db::format_date(&p.created_at);
        let domain_tag = p
            .domain
            .as_deref()
            .map(|d| format!(" (domain: {})", d))
            .unwrap_or_default();
        let preview = truncate_text(&p.text, 120);
        output.push_str(&format!(
            "- [{}] posted {}: \"{}\"{}\n",
            p.repo, date, preview, domain_tag
        ));
    }

    for r in &result.high_value {
        let domain_tag = r
            .domain
            .as_deref()
            .map(|d| format!(" [{}]", d))
            .unwrap_or_default();
        let preview = truncate_text(&r.text, 120);
        output.push_str(&format!(
            "- [{}] high-value{}: \"{}\" (recalled {}x)\n",
            r.repo, domain_tag, preview, r.recall_count
        ));
    }

    for c in &result.chain_extensions {
        let domain_tag = c
            .domain
            .as_deref()
            .map(|d| format!("/{}", d))
            .unwrap_or_default();
        let truncated: String = c.text.chars().take(60).collect();
        let ellipsis = if c.text.len() > 60 { "..." } else { "" };
        output.push_str(&format!(
            "- Chain extended: {}{} (latest: \"{}{}\")\n",
            c.repo, domain_tag, truncated, ellipsis
        ));
    }

    if !result.pending_tasks.is_empty() {
        output.push_str(&task::format_pending_for_surface(&result.pending_tasks));
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::ReflectionMeta;
    use crate::testutil::test_storage;

    #[test]
    fn surface_empty_database() {
        let (db, _index, _dir) = test_storage();
        let result = surface(&db, "kelex", &TimeRange::default()).unwrap();
        assert!(result.recent_posts.is_empty());
        assert!(result.high_value.is_empty());
        assert!(result.chain_extensions.is_empty());
    }

    #[test]
    fn surface_shows_recent_board_posts() {
        let (db, _index, _dir) = test_storage();
        db.insert_reflection("rafters", "fresh insight", "team")
            .unwrap();

        let result = surface(&db, "kelex", &TimeRange::default()).unwrap();
        assert_eq!(result.recent_posts.len(), 1);
        assert_eq!(result.recent_posts[0].text, "fresh insight");
    }

    #[test]
    fn surface_shows_high_value_cross_repo() {
        let (db, _index, _dir) = test_storage();
        let r = db
            .insert_reflection("rafters", "valuable pattern", "self")
            .unwrap();
        db.boost_reflection(&r.id).unwrap();
        db.boost_reflection(&r.id).unwrap();

        let result = surface(&db, "kelex", &TimeRange::default()).unwrap();
        assert_eq!(result.high_value.len(), 1);
        assert_eq!(result.high_value[0].text, "valuable pattern");
        assert_eq!(result.high_value[0].recall_count, 2);
    }

    #[test]
    fn surface_excludes_own_repo_from_high_value() {
        let (db, _index, _dir) = test_storage();
        let r = db
            .insert_reflection("kelex", "own valuable pattern", "self")
            .unwrap();
        db.boost_reflection(&r.id).unwrap();

        let result = surface(&db, "kelex", &TimeRange::default()).unwrap();
        assert!(result.high_value.is_empty());
    }

    #[test]
    fn surface_shows_chain_extensions() {
        let (db, _index, _dir) = test_storage();
        let parent = db
            .insert_reflection("kelex", "root insight", "self")
            .unwrap();
        let meta = ReflectionMeta {
            parent_id: Some(parent.id.clone()),
            ..Default::default()
        };
        db.insert_reflection_with_meta("kelex", "builds on root", "self", &meta)
            .unwrap();

        let result = surface(&db, "kelex", &TimeRange::default()).unwrap();
        assert_eq!(result.chain_extensions.len(), 1);
        assert_eq!(result.chain_extensions[0].text, "builds on root");
    }

    #[test]
    fn format_surface_empty_returns_empty() {
        let result = SurfaceResult {
            recent_posts: vec![],
            high_value: vec![],
            chain_extensions: vec![],
            pending_tasks: vec![],
        };
        let output = format_surface(&result, "kelex");
        assert!(output.is_empty());
    }

    #[test]
    fn surface_shows_pending_tasks() {
        let (db, _index, _dir) = test_storage();
        task::create_task(&db, "kelex", "legion", "implement search", None, "high")
            .expect("create task");

        let result = surface(&db, "legion", &TimeRange::default()).expect("surface");
        assert_eq!(result.pending_tasks.len(), 1);
        assert_eq!(result.pending_tasks[0].text, "implement search");
    }

    #[test]
    fn surface_format_includes_pending_tasks() {
        let (db, _index, _dir) = test_storage();
        task::create_task(
            &db,
            "kelex",
            "legion",
            "implement search",
            Some("BM25 index"),
            "high",
        )
        .expect("create task");

        let result = surface(&db, "legion", &TimeRange::default()).expect("surface");
        let output = format_surface(&result, "legion");
        assert!(output.contains("Task from kelex"));
        assert!(output.contains("implement search"));
        assert!(output.contains("[high]"));
    }

    #[test]
    fn format_surface_includes_header() {
        let (db, _index, _dir) = test_storage();
        db.insert_reflection("rafters", "test post", "team")
            .unwrap();

        let result = surface(&db, "kelex", &TimeRange::default()).unwrap();
        let output = format_surface(&result, "kelex");
        assert!(output.contains("[Synapse] For kelex:"));
        assert!(output.contains("[rafters]"));
        assert!(output.contains("test post"));
    }
}
