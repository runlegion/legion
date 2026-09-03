//! The `cmd-check` entry point (FR-CMD-008): builds a `ToolCall` from
//! `--tool`/`<input>`, runs it through [`Router::route`] unchanged, and
//! shapes the result into [`CmdCheckOutput`], the schema the CLI verb and,
//! later, the PreToolUse adapter (slice 9) both consume.
//!
//! This module owns NO decision logic. Everything it does is translate
//! arguments into the same `ToolCall` the adapter will build and format
//! `Routed` back out -- routing itself is entirely `Router::route`'s job.
//! A rule that lived only here would let the CLI verb and the future
//! adapter disagree about what a call decides, which is the exact failure
//! FR-CMD-008 exists to close off.

use serde::{Deserialize, Serialize};

use crate::call::{ParseError, Tool, ToolCall};
use crate::ctx::Ctx;
use crate::decision::{Decision, Routed, Targets};
use crate::router::Router;

/// Every `--tool` name `cmd-check` accepts, in the order named on refusal.
///
/// Deliberately the CLOSED set from FR-CMD-001's `Tool` enum, not
/// `Tool::Other`: an unrecognized `--tool` value is a CLI mistake, not a
/// harness sending a tool this crate has not been taught yet, so it gets a
/// refusal naming what IS accepted rather than silently falling through to
/// `Other`.
const ACCEPTED_TOOL_NAMES: &[&str] = &[
    "Bash",
    "Edit",
    "Write",
    "MultiEdit",
    "Read",
    "Grep",
    "Glob",
    "Agent",
    "Task",
    "WebFetch",
    "WebSearch",
    "AskUserQuestion",
];

/// A `cmd-check` invocation this module could not resolve to a `ToolCall`.
#[derive(Debug, thiserror::Error)]
pub enum CmdCheckError {
    #[error("unknown --tool '{tool}' -- accepted values: {accepted}")]
    UnknownTool { tool: String, accepted: String },

    /// `<input>` for a non-Bash `--tool` did not parse as JSON at all (a
    /// syntax failure, before `ToolCall::from_hook_json` ever sees it).
    #[error("input is not valid JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),

    /// `ToolCall::from_hook_json` refused the constructed envelope. Kept
    /// distinct from `InvalidJson` per the Error Handling section of
    /// FR-CMD-008: a caller reading the error should be able to tell a
    /// syntax problem in their own `<input>` from a structural one this
    /// crate's parser raised.
    #[error("{0}")]
    Parse(#[from] ParseError),
}

/// What `cmd-check` prints: the router's [`Decision`] plus [`Targets`], with
/// `reason`/`rewrite` lifted out for callers that want them without
/// matching the `decision` enum themselves.
///
/// `note` crosses the process boundary here or the advisory arm dies at this
/// exact seam: the router sets [`Routed::note`], and if this struct dropped
/// it, the PreToolUse adapter (slice 9) would have nothing to inject and
/// every inject-only guard would silently become a bare Allow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CmdCheckOutput {
    pub decision: Decision,
    pub reason: Option<String>,
    pub rewrite: Option<String>,
    pub note: Option<String>,
    pub targets: Targets,
}

impl From<Routed> for CmdCheckOutput {
    fn from(routed: Routed) -> Self {
        let (reason, rewrite) = match &routed.decision {
            Decision::Allow => (None, None),
            Decision::Rewrite {
                command, reason, ..
            } => (Some(reason.clone()), Some(command.clone())),
            Decision::Proxy(reason) | Decision::Deny(reason) | Decision::Ask(reason) => {
                (Some(reason.clone()), None)
            }
        };
        CmdCheckOutput {
            decision: routed.decision,
            reason,
            rewrite,
            note: routed.note,
            targets: routed.targets,
        }
    }
}

/// Parse `--tool` against the closed set, refusing anything else by name.
fn parse_tool_name(name: &str) -> Result<Tool, CmdCheckError> {
    if ACCEPTED_TOOL_NAMES.contains(&name) {
        Ok(Tool::from(name.to_owned()))
    } else {
        Err(CmdCheckError::UnknownTool {
            tool: name.to_owned(),
            accepted: ACCEPTED_TOOL_NAMES.join(", "),
        })
    }
}

/// Build the `ToolCall` `<input>` describes for `tool`.
///
/// Bash is special-cased: `<input>` IS the raw command string, not JSON, so
/// `ToolCall::Bash` is built directly. Every other tool goes through
/// `ToolCall::from_hook_json` via a constructed `{tool_name, tool_input}`
/// envelope -- the SAME constructor and the SAME per-tool field shapes
/// FR-CMD-001 defines, so a non-Bash call here can never diverge from how
/// the PreToolUse adapter will parse the identical harness payload.
fn build_tool_call(tool: &Tool, input: &str) -> Result<ToolCall, CmdCheckError> {
    if *tool == Tool::Bash {
        return Ok(ToolCall::Bash {
            command: input.to_owned(),
        });
    }
    let tool_input: serde_json::Value = serde_json::from_str(input)?;
    let envelope = serde_json::json!({
        "tool_name": tool.as_str(),
        "tool_input": tool_input,
    });
    Ok(ToolCall::from_hook_json(&envelope)?)
}

/// Route one call through `router` and shape the result (FR-CMD-008).
///
/// The only thing this function decides is which `ToolCall` variant
/// `tool_name`/`input` describe -- routing itself is entirely
/// `Router::route`'s job, so `cmd-check` and the PreToolUse adapter it will
/// back (slice 9) can never diverge on what a call decides.
pub fn cmd_check(
    router: &Router,
    ctx: &Ctx,
    tool_name: &str,
    input: &str,
) -> Result<CmdCheckOutput, CmdCheckError> {
    let tool = parse_tool_name(tool_name)?;
    let call = build_tool_call(&tool, input)?;
    let routed = router.route(&call, ctx);
    Ok(CmdCheckOutput::from(routed))
}

/// Render `output` the way `cmd-check` prints without `--json`: the
/// Decision's variant and reason, the note when one is present, then
/// Targets, one field per line.
pub fn format_plain(output: &CmdCheckOutput) -> String {
    let variant = match &output.decision {
        Decision::Allow => "Allow",
        Decision::Rewrite { .. } => "Rewrite",
        Decision::Proxy(_) => "Proxy",
        Decision::Deny(_) => "Deny",
        Decision::Ask(_) => "Ask",
    };

    let mut lines = vec![format!("decision: {variant}")];
    if let Some(reason) = &output.reason {
        lines.push(format!("reason: {reason}"));
    }
    if let Some(rewrite) = &output.rewrite {
        lines.push(format!("rewrite: {rewrite}"));
    }
    if let Some(note) = &output.note {
        lines.push(format!("note: {note}"));
    }
    lines.push(format!(
        "targets.paths: {}",
        output.targets.paths.join(", ")
    ));
    lines.push(format!(
        "targets.verb: {}",
        output.targets.verb.as_deref().unwrap_or("")
    ));
    lines.push(format!(
        "targets.issues: {}",
        join_u64(&output.targets.issues)
    ));
    lines.push(format!("targets.prs: {}", join_u64(&output.targets.prs)));
    lines.push(format!(
        "targets.repo: {}",
        output.targets.repo.as_deref().unwrap_or("")
    ));
    lines.push(format!(
        "targets.words: {}",
        output.targets.words.join(", ")
    ));

    let mut out = lines.join("\n");
    out.push('\n');
    out
}

fn join_u64(values: &[u64]) -> String {
    values
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::RouteTable;

    fn router() -> Router {
        Router::new(RouteTable::embedded().expect("embedded table parses")).expect("compile")
    }

    #[test]
    fn an_unknown_tool_name_is_refused_and_lists_accepted_names() {
        let err = parse_tool_name("Frobnicate").expect_err("must refuse");
        match err {
            CmdCheckError::UnknownTool { tool, accepted } => {
                assert_eq!(tool, "Frobnicate");
                assert!(accepted.contains("Bash"));
                assert!(accepted.contains("AskUserQuestion"));
            }
            other => panic!("expected UnknownTool, got {other:?}"),
        }
    }

    #[test]
    fn tool_other_is_not_an_accepted_cmd_check_tool_name() {
        // Tool::from is permissive (unknown -> Other) for the harness side;
        // cmd-check's own --tool parsing must NOT inherit that leniency.
        assert!(parse_tool_name("SomeFutureHarnessTool").is_err());
    }

    #[test]
    fn a_bash_command_routes_through_the_embedded_table_and_allows() {
        let r = router();
        let ctx = Ctx::default();
        let out = cmd_check(&r, &ctx, "Bash", "ls -la").expect("routes");
        assert_eq!(out.decision, Decision::Allow);
        assert_eq!(out.reason, None);
        assert_eq!(out.rewrite, None);
        assert_eq!(out.note, None);
    }

    #[test]
    fn tool_defaults_are_not_assumed_bash_input_is_never_parsed_as_json() {
        // A Bash command that happens to look like invalid JSON must still
        // route -- Bash input is never run through serde_json.
        let r = router();
        let ctx = Ctx::default();
        let out = cmd_check(&r, &ctx, "Bash", "echo {not json").expect("routes");
        assert_eq!(out.decision, Decision::Allow);
    }

    #[test]
    fn a_non_bash_tool_routes_using_the_shared_from_hook_json_constructor() {
        let r = router();
        let ctx = Ctx::default();
        let out = cmd_check(
            &r,
            &ctx,
            "Edit",
            r#"{"file_path": "a.rs", "new_string": "x"}"#,
        )
        .expect("routes");
        // The embedded table matches nothing yet (slice 1), so every call
        // Allows -- the property under test is that a well-formed non-Bash
        // JSON input reaches the router at all, not the decision itself.
        assert_eq!(out.decision, Decision::Allow);
    }

    #[test]
    fn malformed_json_for_a_non_bash_tool_is_an_error_not_a_panic() {
        let r = router();
        let ctx = Ctx::default();
        let err = cmd_check(&r, &ctx, "Edit", "{not valid json").expect_err("must error");
        assert!(matches!(err, CmdCheckError::InvalidJson(_)));
    }

    #[test]
    fn a_non_object_json_value_for_a_non_bash_tool_is_lenient_not_a_panic() {
        // "5" is valid JSON but not an object -- from_hook_json's field
        // helpers must degrade to empty fields rather than panicking.
        let r = router();
        let ctx = Ctx::default();
        let out = cmd_check(&r, &ctx, "Edit", "5").expect("does not panic, does not error");
        assert_eq!(out.decision, Decision::Allow);
    }

    #[test]
    fn a_note_carrying_routed_answer_round_trips_through_cmd_check_output() {
        // Router::route cannot yet produce a matched, note-carrying answer
        // (matching lands in slices 2/3) -- this exercises the formatting
        // seam directly, the same way router.rs's own note test does.
        let routed = Routed::from_route(Decision::Allow, Some("prefer legion recall".into()));
        let output = CmdCheckOutput::from(routed);
        assert_eq!(output.decision, Decision::Allow);
        assert_eq!(output.note.as_deref(), Some("prefer legion recall"));

        let json = serde_json::to_string(&output).expect("serializes");
        let back: CmdCheckOutput = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back, output);
        assert_eq!(back.note.as_deref(), Some("prefer legion recall"));

        let plain = format_plain(&output);
        assert!(plain.contains("note: prefer legion recall"));
        assert!(plain.contains("decision: Allow"));
    }

    #[test]
    fn a_deny_decision_carries_its_reason_into_the_flattened_field_and_plain_output() {
        let routed = Routed::deny("no sanctioned use");
        let output = CmdCheckOutput::from(routed);
        assert_eq!(output.reason.as_deref(), Some("no sanctioned use"));
        assert_eq!(output.note, None);

        let plain = format_plain(&output);
        assert!(plain.contains("decision: Deny"));
        assert!(plain.contains("reason: no sanctioned use"));
        assert!(!plain.contains("note:"));
    }

    #[test]
    fn a_rewrite_decision_carries_command_into_rewrite_and_plain_output() {
        let routed = Routed::from_route(
            Decision::Rewrite {
                command: "legion issue list".into(),
                reason: "work-source actions go through legion".into(),
                carry: vec![],
            },
            None,
        );
        let output = CmdCheckOutput::from(routed);
        assert_eq!(output.rewrite.as_deref(), Some("legion issue list"));
        assert_eq!(
            output.reason.as_deref(),
            Some("work-source actions go through legion")
        );

        let plain = format_plain(&output);
        assert!(plain.contains("decision: Rewrite"));
        assert!(plain.contains("rewrite: legion issue list"));
    }

    #[test]
    fn plain_output_prints_every_targets_field_even_when_empty() {
        let routed = Routed::allow();
        let output = CmdCheckOutput::from(routed);
        let plain = format_plain(&output);
        for field in [
            "targets.paths:",
            "targets.verb:",
            "targets.issues:",
            "targets.prs:",
            "targets.repo:",
            "targets.words:",
        ] {
            assert!(plain.contains(field), "missing {field} in:\n{plain}");
        }
    }

    #[test]
    fn json_output_matches_the_documented_schema_field_names() {
        let routed = Routed::allow();
        let output = CmdCheckOutput::from(routed);
        let json = serde_json::to_string(&output).expect("serializes");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parses back");
        let obj = value.as_object().expect("object");
        for field in ["decision", "reason", "rewrite", "note", "targets"] {
            assert!(obj.contains_key(field), "missing field {field} in {json}");
        }
    }
}
