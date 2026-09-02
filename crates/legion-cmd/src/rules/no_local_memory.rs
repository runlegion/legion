//! The `no-local-memory` rule module (FR-CMD-005): blocks writes into the
//! Claude Code auto-memory directory.
//!
//! Ported from `plugin/hooks/no-local-memory.sh`, registered there under
//! `PreToolUse` for exactly `Write`, `Edit`, `MultiEdit`. That script's
//! posture is a genuine block, not an advisory nudge -- it emits `emit_deny`
//! unconditionally on a match, documents "Bypass: none", and offers no
//! Allow-with-note path. FR-CMD-005 rev 6's warning against turning a nudge
//! into a block (and its converse: never softening a real block into
//! Allow-with-note, which would drop enforcement) does not apply here as a
//! correction -- it applies as confirmation that `Deny` is the right shape
//! for this module.
//!
//! This is a path-only predicate. Like the shell script, it never reads
//! `content`, `new_string`, or `edits` -- only `file_path`.

use crate::call::ToolCall;
use crate::ctx::Ctx;
use crate::decision::Decision;

/// The script's exact deny message, ported verbatim (FR-CMD-016).
const DENY_MESSAGE: &str = "Blocked: this file path is in the Claude Code auto-memory \
directory. Legion is the memory layer for this project -- use `legion reflect --repo <name> \
--text '...'` instead. Reflections stored via legion are searchable across sessions, repos, \
and agents via `legion recall` and `legion consult`; files in ~/.claude/projects/*/memory/ \
are invisible outside this single session/agent. If the content is project-wide guidance \
(not a personal reflection), it belongs in CLAUDE.md, not auto-memory.";

/// Decide a `Write`/`Edit`/`MultiEdit` call against the auto-memory path.
///
/// Any other `ToolCall` variant is a no-op pass-through to `Allow` -- this
/// module owns exactly the three tools its script matched (FR-CMD-005) and
/// never denies a call it does not own.
pub fn decide(call: &ToolCall, _ctx: &Ctx) -> Decision {
    let file_path: &str = match call {
        ToolCall::Write { file_path, .. } => file_path,
        ToolCall::Edit { file_path, .. } => file_path,
        ToolCall::MultiEdit { file_path, .. } => file_path,
        _ => return Decision::Allow,
    };

    if file_path.is_empty() {
        return Decision::Allow;
    }

    if is_auto_memory_path(file_path) {
        return Decision::Deny(DENY_MESSAGE.to_owned());
    }

    Decision::Allow
}

/// Port of the script's `grep -qE '\.claude/projects/.*/memory/'`.
///
/// Checks every occurrence of `.claude/projects/` (not just the first) for a
/// later `/memory/` substring, matching the regex's own backtracking rather
/// than assuming the first occurrence is the only one that could match. No
/// path normalization beyond that -- the script never resolved symlinks or
/// special-cased a leading absolute-path form, and this predicate does not
/// invent behavior the script never had (FR-CMD-016).
fn is_auto_memory_path(file_path: &str) -> bool {
    file_path
        .match_indices(".claude/projects/")
        .any(|(start, matched)| {
            let after = start + matched.len();
            file_path[after..].contains("/memory/")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(file_path: &str) -> ToolCall {
        ToolCall::Write {
            file_path: file_path.into(),
            content: "irrelevant".into(),
        }
    }

    fn edit(file_path: &str) -> ToolCall {
        ToolCall::Edit {
            file_path: file_path.into(),
            new_string: "irrelevant".into(),
        }
    }

    fn multi_edit(file_path: &str) -> ToolCall {
        ToolCall::MultiEdit {
            file_path: file_path.into(),
            edits: vec!["irrelevant".into()],
        }
    }

    const MEMORY_PATH: &str = "/Users/x/.claude/projects/legion/memory/notes.md";

    #[test]
    fn write_to_the_auto_memory_directory_is_denied() {
        match decide(&write(MEMORY_PATH), &Ctx::default()) {
            Decision::Deny(reason) => {
                assert!(reason.contains("legion reflect"), "{reason}");
                assert!(reason.contains("legion recall"), "{reason}");
                assert!(reason.contains("legion consult"), "{reason}");
                assert!(reason.contains("CLAUDE.md"), "{reason}");
            }
            other => panic!("expected Deny, got {other:?}"),
        }
    }

    #[test]
    fn edit_to_the_auto_memory_directory_is_denied() {
        assert_eq!(
            decide(&edit(MEMORY_PATH), &Ctx::default()),
            Decision::Deny(DENY_MESSAGE.to_owned())
        );
    }

    #[test]
    fn multi_edit_to_the_auto_memory_directory_is_denied() {
        assert_eq!(
            decide(&multi_edit(MEMORY_PATH), &Ctx::default()),
            Decision::Deny(DENY_MESSAGE.to_owned())
        );
    }

    #[test]
    fn a_path_with_claude_projects_but_no_memory_segment_is_allowed() {
        let path = "/Users/x/.claude/projects/legion/notes/todo.md";
        assert_eq!(decide(&write(path), &Ctx::default()), Decision::Allow);
    }

    #[test]
    fn a_path_with_a_memory_segment_but_no_claude_projects_prefix_is_allowed() {
        let path = "/Users/x/work/legion/memory/notes.md";
        assert_eq!(decide(&write(path), &Ctx::default()), Decision::Allow);
    }

    #[test]
    fn a_memory_segment_nested_more_than_one_directory_deep_is_denied() {
        let path = "/Users/x/.claude/projects/legion/agents/sub/deep/memory/notes.md";
        match decide(&write(path), &Ctx::default()) {
            Decision::Deny(_) => {}
            other => panic!("expected Deny (the script's pattern uses .*), got {other:?}"),
        }
    }

    #[test]
    fn an_empty_file_path_is_allowed() {
        assert_eq!(decide(&write(""), &Ctx::default()), Decision::Allow);
        assert_eq!(decide(&edit(""), &Ctx::default()), Decision::Allow);
        assert_eq!(decide(&multi_edit(""), &Ctx::default()), Decision::Allow);
    }

    #[test]
    fn a_tool_this_module_does_not_own_is_always_allowed() {
        assert_eq!(
            decide(
                &ToolCall::Bash {
                    command: format!("cat {MEMORY_PATH}"),
                },
                &Ctx::default()
            ),
            Decision::Allow
        );
        assert_eq!(
            decide(
                &ToolCall::Read {
                    file_path: MEMORY_PATH.into(),
                },
                &Ctx::default()
            ),
            Decision::Allow,
            "reading auto-memory files is not this module's job -- the script never blocked reads"
        );
    }
}
