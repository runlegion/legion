//! The tool call the router decides about (FR-CMD-001).
//!
//! `ToolCall` is a CARRYING enum: the input variant *is* the tool, so
//! `tool_input` cannot disagree with `tool_name` the way a
//! `{ name: String, input: Value }` pair can. `Tool` is the separate closed
//! enum rulings match on (FR-CMD-017), deserialized from a plain string.

use serde::{Deserialize, Serialize};

/// A parsed harness tool call.
///
/// Thirteen variants. `Other` is the open end: an unrecognized `tool_name` is
/// data, not an error, because the harness gains tools faster than this enum
/// does and a hard failure there would fail closed on every new tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCall {
    Bash {
        command: String,
    },
    Edit {
        file_path: String,
        new_string: String,
    },
    Write {
        file_path: String,
        content: String,
    },
    MultiEdit {
        file_path: String,
        edits: Vec<String>,
    },
    Read {
        file_path: String,
    },
    Grep {
        pattern: String,
        path: String,
    },
    Glob {
        pattern: String,
        path: String,
    },
    Agent {
        prompt: String,
        subagent_type: String,
    },
    Task {
        prompt: String,
        subagent_type: String,
    },
    WebFetch {
        url: String,
    },
    WebSearch {
        query: String,
    },
    AskUserQuestion {
        raw: String,
    },
    Other {
        name: String,
        raw: String,
    },
}

/// The tool identity a ruling matches on (FR-CMD-017).
///
/// Kept separate from `ToolCall` on purpose: a ruling names a tool without
/// carrying that tool's input, and `ToolCall` cannot be constructed without
/// input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(into = "String")]
pub enum Tool {
    Bash,
    Edit,
    Write,
    MultiEdit,
    Read,
    Grep,
    Glob,
    Agent,
    Task,
    WebFetch,
    WebSearch,
    AskUserQuestion,
    Other(String),
}

impl Tool {
    /// The harness spelling of this tool.
    pub fn as_str(&self) -> &str {
        match self {
            Tool::Bash => "Bash",
            Tool::Edit => "Edit",
            Tool::Write => "Write",
            Tool::MultiEdit => "MultiEdit",
            Tool::Read => "Read",
            Tool::Grep => "Grep",
            Tool::Glob => "Glob",
            Tool::Agent => "Agent",
            Tool::Task => "Task",
            Tool::WebFetch => "WebFetch",
            Tool::WebSearch => "WebSearch",
            Tool::AskUserQuestion => "AskUserQuestion",
            Tool::Other(name) => name,
        }
    }
}

impl From<String> for Tool {
    fn from(s: String) -> Self {
        match s.as_str() {
            "Bash" => Tool::Bash,
            "Edit" => Tool::Edit,
            "Write" => Tool::Write,
            "MultiEdit" => Tool::MultiEdit,
            "Read" => Tool::Read,
            "Grep" => Tool::Grep,
            "Glob" => Tool::Glob,
            "Agent" => Tool::Agent,
            "Task" => Tool::Task,
            "WebFetch" => Tool::WebFetch,
            "WebSearch" => Tool::WebSearch,
            "AskUserQuestion" => Tool::AskUserQuestion,
            _ => Tool::Other(s),
        }
    }
}

impl From<Tool> for String {
    fn from(t: Tool) -> String {
        t.as_str().to_owned()
    }
}

impl<'de> Deserialize<'de> for Tool {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Tool::from(String::deserialize(d)?))
    }
}

/// A harness payload this crate cannot read.
///
/// An unrecognized `tool_name` is NOT one of these -- it maps to
/// `ToolCall::Other`. These are structural failures: the payload is not an
/// object, or `tool_name` is missing or not a string.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("hook payload is not a JSON object")]
    NotAnObject,
    #[error("hook payload has no string `tool_name` field")]
    MissingToolName,
}

/// Read a string field out of `tool_input`, defaulting to empty.
///
/// Deliberately lenient: a tool whose input is missing a field the router does
/// not read for that call must not fail the whole parse. The router decides on
/// what is present; absence is an empty string, which no route matches.
fn field(input: Option<&serde_json::Value>, key: &str) -> String {
    input
        .and_then(|v| v.get(key))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_owned()
}

/// Read a string-array field out of `tool_input`, defaulting to empty.
fn field_list(input: Option<&serde_json::Value>, key: &str) -> Vec<String> {
    input
        .and_then(|v| v.get(key))
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .map(|e| match e.as_str() {
                    Some(s) => s.to_owned(),
                    None => e.to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

impl ToolCall {
    /// The tool this call is for.
    pub fn tool(&self) -> Tool {
        match self {
            ToolCall::Bash { .. } => Tool::Bash,
            ToolCall::Edit { .. } => Tool::Edit,
            ToolCall::Write { .. } => Tool::Write,
            ToolCall::MultiEdit { .. } => Tool::MultiEdit,
            ToolCall::Read { .. } => Tool::Read,
            ToolCall::Grep { .. } => Tool::Grep,
            ToolCall::Glob { .. } => Tool::Glob,
            ToolCall::Agent { .. } => Tool::Agent,
            ToolCall::Task { .. } => Tool::Task,
            ToolCall::WebFetch { .. } => Tool::WebFetch,
            ToolCall::WebSearch { .. } => Tool::WebSearch,
            ToolCall::AskUserQuestion { .. } => Tool::AskUserQuestion,
            ToolCall::Other { name, .. } => Tool::Other(name.clone()),
        }
    }

    /// Parse a harness PreToolUse payload (FR-CMD-008).
    ///
    /// The ONLY constructor the adapter and `cmd-check` may use, so both read
    /// the same payload the same way and cannot drift.
    pub fn from_hook_json(v: &serde_json::Value) -> Result<ToolCall, ParseError> {
        if !v.is_object() {
            return Err(ParseError::NotAnObject);
        }
        let name = v
            .get("tool_name")
            .and_then(|n| n.as_str())
            .ok_or(ParseError::MissingToolName)?;
        let input = v.get("tool_input");
        Ok(match name {
            "Bash" => ToolCall::Bash {
                command: field(input, "command"),
            },
            "Edit" => ToolCall::Edit {
                file_path: field(input, "file_path"),
                new_string: field(input, "new_string"),
            },
            "Write" => ToolCall::Write {
                file_path: field(input, "file_path"),
                content: field(input, "content"),
            },
            "MultiEdit" => ToolCall::MultiEdit {
                file_path: field(input, "file_path"),
                edits: field_list(input, "edits"),
            },
            "Read" => ToolCall::Read {
                file_path: field(input, "file_path"),
            },
            "Grep" => ToolCall::Grep {
                pattern: field(input, "pattern"),
                path: field(input, "path"),
            },
            "Glob" => ToolCall::Glob {
                pattern: field(input, "pattern"),
                path: field(input, "path"),
            },
            "Agent" => ToolCall::Agent {
                prompt: field(input, "prompt"),
                subagent_type: field(input, "subagent_type"),
            },
            "Task" => ToolCall::Task {
                prompt: field(input, "prompt"),
                subagent_type: field(input, "subagent_type"),
            },
            "WebFetch" => ToolCall::WebFetch {
                url: field(input, "url"),
            },
            "WebSearch" => ToolCall::WebSearch {
                query: field(input, "query"),
            },
            "AskUserQuestion" => ToolCall::AskUserQuestion {
                raw: input.map(|i| i.to_string()).unwrap_or_default(),
            },
            other => ToolCall::Other {
                name: other.to_owned(),
                raw: input.map(|i| i.to_string()).unwrap_or_default(),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(name: &str, input: serde_json::Value) -> ToolCall {
        ToolCall::from_hook_json(&json!({"tool_name": name, "tool_input": input}))
            .expect("payload should parse")
    }

    #[test]
    fn every_variant_parses_from_a_harness_payload() {
        assert_eq!(
            parse("Bash", json!({"command": "ls -la"})),
            ToolCall::Bash {
                command: "ls -la".into()
            }
        );
        assert_eq!(
            parse("Edit", json!({"file_path": "a.rs", "new_string": "x"})),
            ToolCall::Edit {
                file_path: "a.rs".into(),
                new_string: "x".into()
            }
        );
        assert_eq!(
            parse("Write", json!({"file_path": "a.rs", "content": "x"})),
            ToolCall::Write {
                file_path: "a.rs".into(),
                content: "x".into()
            }
        );
        assert_eq!(
            parse(
                "MultiEdit",
                json!({"file_path": "a.rs", "edits": ["one", "two"]})
            ),
            ToolCall::MultiEdit {
                file_path: "a.rs".into(),
                edits: vec!["one".into(), "two".into()]
            }
        );
        assert_eq!(
            parse("Read", json!({"file_path": "a.rs"})),
            ToolCall::Read {
                file_path: "a.rs".into()
            }
        );
        assert_eq!(
            parse("Grep", json!({"pattern": "fn ", "path": "src"})),
            ToolCall::Grep {
                pattern: "fn ".into(),
                path: "src".into()
            }
        );
        assert_eq!(
            parse("Glob", json!({"pattern": "**/*.rs", "path": "src"})),
            ToolCall::Glob {
                pattern: "**/*.rs".into(),
                path: "src".into()
            }
        );
        assert_eq!(
            parse("Agent", json!({"prompt": "go", "subagent_type": "rust"})),
            ToolCall::Agent {
                prompt: "go".into(),
                subagent_type: "rust".into()
            }
        );
        assert_eq!(
            parse("Task", json!({"prompt": "go", "subagent_type": "rust"})),
            ToolCall::Task {
                prompt: "go".into(),
                subagent_type: "rust".into()
            }
        );
        assert_eq!(
            parse("WebFetch", json!({"url": "https://example.com"})),
            ToolCall::WebFetch {
                url: "https://example.com".into()
            }
        );
        assert_eq!(
            parse("WebSearch", json!({"query": "rust"})),
            ToolCall::WebSearch {
                query: "rust".into()
            }
        );
        assert!(matches!(
            parse("AskUserQuestion", json!({"questions": []})),
            ToolCall::AskUserQuestion { .. }
        ));
    }

    #[test]
    fn unknown_tool_name_maps_to_other_rather_than_erroring() {
        let call = parse("SomeFutureTool", json!({"whatever": 1}));
        match call {
            ToolCall::Other { name, raw } => {
                assert_eq!(name, "SomeFutureTool");
                assert!(raw.contains("whatever"));
            }
            other => panic!("expected Other, got {other:?}"),
        }
        assert_eq!(
            parse("SomeFutureTool", json!({})).tool(),
            Tool::Other("SomeFutureTool".into())
        );
    }

    #[test]
    fn tool_accessor_matches_every_variant() {
        assert_eq!(parse("Bash", json!({})).tool(), Tool::Bash);
        assert_eq!(parse("Edit", json!({})).tool(), Tool::Edit);
        assert_eq!(parse("Write", json!({})).tool(), Tool::Write);
        assert_eq!(parse("MultiEdit", json!({})).tool(), Tool::MultiEdit);
        assert_eq!(parse("Read", json!({})).tool(), Tool::Read);
        assert_eq!(parse("Grep", json!({})).tool(), Tool::Grep);
        assert_eq!(parse("Glob", json!({})).tool(), Tool::Glob);
        assert_eq!(parse("Agent", json!({})).tool(), Tool::Agent);
        assert_eq!(parse("Task", json!({})).tool(), Tool::Task);
        assert_eq!(parse("WebFetch", json!({})).tool(), Tool::WebFetch);
        assert_eq!(parse("WebSearch", json!({})).tool(), Tool::WebSearch);
        assert_eq!(
            parse("AskUserQuestion", json!({})).tool(),
            Tool::AskUserQuestion
        );
    }

    #[test]
    fn structural_failures_are_errors_not_other() {
        assert_eq!(
            ToolCall::from_hook_json(&json!("not an object")),
            Err(ParseError::NotAnObject)
        );
        assert_eq!(
            ToolCall::from_hook_json(&json!({"tool_input": {}})),
            Err(ParseError::MissingToolName)
        );
        assert_eq!(
            ToolCall::from_hook_json(&json!({"tool_name": 7})),
            Err(ParseError::MissingToolName)
        );
    }

    #[test]
    fn missing_tool_input_field_is_empty_not_a_parse_failure() {
        assert_eq!(
            ToolCall::from_hook_json(&json!({"tool_name": "Bash"})),
            Ok(ToolCall::Bash {
                command: String::new()
            })
        );
    }

    #[test]
    fn tool_round_trips_through_its_string_form() {
        for t in [
            Tool::Bash,
            Tool::Edit,
            Tool::Write,
            Tool::MultiEdit,
            Tool::Read,
            Tool::Grep,
            Tool::Glob,
            Tool::Agent,
            Tool::Task,
            Tool::WebFetch,
            Tool::WebSearch,
            Tool::AskUserQuestion,
            Tool::Other("Custom".into()),
        ] {
            assert_eq!(Tool::from(t.as_str().to_owned()), t);
        }
    }
}
