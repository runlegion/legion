use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::watch::WAKE_WORTHY_VERBS;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Behavioral shape of a verb -- governs wake-gate and reply routing.
///
/// Only `Wake` is load-bearing for the wake gate: a signal whose verb maps to
/// this shape will page an asleep recipient. The remaining shapes are modeled
/// for future downstream routing; today they all mean "does not wake".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VerbShape {
    /// Wakes an asleep recipient. The agent MUST reply (or explicitly decline).
    Wake,
    /// Informational delivery -- no wake, silence is acknowledgment.
    Record,
    /// Explicitly discard / no-op; useful for machine-generated noise.
    #[serde(rename = "fuckoff")]
    Fuckoff,
    /// May close a pending ask when the status matches a resolution pattern.
    #[serde(rename = "maybe-close")]
    MaybeClose,
}

/// Per-verb specification: shape plus any field constraints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerbSpec {
    /// Behavioral shape controlling wake and routing decisions.
    pub shape: VerbShape,

    /// Detail keys that MUST be present in a signal carrying this verb.
    ///
    /// An absent required field is a send-time error, not a warning.
    #[serde(default)]
    pub required_fields: Vec<String>,
}

// ---------------------------------------------------------------------------
// TOML manifest schema
//
// Each manifest file uses a top-level [verbs] table:
//
//   [verbs.rfc]
//   shape = "wake"
//   required_fields = ["budget"]
//
//   [verbs.announce]
//   shape = "record"
//
// Multiple files in the verbs dir are loaded and merged; later files override
// earlier ones (sorted by filename for determinism).
// ---------------------------------------------------------------------------

/// Wire-format for a single manifest file: a [verbs] table.
#[derive(Debug, Deserialize)]
struct ManifestFile {
    #[serde(default)]
    verbs: HashMap<String, VerbSpec>,
}

/// Complete verb manifest: maps verb names to their `VerbSpec`.
///
/// Use `builtin_default()` for the canonical embedded manifest, or `load(dir)`
/// to merge user-supplied overrides on top of it. `active_manifest()` returns
/// the process-lifetime singleton, resolving from env vars or the embedded
/// default.
#[derive(Debug, Clone)]
pub struct VerbManifest {
    verbs: HashMap<String, VerbSpec>,
}

impl VerbManifest {
    /// Return the embedded default manifest.
    ///
    /// The Wake set is derived directly from `WAKE_WORTHY_VERBS` (watch.rs) so
    /// the two sources cannot drift without a compile error.  Informational
    /// verbs (`announce`, `ack`, `info`, `answer`) are `Record`.  `rfc` carries
    /// `required_fields = ["budget"]` as the canonical example of a structured
    /// wake verb.
    pub fn builtin_default() -> Self {
        let mut verbs: HashMap<String, VerbSpec> = HashMap::new();

        // Wake set -- derived from the watch.rs const so drift is impossible.
        for &verb in WAKE_WORTHY_VERBS {
            verbs.insert(
                verb.to_string(),
                VerbSpec {
                    shape: VerbShape::Wake,
                    required_fields: Vec::new(),
                },
            );
        }

        // rfc overrides its entry with a required "budget" field.
        verbs.insert(
            "rfc".to_string(),
            VerbSpec {
                shape: VerbShape::Wake,
                required_fields: vec!["budget".to_string()],
            },
        );

        // Informational verbs: deliver to live sessions, never wake.
        for verb in ["announce", "ack", "info", "answer"] {
            verbs.insert(
                verb.to_string(),
                VerbSpec {
                    shape: VerbShape::Record,
                    required_fields: Vec::new(),
                },
            );
        }

        Self { verbs }
    }

    /// Load a manifest from a directory of `*.toml` files, overlaying them
    /// onto `builtin_default()`.
    ///
    /// Files are processed in sorted (lexicographic) order so the merge is
    /// deterministic.  A missing or empty directory silently returns
    /// `builtin_default()`.  A malformed TOML file logs to stderr and is
    /// skipped (fail-open: a broken manifest must never brick the wake gate).
    pub fn load(dir: &Path) -> Self {
        let mut manifest = Self::builtin_default();

        let read_dir = match std::fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(_) => return manifest, // dir absent or unreadable -> embedded default
        };

        let mut paths: Vec<std::path::PathBuf> = read_dir
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("toml") {
                    Some(path)
                } else {
                    None
                }
            })
            .collect();

        paths.sort();

        for path in &paths {
            let src = match std::fs::read_to_string(path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!(
                        "[legion verbs] failed to read {:?}: {} -- skipping",
                        path, e
                    );
                    continue;
                }
            };

            let parsed: ManifestFile = match toml::from_str(&src) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!(
                        "[legion verbs] malformed TOML in {:?}: {} -- skipping",
                        path, e
                    );
                    continue;
                }
            };

            for (verb, spec) in parsed.verbs {
                manifest.verbs.insert(verb, spec);
            }
        }

        manifest
    }

    /// Returns `true` iff `verb` maps to `VerbShape::Wake` in this manifest.
    ///
    /// Unknown verbs return `false` -- an unlisted verb does not wake.
    pub fn is_wake_worthy(&self, verb: &str) -> bool {
        matches!(
            self.verbs.get(verb),
            Some(VerbSpec {
                shape: VerbShape::Wake,
                ..
            })
        )
    }

    /// Returns the required detail-field names for `verb`.
    ///
    /// Returns an empty slice for unknown verbs or verbs with no requirements.
    pub fn required_fields(&self, verb: &str) -> &[String] {
        self.verbs
            .get(verb)
            .map(|s| s.required_fields.as_slice())
            .unwrap_or(&[])
    }

    /// The complete set of wake-worthy verb names in this manifest.
    ///
    /// Returned in sorted order for stable display.
    pub fn wake_verb_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .verbs
            .iter()
            .filter(|(_, spec)| spec.shape == VerbShape::Wake)
            .map(|(name, _)| name.as_str())
            .collect();
        names.sort_unstable();
        names
    }
}

// ---------------------------------------------------------------------------
// Process-lifetime singleton
// ---------------------------------------------------------------------------

static ACTIVE_MANIFEST: OnceLock<VerbManifest> = OnceLock::new();

/// Return the process-lifetime active verb manifest.
///
/// Resolution order (first match wins):
/// 1. `LEGION_VERBS_DIR` env var -- explicit override for tests and operators.
/// 2. `${CLAUDE_PLUGIN_ROOT}/verbs` -- conventional plugin dir when the plugin
///    root is configured.
/// 3. Embedded `builtin_default()` -- always available, no filesystem required.
///
/// The result is cached for the process lifetime via `OnceLock`. Tests that
/// need a different manifest should call `VerbManifest::load()` directly
/// rather than relying on this singleton.
pub fn active_manifest() -> &'static VerbManifest {
    ACTIVE_MANIFEST.get_or_init(|| {
        if let Ok(dir) = std::env::var("LEGION_VERBS_DIR") {
            let path = Path::new(&dir);
            if path.exists() {
                return VerbManifest::load(path);
            }
        }

        if let Ok(root) = std::env::var("CLAUDE_PLUGIN_ROOT") {
            let path = Path::new(&root).join("verbs");
            if path.exists() {
                return VerbManifest::load(&path);
            }
        }

        VerbManifest::builtin_default()
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: write a toml string to a tempdir file and return the dir.
    fn write_toml(dir: &tempfile::TempDir, name: &str, content: &str) {
        let path = dir.path().join(name);
        std::fs::write(path, content).expect("failed to write test toml");
    }

    // ---------------------------------------------------------------------------
    // builtin_default correctness
    // ---------------------------------------------------------------------------

    #[test]
    fn builtin_default_marks_all_wake_worthy_verbs_as_wake() {
        let m = VerbManifest::builtin_default();
        for verb in WAKE_WORTHY_VERBS {
            assert!(
                m.is_wake_worthy(verb),
                "WAKE_WORTHY_VERBS entry '{}' must be Wake shape in builtin_default",
                verb
            );
        }
    }

    #[test]
    fn builtin_default_marks_informational_verbs_as_not_wake() {
        let m = VerbManifest::builtin_default();
        for verb in ["announce", "ack", "info", "answer"] {
            assert!(
                !m.is_wake_worthy(verb),
                "informational verb '{}' must not be wake-worthy",
                verb
            );
        }
    }

    #[test]
    fn builtin_default_rfc_has_required_budget_field() {
        let m = VerbManifest::builtin_default();
        let fields = m.required_fields("rfc");
        assert!(
            fields.contains(&"budget".to_string()),
            "rfc must have required_fields = [\"budget\"] in builtin_default"
        );
    }

    #[test]
    fn builtin_default_unknown_verb_is_not_wake_worthy() {
        let m = VerbManifest::builtin_default();
        assert!(!m.is_wake_worthy("escalation"));
        assert!(!m.is_wake_worthy(""));
    }

    // ---------------------------------------------------------------------------
    // load: absent / empty dir returns builtin_default
    // ---------------------------------------------------------------------------

    #[test]
    fn load_absent_dir_returns_builtin_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Use a nonexistent subpath.
        let absent = dir.path().join("no_such_dir");
        let m = VerbManifest::load(&absent);
        // Must have the same wake set as builtin_default.
        let default = VerbManifest::builtin_default();
        for verb in WAKE_WORTHY_VERBS {
            assert_eq!(
                m.is_wake_worthy(verb),
                default.is_wake_worthy(verb),
                "verb '{}' wake status differs from builtin_default",
                verb
            );
        }
    }

    #[test]
    fn load_empty_dir_returns_builtin_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let m = VerbManifest::load(dir.path());
        let default = VerbManifest::builtin_default();
        for verb in WAKE_WORTHY_VERBS {
            assert_eq!(m.is_wake_worthy(verb), default.is_wake_worthy(verb));
        }
        for verb in ["announce", "ack", "info", "answer"] {
            assert_eq!(m.is_wake_worthy(verb), default.is_wake_worthy(verb));
        }
    }

    // ---------------------------------------------------------------------------
    // load: adding a new verb without touching Rust source
    // ---------------------------------------------------------------------------

    #[test]
    fn load_adds_new_wake_verb_from_toml_without_recompile() {
        // This is the "no recompile" criterion from the issue:
        // writing a TOML file and loading it makes a new verb wake-worthy.
        let dir = tempfile::tempdir().expect("tempdir");
        write_toml(
            &dir,
            "custom.toml",
            r#"
[verbs.escalation]
shape = "wake"
"#,
        );
        let m = VerbManifest::load(dir.path());
        assert!(
            m.is_wake_worthy("escalation"),
            "escalation must be wake-worthy after loading custom.toml"
        );
        // Existing wake verbs must still work.
        assert!(m.is_wake_worthy("question"));
    }

    // ---------------------------------------------------------------------------
    // load: overriding an existing verb's shape
    // ---------------------------------------------------------------------------

    #[test]
    fn load_can_override_existing_verb_shape() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_toml(
            &dir,
            "override.toml",
            r#"
[verbs.question]
shape = "record"
"#,
        );
        let m = VerbManifest::load(dir.path());
        assert!(
            !m.is_wake_worthy("question"),
            "overriding question to record must make it non-wake"
        );
        // Other wake verbs must be unaffected.
        assert!(m.is_wake_worthy("request"));
    }

    // ---------------------------------------------------------------------------
    // load: malformed TOML is fail-open
    // ---------------------------------------------------------------------------

    #[test]
    fn load_malformed_toml_is_fail_open() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_toml(&dir, "bad.toml", "this is not valid toml }{{{");
        // Must not panic; must return a usable manifest.
        let m = VerbManifest::load(dir.path());
        // The builtin wake set must still be intact.
        assert!(m.is_wake_worthy("question"));
        assert!(!m.is_wake_worthy("announce"));
    }

    #[test]
    fn load_good_file_processed_after_bad_file() {
        // Sorted by filename: "a_bad.toml" < "b_good.toml".
        // The good file must still be applied even if the bad one is skipped.
        let dir = tempfile::tempdir().expect("tempdir");
        write_toml(&dir, "a_bad.toml", "not valid { toml");
        write_toml(
            &dir,
            "b_good.toml",
            r#"
[verbs.escalation]
shape = "wake"
"#,
        );
        let m = VerbManifest::load(dir.path());
        assert!(m.is_wake_worthy("escalation"));
    }

    // ---------------------------------------------------------------------------
    // required_fields
    // ---------------------------------------------------------------------------

    #[test]
    fn required_fields_returns_empty_for_unknown_verb() {
        let m = VerbManifest::builtin_default();
        assert!(m.required_fields("nonexistent").is_empty());
    }

    #[test]
    fn required_fields_returns_empty_for_question() {
        let m = VerbManifest::builtin_default();
        assert!(m.required_fields("question").is_empty());
    }

    // ---------------------------------------------------------------------------
    // shipped default.toml wake set matches builtin_default
    // ---------------------------------------------------------------------------

    #[test]
    fn shipped_default_toml_wake_set_matches_builtin_default() {
        // Locate plugin/verbs/default.toml relative to CARGO_MANIFEST_DIR.
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let toml_path = std::path::Path::new(manifest_dir)
            .join("plugin")
            .join("verbs")
            .join("default.toml");

        let dir = tempfile::tempdir().expect("tempdir");
        // Copy the shipped file into a fresh temp dir so load() picks it up.
        let dest = dir.path().join("default.toml");
        std::fs::copy(&toml_path, &dest)
            .unwrap_or_else(|e| panic!("failed to copy {:?}: {}", toml_path, e));

        let loaded = VerbManifest::load(dir.path());
        let builtin = VerbManifest::builtin_default();

        // Every builtin wake verb must be wake in the shipped file.
        for verb in WAKE_WORTHY_VERBS {
            assert_eq!(
                loaded.is_wake_worthy(verb),
                builtin.is_wake_worthy(verb),
                "shipped default.toml and builtin_default disagree on wake status for '{}'",
                verb
            );
        }

        // Informational verbs must not be wake in the shipped file either.
        for verb in ["announce", "ack", "info", "answer"] {
            assert!(
                !loaded.is_wake_worthy(verb),
                "shipped default.toml marks '{}' as wake; it should be record",
                verb
            );
        }
    }

    // ---------------------------------------------------------------------------
    // regression: is_wake_worthy via manifest == #586 canon
    // ---------------------------------------------------------------------------

    #[test]
    fn manifest_wake_gate_is_behavior_identical_to_586_canon() {
        let m = VerbManifest::builtin_default();

        // Canon wake verbs from #586 -- every one must wake.
        let canon_wake = [
            "question",
            "request",
            "handoff",
            "correction",
            "proposal",
            "decision",
            "rfc",
            "routing",
        ];
        for verb in canon_wake {
            assert!(
                m.is_wake_worthy(verb),
                "#586 canon wake verb '{}' must be wake-worthy",
                verb
            );
        }

        // Dropped verbs (old #404 set, not canon per #586) must NOT wake.
        for verb in ["help", "blocker"] {
            assert!(
                !m.is_wake_worthy(verb),
                "status-slot verb '{}' must not be wake-worthy (dropped in #586)",
                verb
            );
        }

        // Informational verbs must not wake.
        for verb in ["announce", "ack", "info", "answer", "review"] {
            assert!(
                !m.is_wake_worthy(verb),
                "informational verb '{}' must not be wake-worthy",
                verb
            );
        }
    }
}
