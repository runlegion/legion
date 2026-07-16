//! First-class `created_at` date filtering for legion's read surfaces
//! (`recall`, `consult`, `bullpen`, `surface`; #786).
//!
//! [`TimeRange`] is a half-open-or-closed window over `created_at`: `since`
//! is an inclusive lower bound, `until` an inclusive upper bound, either or
//! both `None` meaning unbounded on that end. It is parsed ONCE at the
//! CLI/API boundary from `--since`/`--until`/`--on` and threaded verbatim
//! down to the query layer as a single `&TimeRange` parameter -- the same
//! read path serves whole-store (`TimeRange::default()`, both ends `None`)
//! and bounded queries, no forked code path.
//!
//! `created_at` is stored UTC as an RFC3339 string, matching
//! `chrono::Utc::now().to_rfc3339()` elsewhere in this crate (e.g.
//! `Database::insert_reflection_with_meta`). Boundary dates are
//! interpreted in the local system timezone
//! (`jiff::tz::TimeZone::system()`) and converted to UTC once, at parse
//! time, as fixed [`jiff::civil::Date`] values (not re-interpreted later --
//! `today`/`yesterday` resolve against "now" exactly once, when the CLI
//! flag is parsed):
//!
//! - `since` becomes local midnight of that date, in UTC (inclusive lower
//!   bound).
//! - `until` becomes local midnight of the day AFTER that date, in UTC,
//!   used as an EXCLUSIVE upper bound. This exclusive-next-day shape
//!   (instead of an inclusive `23:59:59.999...` bound) sidesteps RFC3339
//!   fractional-second precision mismatches between a hand-built bound
//!   string and the variable-precision fractional seconds `chrono` emits
//!   for stored rows -- a bound built with fewer or more fractional digits
//!   than a given row's timestamp would otherwise compare incorrectly.
//!
//! Bound strings are formatted to match `chrono`'s `to_rfc3339()` shape
//! exactly (`+00:00` UTC offset, not `Z`) so plain lexicographic string
//! comparison against stored `created_at` values is correct.

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{ToSpan, Zoned};

use crate::error::{LegionError, Result};

/// A half-open-or-closed `created_at` window. `None` on either end means
/// unbounded on that end; `TimeRange::default()` (both `None`) is
/// unbounded on both ends -- the whole-store case.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TimeRange {
    pub since: Option<Date>,
    pub until: Option<Date>,
}

impl TimeRange {
    /// Parse `--since`/`--until`/`--on` values. Accepted grammar
    /// (deliberately small): absolute `YYYY-MM-DD`, relative `<N>d` /
    /// `<N>w` (days/weeks back from today), and the literals `today` /
    /// `yesterday`. `--on <date>` is sugar for `since=until=<date>`.
    ///
    /// `--on` combined with `--since`/`--until` is rejected by clap's
    /// `conflicts_with_all` before this is ever called; `on` wins here too
    /// as a defense-in-depth default, matching the CLI contract.
    pub fn parse(since: Option<&str>, until: Option<&str>, on: Option<&str>) -> Result<Self> {
        if let Some(on) = on {
            let date = parse_date(on)?;
            return Ok(Self {
                since: Some(date),
                until: Some(date),
            });
        }

        let since = since.map(parse_date).transpose()?;
        let until = until.map(parse_date).transpose()?;
        Ok(Self { since, until })
    }

    /// True when both ends are unbounded (the whole-store case).
    pub fn is_unbounded(&self) -> bool {
        self.since.is_none() && self.until.is_none()
    }

    /// Inclusive lower bound as a UTC RFC3339 string (local midnight of
    /// `since`), or `None` when unbounded below.
    pub fn since_bound(&self) -> Result<Option<String>> {
        self.since.map(day_start_utc).transpose()
    }

    /// Exclusive upper bound as a UTC RFC3339 string (local midnight of the
    /// day AFTER `until`), or `None` when unbounded above. Exclusive so a
    /// row created any time during `until`'s local day still matches --
    /// see the module docs for why this is a next-day bound rather than an
    /// inclusive end-of-day one.
    pub fn until_bound(&self) -> Result<Option<String>> {
        self.until
            .map(|d| {
                let next = d
                    .checked_add(1.day())
                    .map_err(|_| LegionError::InvalidDateFilter {
                        input: d.to_string(),
                    })?;
                day_start_utc(next)
            })
            .transpose()
    }

    /// `true` when `created_at` (a stored RFC3339 UTC string) falls inside
    /// this range. Used by read paths that scan a full, already-fetched
    /// candidate set in Rust (the hybrid recall join and `--cosine-only`)
    /// rather than filtering in SQL or Tantivy -- see `recall.rs`'s
    /// `join_and_score` for why that is safe (never a post-filter over a
    /// truncated/limited fetch). Shares the exact bound strings
    /// `since_bound`/`until_bound` produce, so the in-memory check and the
    /// SQL/Tantivy predicates can never disagree.
    pub fn contains(&self, created_at: &str) -> Result<bool> {
        if let Some(since) = self.since_bound()?
            && created_at < since.as_str()
        {
            return Ok(false);
        }
        if let Some(until) = self.until_bound()?
            && created_at >= until.as_str()
        {
            return Ok(false);
        }
        Ok(true)
    }

    /// SQL fragment applying this range as a `created_at` predicate, with
    /// positional placeholders starting at `first_idx` (SQLite positional
    /// params are 1-indexed). Composes into `format!`-built SQL the same
    /// way `archive_mode_where_clause` composes the archive-mode filter
    /// (#457/#782): callers splice the returned text into their WHERE
    /// clause and push `since_bound()`/`until_bound()` (in that order) onto
    /// their bind values.
    ///
    /// Each bound placeholder is referenced twice (once for the `IS NULL`
    /// unbounded-end guard, once for the comparison) but bound only ONCE --
    /// SQLite parameters may repeat within a statement. This keeps the
    /// bind-value count and placeholder count fixed regardless of whether
    /// either end is `Some` or `None`, so callers never need to branch the
    /// query text on which ends are bounded.
    pub fn sql_clause(first_idx: usize) -> String {
        let since_idx = first_idx;
        let until_idx = first_idx + 1;
        format!(
            " AND (?{since_idx} IS NULL OR created_at >= ?{since_idx}) \
             AND (?{until_idx} IS NULL OR created_at < ?{until_idx})"
        )
    }
}

/// Convert a local calendar date to its UTC midnight instant, formatted to
/// match `chrono::DateTime<Utc>::to_rfc3339()`'s shape exactly (`+00:00`,
/// not `Z`) so lexicographic string comparison against stored `created_at`
/// values is correct.
fn day_start_utc(date: Date) -> Result<String> {
    let local_midnight =
        date.to_zoned(TimeZone::system())
            .map_err(|_| LegionError::InvalidDateFilter {
                input: date.to_string(),
            })?;
    let utc = local_midnight.with_time_zone(TimeZone::UTC);
    Ok(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}+00:00",
        utc.year(),
        utc.month(),
        utc.day(),
        utc.hour(),
        utc.minute(),
        utc.second()
    ))
}

/// Parse one `--since`/`--until`/`--on` value against the accepted
/// grammar: `YYYY-MM-DD`, `<N>d`, `<N>w`, `today`, `yesterday`.
fn parse_date(input: &str) -> Result<Date> {
    let trimmed = input.trim();
    let invalid = || LegionError::InvalidDateFilter {
        input: input.to_string(),
    };

    let today = || Zoned::now().date();

    match trimmed {
        "today" => return Ok(today()),
        "yesterday" => return today().yesterday().map_err(|_| invalid()),
        _ => {}
    }

    if let Some(digits) = trimmed.strip_suffix('d')
        && let Ok(n) = digits.parse::<i64>()
    {
        return today().checked_sub(n.days()).map_err(|_| invalid());
    }

    if let Some(digits) = trimmed.strip_suffix('w')
        && let Ok(n) = digits.parse::<i64>()
    {
        return today().checked_sub(n.weeks()).map_err(|_| invalid());
    }

    trimmed.parse::<Date>().map_err(|_| invalid())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_unbounded() {
        let range = TimeRange::default();
        assert!(range.is_unbounded());
        assert_eq!(range.since_bound().unwrap(), None);
        assert_eq!(range.until_bound().unwrap(), None);
    }

    #[test]
    fn parse_absolute_date() {
        let range = TimeRange::parse(Some("2026-07-14"), None, None).unwrap();
        assert_eq!(range.since, Some("2026-07-14".parse().unwrap()));
        assert_eq!(range.until, None);
        assert!(!range.is_unbounded());
    }

    #[test]
    fn parse_on_is_sugar_for_since_until() {
        let range = TimeRange::parse(None, None, Some("2026-07-14")).unwrap();
        let d: Date = "2026-07-14".parse().unwrap();
        assert_eq!(range.since, Some(d));
        assert_eq!(range.until, Some(d));
    }

    #[test]
    fn parse_today_and_yesterday() {
        let today = Zoned::now().date();
        let range = TimeRange::parse(Some("today"), None, None).unwrap();
        assert_eq!(range.since, Some(today));

        let range = TimeRange::parse(Some("yesterday"), None, None).unwrap();
        assert_eq!(range.since, Some(today.yesterday().unwrap()));
    }

    #[test]
    fn parse_relative_days_and_weeks() {
        let today = Zoned::now().date();
        let range = TimeRange::parse(Some("3d"), None, None).unwrap();
        assert_eq!(range.since, Some(today.checked_sub(3.days()).unwrap()));

        let range = TimeRange::parse(Some("2w"), None, None).unwrap();
        assert_eq!(range.since, Some(today.checked_sub(2.weeks()).unwrap()));
    }

    #[test]
    fn parse_rejects_garbage() {
        let err = TimeRange::parse(Some("not-a-date"), None, None).unwrap_err();
        assert!(matches!(err, LegionError::InvalidDateFilter { .. }));
        assert!(err.to_string().contains("not-a-date"));
        assert!(err.to_string().contains("YYYY-MM-DD"));
    }

    #[test]
    fn parse_rejects_empty_and_whitespace() {
        assert!(TimeRange::parse(Some(""), None, None).is_err());
        assert!(TimeRange::parse(Some("   "), None, None).is_err());
    }

    #[test]
    fn empty_window_is_not_an_error() {
        // since > until: a valid (fixed) TimeRange whose predicate matches
        // nothing. Never an error at parse time -- the empty-result
        // behavior lives entirely in the query layer's WHERE predicate.
        let range = TimeRange::parse(Some("2026-07-20"), Some("2026-07-10"), None).unwrap();
        assert!(range.since_bound().unwrap() > range.until_bound().unwrap());
    }

    #[test]
    fn on_bound_covers_the_whole_local_day() {
        // A row created at any point during the `--on` date's local day
        // must satisfy `contains`, including right at local midnight and
        // right before the next local midnight.
        let range = TimeRange::parse(None, None, Some("2026-07-14")).unwrap();
        let since = range.since_bound().unwrap().unwrap();
        let until = range.until_bound().unwrap().unwrap();

        // Exactly at the lower bound: included.
        assert!(range.contains(&since).unwrap());
        // A microsecond-precision timestamp late in the day, just before
        // the exclusive next-day bound: included. Constructed by taking
        // the until bound (next day's midnight) and asserting it is
        // strictly greater than a plausible late-day timestamp.
        let late_in_day = since[..11].to_string() + "23:59:59.999999+00:00";
        assert!(late_in_day.as_str() < until.as_str());
        assert!(range.contains(&late_in_day).unwrap());
        // The next day's midnight itself is excluded (exclusive upper bound).
        assert!(!range.contains(&until).unwrap());
    }

    #[test]
    fn contains_respects_since_only() {
        let range = TimeRange::parse(Some("2026-07-14"), None, None).unwrap();
        let since = range.since_bound().unwrap().unwrap();
        assert!(range.contains(&since).unwrap());
        assert!(!range.contains("2026-07-13T23:59:59+00:00").unwrap());
    }

    #[test]
    fn contains_respects_until_only() {
        let range = TimeRange::parse(None, Some("2026-07-14"), None).unwrap();
        assert!(range.contains("2026-07-14T12:00:00+00:00").unwrap());
        let until = range.until_bound().unwrap().unwrap();
        assert!(!range.contains(&until).unwrap());
    }

    #[test]
    fn sql_clause_shape() {
        let clause = TimeRange::sql_clause(3);
        assert!(clause.contains("?3 IS NULL OR created_at >= ?3"));
        assert!(clause.contains("?4 IS NULL OR created_at < ?4"));
    }
}
