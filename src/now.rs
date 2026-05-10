//! Time-of-day awareness for SessionStart (#410). Agents pattern-match on
//! conversation density and start saying "tonight" / "wind down" when the
//! operator has the rest of the workday ahead. Injecting a time + sunphase
//! line into SessionStart shifts the framing to the actual hour.
//!
//! Day-of-week matters too: this session demonstrated planning a "Mon-Thu"
//! sprint on a Sunday because the date was visible but the weekday wasn't.
//!
//! Output is intentionally one short line so it never crowds identity.

use chrono::{DateTime, Datelike, Local, Timelike};
use serde::Serialize;

/// Coarse time-of-day bucket. Drives framing register (morning -> kicking
/// off, afternoon -> mid-stride, evening -> wrap-up) more reliably than
/// raw hour numerals do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Sunphase {
    LateNight,
    PreDawn,
    Morning,
    Midday,
    Afternoon,
    Evening,
    Night,
}

impl Sunphase {
    pub fn from_local_hour(hour: u32) -> Self {
        match hour {
            0..=4 => Self::LateNight,
            5..=6 => Self::PreDawn,
            7..=10 => Self::Morning,
            11..=13 => Self::Midday,
            14..=17 => Self::Afternoon,
            18..=20 => Self::Evening,
            _ => Self::Night,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::LateNight => "late-night",
            Self::PreDawn => "pre-dawn",
            Self::Morning => "morning",
            Self::Midday => "midday",
            Self::Afternoon => "afternoon",
            Self::Evening => "evening",
            Self::Night => "night",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct NowSnapshot {
    pub date: String,
    pub weekday: String,
    pub time: String,
    pub timezone: String,
    pub sunphase: &'static str,
}

impl NowSnapshot {
    pub fn from_local(now: DateTime<Local>) -> Self {
        let phase = Sunphase::from_local_hour(now.hour());
        Self {
            date: now.format("%Y-%m-%d").to_string(),
            weekday: now.weekday().to_string(),
            time: now.format("%H:%M").to_string(),
            timezone: now.format("%Z").to_string(),
            sunphase: phase.as_str(),
        }
    }

    /// One-line banner suitable for SessionStart additionalContext.
    /// Format: `[Legion] Now: Sunday 2026-05-10, 14:32 PT (afternoon)`.
    /// Timezone is omitted when chrono returns an empty `%Z` (some systems
    /// where `%Z` resolves to nothing rather than a name) so the line stays
    /// well-formed.
    pub fn banner(&self) -> String {
        let tz = if self.timezone.is_empty() {
            String::new()
        } else {
            format!(" {}", self.timezone)
        };
        format!(
            "[Legion] Now: {} {}, {}{} ({})",
            self.weekday, self.date, self.time, tz, self.sunphase
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn sunphase_covers_all_24_hours() {
        for h in 0..24 {
            // Should never panic; every hour maps to exactly one phase.
            let _ = Sunphase::from_local_hour(h);
        }
    }

    #[test]
    fn sunphase_boundaries() {
        assert_eq!(Sunphase::from_local_hour(0), Sunphase::LateNight);
        assert_eq!(Sunphase::from_local_hour(4), Sunphase::LateNight);
        assert_eq!(Sunphase::from_local_hour(5), Sunphase::PreDawn);
        assert_eq!(Sunphase::from_local_hour(6), Sunphase::PreDawn);
        assert_eq!(Sunphase::from_local_hour(7), Sunphase::Morning);
        assert_eq!(Sunphase::from_local_hour(10), Sunphase::Morning);
        assert_eq!(Sunphase::from_local_hour(11), Sunphase::Midday);
        assert_eq!(Sunphase::from_local_hour(13), Sunphase::Midday);
        assert_eq!(Sunphase::from_local_hour(14), Sunphase::Afternoon);
        assert_eq!(Sunphase::from_local_hour(17), Sunphase::Afternoon);
        assert_eq!(Sunphase::from_local_hour(18), Sunphase::Evening);
        assert_eq!(Sunphase::from_local_hour(20), Sunphase::Evening);
        assert_eq!(Sunphase::from_local_hour(21), Sunphase::Night);
        assert_eq!(Sunphase::from_local_hour(23), Sunphase::Night);
    }

    #[test]
    fn snapshot_from_known_local() {
        let dt = Local
            .with_ymd_and_hms(2026, 5, 10, 14, 32, 0)
            .single()
            .unwrap();
        let snap = NowSnapshot::from_local(dt);
        assert_eq!(snap.date, "2026-05-10");
        assert_eq!(snap.weekday, "Sun");
        assert_eq!(snap.time, "14:32");
        assert_eq!(snap.sunphase, "afternoon");
    }

    #[test]
    fn banner_includes_weekday_date_time_phase() {
        let dt = Local
            .with_ymd_and_hms(2026, 5, 10, 14, 32, 0)
            .single()
            .unwrap();
        let banner = NowSnapshot::from_local(dt).banner();
        assert!(banner.starts_with("[Legion] Now: Sun 2026-05-10"));
        assert!(banner.contains("14:32"));
        assert!(banner.contains("(afternoon)"));
    }

    #[test]
    fn banner_omits_empty_timezone() {
        let dt = Local
            .with_ymd_and_hms(2026, 5, 10, 14, 32, 0)
            .single()
            .unwrap();
        let mut snap = NowSnapshot::from_local(dt);
        snap.timezone = String::new();
        let banner = snap.banner();
        assert!(
            !banner.contains("  "),
            "double space when timezone is empty: {banner}"
        );
        assert!(banner.ends_with("(afternoon)"));
    }
}
