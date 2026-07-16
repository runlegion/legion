use crate::db::Database;
use crate::error::{LegionError, Result};

/// Direction filter for listing tasks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Direction {
    /// Tasks assigned TO this repo (default).
    Inbound,
    /// Tasks created BY this repo.
    Outbound,
}

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

/// Create a new task delegated from one repo to another.
///
/// Returns the generated task ID (UUIDv7).
pub fn create_task(
    db: &Database,
    from_repo: &str,
    to_repo: &str,
    text: &str,
    context: Option<&str>,
    priority: &str,
) -> Result<String> {
    db.insert_task(from_repo, to_repo, text, context, priority)
}

/// List tasks for a repo, filtered by direction.
pub fn list_tasks(db: &Database, repo: &str, direction: Direction) -> Result<Vec<Task>> {
    db.get_tasks(repo, direction)
}

/// Transition a task from an expected status to a new status.
///
/// Returns an error if the task does not exist or its current status
/// does not match `expected_status`.
fn transition_task(
    db: &Database,
    id: &str,
    action: &str,
    expected_status: &str,
    new_status: &str,
    note: Option<&str>,
) -> Result<()> {
    let task = db
        .get_task_by_id(id)?
        .ok_or_else(|| LegionError::TaskNotFound(id.to_string()))?;

    if task.status != expected_status {
        return Err(LegionError::InvalidTaskTransition {
            action: action.to_string(),
            current: task.status,
        });
    }

    db.update_task_status(id, new_status, note)
}

/// Accept a pending task (pending -> accepted).
pub fn accept_task(db: &Database, id: &str) -> Result<()> {
    transition_task(db, id, "accept", "pending", "accepted", None)
}

/// Complete an accepted task (accepted -> done), with optional note.
pub fn complete_task(db: &Database, id: &str, note: Option<&str>) -> Result<()> {
    transition_task(db, id, "complete", "accepted", "done", note)
}

/// Block an accepted task (accepted -> blocked), with reason.
pub fn block_task(db: &Database, id: &str, reason: Option<&str>) -> Result<()> {
    transition_task(db, id, "block", "accepted", "blocked", reason)
}

/// Unblock a blocked task (blocked -> accepted).
pub fn unblock_task(db: &Database, id: &str) -> Result<()> {
    transition_task(db, id, "unblock", "blocked", "accepted", None)
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

/// Format a task list for display.
pub fn format_task_list(tasks: &[Task], repo: &str, direction: Direction) -> String {
    if tasks.is_empty() {
        return String::new();
    }

    let label = match direction {
        Direction::Inbound => "inbound",
        Direction::Outbound => "outbound",
    };

    let mut output = format!(
        "[Legion] Tasks for {} ({}, {} total):\n",
        repo,
        label,
        tasks.len()
    );

    for t in tasks {
        let prio = priority_tag(&t.priority);
        let peer = match direction {
            Direction::Inbound => format!("from:{}", t.from_repo),
            Direction::Outbound => format!("to:{}", t.to_repo),
        };
        let note_part = t
            .note
            .as_deref()
            .map(|n| format!(" -- {}", n))
            .unwrap_or_default();
        let date = crate::db::format_date(&t.created_at);
        output.push_str(&format!(
            "- [{}] {}{} ({}, {}{}) {}\n",
            t.status, t.text, prio, peer, date, note_part, t.id
        ));
    }

    output
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
    fn create_and_list_inbound() {
        let (db, _index, _dir) = test_storage();

        let id = create_task(&db, "kelex", "legion", "implement search", None, "med")
            .expect("create task");
        assert!(!id.is_empty());
        // UUIDv7 check
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "7");

        let tasks = list_tasks(&db, "legion", Direction::Inbound).expect("list inbound");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].text, "implement search");
        assert_eq!(tasks[0].from_repo, "kelex");
        assert_eq!(tasks[0].to_repo, "legion");
        assert_eq!(tasks[0].status, "pending");
        assert_eq!(tasks[0].priority, "med");
    }

    #[test]
    fn list_outbound() {
        let (db, _index, _dir) = test_storage();
        create_task(&db, "kelex", "legion", "task 1", None, "med").expect("create");
        create_task(&db, "kelex", "rafters", "task 2", None, "high").expect("create");

        let tasks = list_tasks(&db, "kelex", Direction::Outbound).expect("list outbound");
        assert_eq!(tasks.len(), 2);
    }

    #[test]
    fn full_lifecycle_create_accept_done() {
        let (db, _index, _dir) = test_storage();
        let id = create_task(&db, "kelex", "legion", "do the thing", None, "med").expect("create");

        accept_task(&db, &id).expect("accept");
        let task = db.get_task_by_id(&id).expect("get").expect("exists");
        assert_eq!(task.status, "accepted");

        complete_task(&db, &id, Some("shipped")).expect("complete");
        let task = db.get_task_by_id(&id).expect("get").expect("exists");
        assert_eq!(task.status, "done");
        assert_eq!(task.note.as_deref(), Some("shipped"));
    }

    #[test]
    fn block_flow() {
        let (db, _index, _dir) = test_storage();
        let id = create_task(&db, "kelex", "legion", "blocked task", None, "med").expect("create");

        accept_task(&db, &id).expect("accept");
        block_task(&db, &id, Some("waiting on upstream")).expect("block");

        let task = db.get_task_by_id(&id).expect("get").expect("exists");
        assert_eq!(task.status, "blocked");
        assert_eq!(task.note.as_deref(), Some("waiting on upstream"));
    }

    #[test]
    fn cannot_complete_pending_task() {
        let (db, _index, _dir) = test_storage();
        let id = create_task(&db, "kelex", "legion", "premature", None, "med").expect("create");

        let err = complete_task(&db, &id, None).unwrap_err();
        assert!(matches!(err, LegionError::InvalidTaskTransition { .. }));
    }

    #[test]
    fn cannot_accept_done_task() {
        let (db, _index, _dir) = test_storage();
        let id = create_task(&db, "kelex", "legion", "finished", None, "med").expect("create");
        accept_task(&db, &id).expect("accept");
        complete_task(&db, &id, None).expect("complete");

        let err = accept_task(&db, &id).unwrap_err();
        assert!(matches!(err, LegionError::InvalidTaskTransition { .. }));
    }

    #[test]
    fn cannot_block_pending_task() {
        let (db, _index, _dir) = test_storage();
        let id =
            create_task(&db, "kelex", "legion", "not accepted yet", None, "med").expect("create");

        let err = block_task(&db, &id, Some("reason")).unwrap_err();
        assert!(matches!(err, LegionError::InvalidTaskTransition { .. }));
    }

    #[test]
    fn task_not_found() {
        let (db, _index, _dir) = test_storage();
        let err = accept_task(&db, "nonexistent-id").unwrap_err();
        assert!(matches!(err, LegionError::TaskNotFound(_)));
    }

    #[test]
    fn create_with_context_and_priority() {
        let (db, _index, _dir) = test_storage();
        let id = create_task(
            &db,
            "kelex",
            "legion",
            "urgent task",
            Some("related to issue #42"),
            "high",
        )
        .expect("create");

        let task = db.get_task_by_id(&id).expect("get").expect("exists");
        assert_eq!(task.context.as_deref(), Some("related to issue #42"));
        assert_eq!(task.priority, "high");
    }

    #[test]
    fn pending_inbound_for_surface() {
        let (db, _index, _dir) = test_storage();
        create_task(&db, "kelex", "legion", "pending one", None, "med").expect("create");
        let id2 =
            create_task(&db, "rafters", "legion", "accepted one", None, "med").expect("create");
        accept_task(&db, &id2).expect("accept");

        let pending = get_pending_inbound(&db, "legion", &crate::timerange::TimeRange::default())
            .expect("pending");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].text, "pending one");
    }

    #[test]
    fn format_task_list_empty() {
        let output = format_task_list(&[], "kelex", Direction::Inbound);
        assert!(output.is_empty());
    }

    #[test]
    fn format_task_list_shows_tasks() {
        let (db, _index, _dir) = test_storage();
        create_task(&db, "kelex", "legion", "test task", None, "high").expect("create");

        let tasks = list_tasks(&db, "legion", Direction::Inbound).expect("list");
        let output = format_task_list(&tasks, "legion", Direction::Inbound);
        assert!(output.contains("[Legion] Tasks for legion"));
        assert!(output.contains("inbound"));
        assert!(output.contains("test task"));
        assert!(output.contains("[high]"));
        assert!(output.contains("from:kelex"));
    }

    #[test]
    fn count_and_get_pending_are_consistent() {
        let (db, _index, _dir) = test_storage();
        create_task(&db, "kelex", "legion", "task one", None, "med").expect("create");
        create_task(&db, "kelex", "legion", "task two", None, "high").expect("create");

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
        create_task(
            &db,
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
