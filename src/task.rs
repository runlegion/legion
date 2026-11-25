use crate::db::Database;
use crate::error::Result;

/// A single task delegated between agents.
#[derive(Debug, Clone, serde::Serialize)]
#[allow(dead_code)]
pub struct Task {
    pub id: String,
    pub from_repo: String,
    pub to_repo: String,
    pub text: String,
    pub context: Option<String>,
    pub priority: String,
    pub status: String,
    pub note: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Map a database row to a Task struct.
pub fn map_task_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
    Ok(Task {
        id: row.get(0)?,
        from_repo: row.get(1)?,
        to_repo: row.get(2)?,
        text: row.get(3)?,
        context: row.get(4)?,
        priority: row.get(5)?,
        status: row.get(6)?,
        note: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

/// Format a priority tag for display. Returns " [high]" or " [low]",
/// or an empty string for the default "med" priority.
fn priority_tag(priority: &str) -> String {
    if priority != "med" {
        format!(" [{}]", priority)
    } else {
        String::new()
    }
}

/// Get pending inbound tasks for a repo (used by surface). `range` applies
/// #786's `created_at` predicate (`TimeRange::default()` is unbounded, a
/// no-op).
pub fn get_pending_inbound(
    db: &Database,
    repo: &str,
    range: &crate::timerange::TimeRange,
) -> Result<Vec<Task>> {
    db.get_pending_tasks_for_repo(repo, range)
}

/// Count pending inbound tasks for a repo (used by bullpen --count).
pub fn count_pending_inbound(db: &Database, repo: &str) -> Result<u64> {
    db.count_pending_tasks_for_repo(repo)
}

/// Format pending tasks for surface output.
pub fn format_pending_for_surface(tasks: &[Task]) -> String {
    let mut output = String::new();
    for t in tasks {
        let prio = priority_tag(&t.priority);
        let context_part = t
            .context
            .as_deref()
            .map(|c| {
                let truncated: String = c.chars().take(60).collect();
                let ellipsis = if c.chars().count() > 60 { "..." } else { "" };
                format!(" (context: {}{})", truncated, ellipsis)
            })
            .unwrap_or_default();
        output.push_str(&format!(
            "- Task from {}: \"{}\"{}{}\n",
            t.from_repo, t.text, prio, context_part
        ));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::test_storage;

    #[test]
    fn pending_inbound_for_surface() {
        let (db, _index, _dir) = test_storage();
        db.insert_task("kelex", "legion", "pending one", None, "med")
            .expect("create");
        let id2 = db
            .insert_task("rafters", "legion", "accepted one", None, "med")
            .expect("create");
        db.update_task_status(&id2, "accepted", None)
            .expect("accept");

        let pending = get_pending_inbound(&db, "legion", &crate::timerange::TimeRange::default())
            .expect("pending");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].text, "pending one");
    }

    #[test]
    fn count_and_get_pending_are_consistent() {
        let (db, _index, _dir) = test_storage();
        db.insert_task("kelex", "legion", "task one", None, "med")
            .expect("create");
        db.insert_task("kelex", "legion", "task two", None, "high")
            .expect("create");

        let count = count_pending_inbound(&db, "legion").expect("count");
        let tasks = get_pending_inbound(&db, "legion", &crate::timerange::TimeRange::default())
            .expect("get");
        assert_eq!(
            count,
            tasks.len() as u64,
            "count_pending and get_pending must agree"
        );
    }

    #[test]
    fn format_pending_for_surface_output() {
        let (db, _index, _dir) = test_storage();
        db.insert_task(
            "kelex",
            "legion",
            "surface task",
            Some("context info"),
            "high",
        )
        .expect("create");

        let pending = get_pending_inbound(&db, "legion", &crate::timerange::TimeRange::default())
            .expect("pending");
        let output = format_pending_for_surface(&pending);
        assert!(output.contains("Task from kelex"));
        assert!(output.contains("surface task"));
        assert!(output.contains("[high]"));
        assert!(output.contains("context: context info"));
    }
}
