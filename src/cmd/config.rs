//! The adapter's own settings, read from the policy file (#1229, FR-CMD-009).
//!
//! `route.deadline_ms` lives in the same JSON document as the routing rules
//! so the operator edits one file, but it is not a routing rule:
//! `legion_cmd::parse_policy` accepts the `route` key and never reads it, and
//! this module reads nothing but that key. The two parses are independent by
//! design; neither depends on the other having run first.
//!
//! The deadline is separate from the precog budget (`precog.budget_ms`, a
//! precog issue): the two are never shared and neither is derived from the
//! other. Route's deadline fails closed (an overrun is a deny); precog's
//! budget fails silent (an overrun is no context).

use std::time::Duration;

use serde_json::Value;

/// The decision deadline when `route.deadline_ms` is unset (FR-CMD-009).
/// Well under the harness's own hook timeout, so the adapter's deny reaches
/// the harness before the harness could time the hook out and fail open.
pub(crate) const DEFAULT_DEADLINE_MS: u64 = 7000;

/// Why the adapter's settings could not be read out of the policy text. Each
/// is a deny at the adapter (FR-CMD-009's "internal error"), never a silent
/// fall back to the default: an operator's typo in the deadline must be
/// caught, not hidden behind the value it was trying to override.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum ConfigError {
    #[error("policy text is not valid JSON: {0}")]
    Json(String),

    #[error("route must be an object")]
    RouteNotObject,

    #[error("route.{0} is not a setting the adapter reads")]
    UnknownSetting(String),

    #[error("route.deadline_ms must be a non-negative integer, found {0}")]
    InvalidDeadline(String),
}

/// The adapter's routing settings (FR-CMD-009: "read from configuration").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RouteSettings {
    pub deadline: Duration,
}

impl Default for RouteSettings {
    fn default() -> Self {
        Self {
            deadline: Duration::from_millis(DEFAULT_DEADLINE_MS),
        }
    }
}

impl RouteSettings {
    /// Reads `route.deadline_ms` from the policy text. A missing `route`
    /// object or a missing `deadline_ms` key means the default; a `route`
    /// object that is not an object, carries a key this adapter does not
    /// read, or carries a deadline that is not a non-negative integer is an
    /// error, so a typo never becomes a silent default.
    pub(crate) fn from_policy_text(text: &str) -> Result<Self, ConfigError> {
        let root: Value =
            serde_json::from_str(text).map_err(|e| ConfigError::Json(e.to_string()))?;
        let Some(route) = root.get("route") else {
            return Ok(Self::default());
        };
        let route = route.as_object().ok_or(ConfigError::RouteNotObject)?;
        if let Some(unknown) = route.keys().find(|key| key.as_str() != "deadline_ms") {
            return Err(ConfigError::UnknownSetting(unknown.clone()));
        }
        let Some(deadline) = route.get("deadline_ms") else {
            return Ok(Self::default());
        };
        let ms: u64 = deadline
            .as_u64()
            .ok_or_else(|| ConfigError::InvalidDeadline(deadline.to_string()))?;
        Ok(Self {
            deadline: Duration::from_millis(ms),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_route_object_means_the_7000ms_default() {
        let settings = RouteSettings::from_policy_text("{}").expect("parses");
        assert_eq!(settings.deadline, Duration::from_millis(7000));
        assert_eq!(settings, RouteSettings::default());
    }

    #[test]
    fn a_missing_deadline_key_means_the_default() {
        let settings = RouteSettings::from_policy_text(r#"{"route": {}}"#).expect("parses");
        assert_eq!(
            settings.deadline,
            Duration::from_millis(DEFAULT_DEADLINE_MS)
        );
    }

    #[test]
    fn a_present_deadline_overrides_the_default() {
        let settings =
            RouteSettings::from_policy_text(r#"{"route": {"deadline_ms": 250}}"#).expect("parses");
        assert_eq!(settings.deadline, Duration::from_millis(250));
    }

    #[test]
    fn a_zero_deadline_is_a_valid_setting() {
        // Zero can never be met, so it forces the overrun path; an operator
        // who sets it gets exactly what they asked for.
        let settings =
            RouteSettings::from_policy_text(r#"{"route": {"deadline_ms": 0}}"#).expect("parses");
        assert_eq!(settings.deadline, Duration::ZERO);
    }

    #[test]
    fn a_non_integer_deadline_is_an_error_not_a_silent_default() {
        let err = RouteSettings::from_policy_text(r#"{"route": {"deadline_ms": "soon"}}"#)
            .expect_err("a string deadline must not become 7000ms");
        assert_eq!(err, ConfigError::InvalidDeadline("\"soon\"".to_string()));
        let err = RouteSettings::from_policy_text(r#"{"route": {"deadline_ms": -1}}"#)
            .expect_err("a negative deadline must not become 7000ms");
        assert_eq!(err, ConfigError::InvalidDeadline("-1".to_string()));
    }

    #[test]
    fn an_unknown_setting_inside_route_is_an_error() {
        // `deadline_ms` misspelled would otherwise silently keep the default.
        let err = RouteSettings::from_policy_text(r#"{"route": {"deadlin_ms": 5}}"#)
            .expect_err("a typo must be caught");
        assert_eq!(err, ConfigError::UnknownSetting("deadlin_ms".to_string()));
    }

    #[test]
    fn a_route_value_that_is_not_an_object_is_an_error() {
        let err = RouteSettings::from_policy_text(r#"{"route": 7000}"#).expect_err("not an object");
        assert_eq!(err, ConfigError::RouteNotObject);
    }

    #[test]
    fn malformed_json_is_its_own_error() {
        let err = RouteSettings::from_policy_text("not json").expect_err("invalid JSON");
        assert!(matches!(err, ConfigError::Json(_)));
    }
}
