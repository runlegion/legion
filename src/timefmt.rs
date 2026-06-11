//! Shared time-display helpers.
//!
//! The single home for relative-time rendering ("3h ago"). Promoted from
//! status.rs (#615) so callers stop hand-rolling clock math; usage.rs
//! previously parsed hour/minute digits out of the timestamp string by hand.

use chrono::{DateTime, Utc};

/// Convert an ISO 8601 / RFC 3339 timestamp to a relative time string:
/// "just now", "Nm ago", "Nh ago", or "Nd ago". Unparseable input is
/// returned unchanged so callers degrade to showing the raw timestamp.
pub fn relative_time(iso_timestamp: &str) -> String {
    let parsed: std::result::Result<DateTime<Utc>, _> =
        DateTime::parse_from_rfc3339(iso_timestamp).map(|dt| dt.with_timezone(&Utc));

    let ts: DateTime<Utc> = match parsed {
        Ok(dt) => dt,
        Err(_) => return iso_timestamp.to_string(),
    };

    let now: DateTime<Utc> = Utc::now();
    let diff: chrono::TimeDelta = now.signed_duration_since(ts);

    let minutes: i64 = diff.num_minutes();
    if minutes < 1 {
        return "just now".to_string();
    }
    if minutes < 60 {
        return format!("{}m ago", minutes);
    }

    let hours: i64 = diff.num_hours();
    if hours < 24 {
        return format!("{}h ago", hours);
    }

    let days: i64 = diff.num_days();
    format!("{}d ago", days)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_time_just_now() {
        let now = Utc::now().to_rfc3339();
        let result = relative_time(&now);
        assert!(
            result == "just now" || result.ends_with("m ago"),
            "unexpected: {}",
            result
        );
    }

    #[test]
    fn relative_time_minutes_ago() {
        let past = (Utc::now() - chrono::Duration::minutes(15)).to_rfc3339();
        let result = relative_time(&past);
        assert!(result.contains("m ago"), "expected minutes ago: {}", result);
    }

    #[test]
    fn relative_time_hours_ago() {
        let past = (Utc::now() - chrono::Duration::hours(3)).to_rfc3339();
        let result = relative_time(&past);
        assert_eq!(result, "3h ago");
    }

    #[test]
    fn relative_time_days_ago() {
        let past = (Utc::now() - chrono::Duration::days(2)).to_rfc3339();
        let result = relative_time(&past);
        assert_eq!(result, "2d ago");
    }

    #[test]
    fn relative_time_invalid_falls_back() {
        let result = relative_time("not-a-timestamp");
        assert_eq!(result, "not-a-timestamp");
    }
}
