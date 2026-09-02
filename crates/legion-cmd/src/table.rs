//! The route table: policy as data, not as match arms (NFR-CMD-005).
//!
//! Every type here is behaviorless serde data. Changing what a command does is
//! a table edit, not a rule rewrite, and the structural check that keeps it
//! that way is that this module contains no routing logic at all -- the
//! evaluator lives in `router`, and there is exactly one of it.

use serde::{Deserialize, Serialize};

/// A table that will not load, named precisely enough to fix.
#[derive(Debug, thiserror::Error)]
pub enum TableError {
    #[error("route table does not parse as TOML: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("route table serializes back out invalid: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("route[{index}] ({binary} {subcommand}): {problem}")]
    Route {
        index: usize,
        binary: String,
        subcommand: String,
        problem: String,
    },
    #[error("route[{index}] ({binary} {subcommand}) pattern '{pattern}': {problem}")]
    Pattern {
        index: usize,
        binary: String,
        subcommand: String,
        pattern: String,
        problem: String,
    },
}

/// What happens when nothing matches.
///
/// EXACTLY TWO VALUES (NFR-CMD-005 rev 6). There is no `drop`, `skip`, or
/// `ignore`: a silently swallowed command produces an agent that cannot tell
/// success from a no-op, which is worse than either answer. A third value in
/// the table is a load failure, not a fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DefaultOutcome {
    Allow,
    Deny,
}

impl<'de> Deserialize<'de> for DefaultOutcome {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        match raw.as_str() {
            "allow" => Ok(DefaultOutcome::Allow),
            "deny" => Ok(DefaultOutcome::Deny),
            other => Err(serde::de::Error::custom(format!(
                "unknown default outcome `{other}` -- the only accepted values are \
                 `allow` and `deny`; there is no silent-drop mode"
            ))),
        }
    }
}

/// The two policy defaults every table must state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Defaults {
    /// No route matched the call.
    pub no_match: DefaultOutcome,
    /// A recall-backed rule found nothing to say.
    pub recall_miss: DefaultOutcome,
}

/// A command that wraps another command (`env`, `xargs`, `time`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Wrapper {
    pub name: String,
    /// Whether this wrapper's own options precede the wrapped command.
    pub options_before_command: bool,
}

/// Where a binary's subcommand group documents itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupHelp {
    pub binary: String,
    pub group: String,
    pub help_command: String,
}

/// A sanctioned escape hatch (FR-CMD-012).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Escape {
    pub flag: String,
    /// A reason-required escape is the whole point: an unexplained bypass is
    /// indistinguishable from an evasion.
    pub requires_reason: bool,
}

/// One flag on a route, and what it captures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlagSpec {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub takes_value: bool,
    #[serde(default)]
    pub capture: Option<String>,
}

/// What kind of argument-level transformation a pattern performs.
///
/// A CLOSED enum. A new kind is a new variant with its own tests, never a
/// regex-over-argv catch-all -- the catch-all is how the shell guards became
/// unprovable in the first place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArgKind {
    Digits { capture: String },
    Flags { policy: FlagPolicy },
    Ext { from: String, to: String },
    Conflict { flags: Vec<String> },
    Path,
    TextCarry { flag: String, placeholder: String },
    Append { args: Vec<String> },
    Scope { reason: String },
    NoExtraPositionals,
}

/// How a pattern treats flags it does not name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "policy", rename_all = "snake_case")]
pub enum FlagPolicy {
    None,
    Blocklist { flags: Vec<String> },
    ClusterSplit,
}

/// What a matched pattern resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArgOutcome {
    Rewrite,
    Proxy,
    Deny,
}

/// One argument-level rule on a route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArgPattern {
    pub name: String,
    #[serde(flatten)]
    pub kind: ArgKind,
    pub outcome: ArgOutcome,
    #[serde(default)]
    pub equivalent_override: Option<String>,
}

/// One routed subcommand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Route {
    pub binary: String,
    /// Space-joined words, so `pr create` is one route rather than a tree.
    pub subcommand: String,
    #[serde(default)]
    pub global_options: Vec<FlagSpec>,
    #[serde(default)]
    pub positional_captures: Vec<String>,
    pub equivalent: String,
    pub reason: String,
    #[serde(default)]
    pub guidance: String,
    #[serde(default)]
    pub flags: Vec<FlagSpec>,
    /// The advisory arm: `Allow` plus this text (NFR-CMD-005 rev 6).
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub patterns: Vec<ArgPattern>,
}

/// The whole policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteTable {
    #[serde(default)]
    pub managed_binaries: Vec<String>,
    #[serde(default)]
    pub interpreters: Vec<String>,
    #[serde(default)]
    pub wrappers: Vec<Wrapper>,
    #[serde(default)]
    pub fully_gated_binaries: Vec<String>,
    #[serde(default)]
    pub group_help: Vec<GroupHelp>,
    #[serde(default)]
    pub escape_vocabulary: Vec<Escape>,
    /// Not optional: a table that does not state its defaults has no answer
    /// for the no-match case, and defaulting the default is how a silent
    /// policy gets chosen by whoever wrote the code rather than the operator.
    pub defaults: Defaults,
    #[serde(default)]
    pub routes: Vec<Route>,
}

/// The route table shipped inside the binary.
///
/// Embedded rather than read from disk: the router must answer with no I/O on
/// its call path (FR-CMD-001), and a table it had to open is a file read.
pub const EMBEDDED_TABLE_TOML: &str = include_str!("../route-table.toml");

impl RouteTable {
    /// Parse the embedded table.
    pub fn embedded() -> Result<RouteTable, TableError> {
        RouteTable::from_toml(EMBEDDED_TABLE_TOML)
    }

    /// Parse a table from TOML.
    pub fn from_toml(s: &str) -> Result<RouteTable, TableError> {
        Ok(toml::from_str(s)?)
    }

    /// Serialize back to TOML.
    pub fn to_toml(&self) -> Result<String, TableError> {
        Ok(toml::to_string(self)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = "\
[defaults]
no_match = \"allow\"
recall_miss = \"allow\"
";

    #[test]
    fn the_embedded_table_parses_with_every_section_present_and_empty() {
        let t = RouteTable::embedded().expect("embedded table must parse");
        assert!(t.managed_binaries.is_empty());
        assert!(t.interpreters.is_empty());
        assert!(t.wrappers.is_empty());
        assert!(t.fully_gated_binaries.is_empty());
        assert!(t.group_help.is_empty());
        assert!(t.escape_vocabulary.is_empty());
        assert!(t.routes.is_empty());
        assert_eq!(t.defaults.no_match, DefaultOutcome::Allow);
        assert_eq!(t.defaults.recall_miss, DefaultOutcome::Allow);
    }

    #[test]
    fn the_embedded_toml_names_every_section_explicitly() {
        // Presence in the STRUCT is satisfied by serde defaults; the issue
        // asks for presence in the TOML, so the file itself is checked.
        for section in [
            "managed_binaries",
            "interpreters",
            "wrappers",
            "fully_gated_binaries",
            "group_help",
            "escape_vocabulary",
            "defaults",
            "routes",
        ] {
            assert!(
                EMBEDDED_TABLE_TOML.contains(section),
                "embedded table is missing section `{section}`"
            );
        }
    }

    #[test]
    fn the_table_round_trips_through_toml() {
        let t = RouteTable::embedded().expect("parse");
        let out = t.to_toml().expect("serialize");
        let back = RouteTable::from_toml(&out).expect("reparse");
        assert_eq!(t, back);
    }

    #[test]
    fn a_populated_table_round_trips_too() {
        let mut t = RouteTable::embedded().expect("parse");
        t.managed_binaries.push("gh".into());
        t.interpreters.push("bash".into());
        t.wrappers.push(Wrapper {
            name: "env".into(),
            options_before_command: true,
        });
        t.fully_gated_binaries.push("curl".into());
        t.group_help.push(GroupHelp {
            binary: "legion".into(),
            group: "issue".into(),
            help_command: "legion issue --help".into(),
        });
        t.escape_vocabulary.push(Escape {
            flag: "--raw".into(),
            requires_reason: true,
        });
        t.routes.push(Route {
            binary: "gh".into(),
            subcommand: "issue list".into(),
            global_options: vec![],
            positional_captures: vec![],
            equivalent: "legion issue list".into(),
            reason: "work-source actions go through legion".into(),
            guidance: String::new(),
            flags: vec![FlagSpec {
                name: "--repo".into(),
                aliases: vec!["-R".into()],
                takes_value: true,
                capture: Some("repo".into()),
            }],
            note: Some("this one is advisory".into()),
            patterns: vec![ArgPattern {
                name: "issue-number".into(),
                kind: ArgKind::Digits {
                    capture: "number".into(),
                },
                outcome: ArgOutcome::Rewrite,
                equivalent_override: None,
            }],
        });
        let out = t.to_toml().expect("serialize");
        let back = RouteTable::from_toml(&out).expect("reparse");
        assert_eq!(t, back);
    }

    #[test]
    fn a_third_default_outcome_is_refused_by_name() {
        for bad in ["drop", "skip", "ignore"] {
            let toml_src = format!("[defaults]\nno_match = \"{bad}\"\nrecall_miss = \"allow\"\n");
            let err = RouteTable::from_toml(&toml_src).expect_err("must refuse");
            let msg = err.to_string();
            assert!(
                msg.contains(bad),
                "error should name the offending value: {msg}"
            );
            assert!(msg.contains("allow"), "error should name `allow`: {msg}");
            assert!(msg.contains("deny"), "error should name `deny`: {msg}");
            assert!(
                msg.contains("no_match"),
                "error should name the field: {msg}"
            );
        }
    }

    #[test]
    fn a_table_with_no_defaults_is_refused() {
        let err = RouteTable::from_toml("managed_binaries = []\n").expect_err("must refuse");
        assert!(err.to_string().contains("defaults"), "{err}");
    }

    #[test]
    fn defaults_deny_parses() {
        let t = RouteTable::from_toml("[defaults]\nno_match = \"deny\"\nrecall_miss = \"deny\"\n")
            .expect("parse");
        assert_eq!(t.defaults.no_match, DefaultOutcome::Deny);
        assert_eq!(t.defaults.recall_miss, DefaultOutcome::Deny);
    }

    #[test]
    fn minimal_table_is_the_documented_shape() {
        let t = RouteTable::from_toml(MINIMAL).expect("parse");
        assert!(t.routes.is_empty());
    }
}
