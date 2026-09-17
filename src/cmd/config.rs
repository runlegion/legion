//! Adapter-side configuration read from the policy file (#1229).
//!
//! `route.deadline_ms` is a setting the *adapter* reads, not a routing rule
//! `legion_cmd::parse_policy` has any vocabulary for -- the two live in one
//! JSON document, on purpose, but are parsed independently: a `"route"` key
//! is invisible to `RawPolicy` (it has no `#[serde(deny_unknown_fields)]`),
//! and this module never touches `tools`/`sym_jobs`.

use std::time::Duration;

/// The decision deadline when `route.deadline_ms` is absent from the policy
/// file (FR-CMD-009): well under the harness's own hook timeout (600s
/// default; today's Bash hook entries use 5-6s, which the cutover issue
/// must raise).
pub(crate) const DEFAULT_DEADLINE_MS: u64 = 7000;

/// Errors reading the adapter's own settings out of the policy text.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum ConfigError {
    /// The policy text is not valid JSON. In practice this never fires on
    /// the adapter's main path -- `legion_cmd::parse_policy` is always
    /// called on the same text first and fails first -- but `RouteSettings`
    /// makes no assumption about call order, so it reports its own error
    /// rather than panicking on a `.unwrap()`.
    #[error("route settings: policy text is not valid JSON: {0}")]
    Malformed(String),

    /// `route.deadline_ms` is present but not a non-negative integer. This
    /// is deliberately an error, not a silent fall back to the default: an
    /// operator's typo in the deadline must be caught, not hidden behind
    /// the value it was trying to override.
    #[error("route.deadline_ms must be a non-negative integer, found {0}")]
    InvalidDeadline(String),
}

/// The adapter's own routing settings (FR-CMD-009), separate from
/// `precog.budget_ms` (a different setting, a different budget, never
/// derived from this one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RouteSettings {
    pub(crate) deadline: Duration,
}

impl Default for RouteSettings {
    fn default() -> Self {
        Self {
            deadline: Duration::from_millis(DEFAULT_DEADLINE_MS),
        }
    }
}

impl RouteSettings {
    /// Reads `route.deadline_ms` out of the raw policy JSON text. Missing
    /// `"route"` object or missing `"deadline_ms"` key both mean "use the
    /// default" (FR-CMD-009: "default of 7000 ms when unset"); a present
    /// key of the wrong shape is `ConfigError::InvalidDeadline`.
    pub(crate) fn from_policy_text(text: &str) -> Result<Self, ConfigError> {
        let raw: serde_json::Value =
            serde_json::from_str(text).map_err(|e| ConfigError::Malformed(e.to_string()))?;
        let Some(deadline_value) = raw.get("route").and_then(|route| route.get("deadline_ms"))
        else {
            return Ok(Self::default());
        };
        let ms = deadline_value
            .as_u64()
            .ok_or_else(|| ConfigError::InvalidDeadline(deadline_value.to_string()))?;
        Ok(Self {
            deadline: Duration::from_millis(ms),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_route_key_defaults_to_7000ms() {
        let settings = RouteSettings::from_policy_text("{}").expect("parses");
        assert_eq!(settings.deadline, Duration::from_millis(7000));
    }

    #[test]
    fn missing_deadline_ms_key_defaults_to_7000ms() {
        let settings = RouteSettings::from_policy_text(r#"{"route": {}}"#).expect("parses");
        assert_eq!(settings.deadline, Duration::from_millis(7000));
    }

    #[test]
    fn present_deadline_ms_overrides_the_default() {
        let settings =
            RouteSettings::from_policy_text(r#"{"route": {"deadline_ms": 250}}"#).expect("parses");
        assert_eq!(settings.deadline, Duration::from_millis(250));
    }

    #[test]
    fn zero_deadline_ms_is_a_valid_setting() {
        // A zero deadline is meaningful for tests: it can never be met, so
        // it forces the overrun path deterministically.
        let settings =
            RouteSettings::from_policy_text(r#"{"route": {"deadline_ms": 0}}"#).expect("parses");
        assert_eq!(settings.deadline, Duration::ZERO);
    }

    #[test]
    fn non_numeric_deadline_ms_is_a_config_error_not_a_silent_default() {
        let err = RouteSettings::from_policy_text(r#"{"route": {"deadline_ms": "soon"}}"#)
            .expect_err("a string deadline must not silently become 7000ms");
        assert!(matches!(err, ConfigError::InvalidDeadline(_)));
    }

    #[test]
    fn negative_deadline_ms_is_a_config_error() {
        let err = RouteSettings::from_policy_text(r#"{"route": {"deadline_ms": -1}}"#)
            .expect_err("a negative deadline must not silently become 7000ms");
        assert!(matches!(err, ConfigError::InvalidDeadline(_)));
    }

    #[test]
    fn malformed_json_is_reported_as_its_own_error() {
        let err = RouteSettings::from_policy_text("not json").expect_err("invalid JSON");
        assert!(matches!(err, ConfigError::Malformed(_)));
    }

    #[test]
    fn default_route_settings_is_7000ms() {
        assert_eq!(
            RouteSettings::default().deadline,
            Duration::from_millis(DEFAULT_DEADLINE_MS)
        );
    }
}
