//! Bullpen domain: team posts, decay TTLs (#376), read cursors,
//! resolution markers (#362), and watch signal handling. Owns the
//! `board_reads` and `watch_handled` DDL plus the reflections
//! lifecycle-column migrations.

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension};

use super::reflections::{REFLECTION_COLUMNS, map_reflection_row};
use super::{Database, Reflection};
use crate::error::{LegionError, Result};

/// WHERE fragment shared by every read of live (active) team posts:
/// team audience, not archived, not soft-deleted. The decay and
/// resolution clauses bind positional parameters, so each query appends
/// those with its own parameter index. `get_unhandled_signals_for_repo`
/// keeps an `r.`-aliased copy (without the archive filter) inline.
const ACTIVE_TEAM_POST_WHERE: &str =
    "audience = 'team' AND archived_at IS NULL AND deleted_at IS NULL";

/// TTL hours for design or architecture posts.
const TTL_DESIGN_HOURS: i64 = 14 * 24;
/// TTL hours for signal-shaped posts (text starts with `@`).
const TTL_SIGNAL_HOURS: i64 = 7 * 24;
/// TTL hours for default broadcast posts.
const TTL_DEFAULT_HOURS: i64 = 48;

/// Compute (expires_at, evergreen) for a stored reflection (#376).
///
/// Non-team rows return (None, false): decay applies only to bullpen posts,
/// not to private reflections recalled by embedding similarity.
///
/// Team rows resolve in priority order:
/// 1. domain == "identity" or tags contains "doctrine" -> evergreen
/// 2. tags contains "design" or "architecture" -> 14 days
/// 3. text starts with `@` (directed signal or @all broadcast) -> 7 days
/// 4. default -> 48 hours
pub(super) fn compute_post_ttl(
    audience: &str,
    domain: Option<&str>,
    tags: Option<&str>,
    text: &str,
    created_at: chrono::DateTime<Utc>,
) -> (Option<String>, bool) {
    if audience != "team" {
        return (None, false);
    }
    let tag_list: Vec<&str> = tags
        .map(|t| t.split(',').map(str::trim).collect())
        .unwrap_or_default();
    let has_tag = |needle: &str| tag_list.contains(&needle);

    if domain == Some("identity") || has_tag("doctrine") {
        return (None, true);
    }
    let hours = if has_tag("design") || has_tag("architecture") {
        TTL_DESIGN_HOURS
    } else if text.starts_with('@') {
        TTL_SIGNAL_HOURS
    } else {
        TTL_DEFAULT_HOURS
    };
    let expires = created_at + chrono::Duration::hours(hours);
    (Some(expires.to_rfc3339()), false)
}

/// `board_reads` (per-reader cursors) and `watch_handled` (per-repo
/// signal dedup) tables.
pub(super) fn create_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS board_reads (
                reader_repo TEXT NOT NULL PRIMARY KEY,
                last_read_at TEXT NOT NULL
            );",
    )?;

    // Migration 7: Per-repo signal handling for @all broadcasts (#85).
    // The watch_handled table tracks which repo has seen which signal,
    // replacing the global handled_at column for watch purposes.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS watch_handled (
                signal_id TEXT NOT NULL,
                repo_name TEXT NOT NULL,
                handled_at TEXT NOT NULL,
                PRIMARY KEY (signal_id, repo_name)
            );",
    )?;
    Ok(())
}

/// Board migrations: the board_reads cursor id (#400) and the
/// reflections bullpen-lifecycle columns (decay #376, resolution #362),
/// in their original patch order.
pub(super) fn migrate(conn: &Connection) -> Result<()> {
    // Migration: add last_read_id to board_reads (#400).
    //
    // Without an id alongside the timestamp, the MCP notifier's
    // per-recipient cursor cannot disambiguate two rows that share a
    // created_at -- the strict-`>` comparator in `get_board_posts_since`
    // is `(created_at > ?1) OR (created_at = ?1 AND id > ?2)`, and
    // `?2 = ""` (the empty seed) makes any non-empty id satisfy the
    // tie-break and re-emits the just-delivered row on every boot. The
    // empty-string default is correct for existing rows: it means "no
    // id known, treat the timestamp as the only ordering signal" --
    // identical to the pre-#400 behaviour the row was inserted under.
    if !Database::has_column(conn, "board_reads", "last_read_id")? {
        conn.execute_batch(
            "ALTER TABLE board_reads ADD COLUMN last_read_id TEXT NOT NULL DEFAULT '';",
        )?;
    }

    // Migration 18: Bullpen post decay (#376).
    //
    // Stale bullpen posts were landing in agent context via the channel
    // notification path, costing tokens to read and reason about threads
    // long since resolved. Filter at injection: every team-audience post
    // gets either an `expires_at` timestamp (computed at insert from
    // domain/tags/text shape) or `evergreen=1` (identity/doctrine).
    // Read-side queries filter out expired non-evergreen rows. Posts stay
    // in the DB; they are simply invisible to the agent surface.
    let new_expires = !Database::has_column(conn, "reflections", "expires_at")?;
    let new_evergreen = !Database::has_column(conn, "reflections", "evergreen")?;
    if new_expires {
        conn.execute_batch("ALTER TABLE reflections ADD COLUMN expires_at TEXT;")?;
    }
    if new_evergreen {
        conn.execute_batch(
            "ALTER TABLE reflections ADD COLUMN evergreen INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_reflections_team_expires \
                 ON reflections(audience, expires_at) \
                 WHERE audience = 'team' AND archived_at IS NULL AND deleted_at IS NULL;",
    )?;

    // Backfill: compute TTL for every team row that lacks one. Done in
    // Rust (not pure SQL) so the same `compute_post_ttl` rule used at
    // insert applies to historical rows -- no drift between migration
    // logic and runtime logic.
    if new_expires || new_evergreen {
        // Backfill row tuple: (id, created_at, audience, domain, tags, text).
        type BackfillRow = (
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            String,
        );
        let mut stmt = conn.prepare(
            "SELECT id, created_at, audience, domain, tags, text \
                 FROM reflections \
                 WHERE audience = 'team' AND expires_at IS NULL AND evergreen = 0",
        )?;
        let rows: Vec<BackfillRow> = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(stmt);

        for (id, created_at, audience, domain, tags, text) in rows {
            let parsed = chrono::DateTime::parse_from_rfc3339(&created_at)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            let (expires_at, evergreen) =
                compute_post_ttl(&audience, domain.as_deref(), tags.as_deref(), &text, parsed);
            conn.execute(
                "UPDATE reflections SET expires_at = ?1, evergreen = ?2 WHERE id = ?3",
                rusqlite::params![expires_at, evergreen as i32, id],
            )?;
        }
    }

    // Migration 19: Signal/post resolution marker (#362).
    //
    // Threads converge on a decision but the originating signal stays
    // open. Agents who did not see the convergence walk the thread again,
    // posting the same conclusion. `resolved_at` + optional
    // `resolved_by_reflection_id` mark a thread done. Resolved rows are
    // filtered out of read paths by default; agents do not see them.
    // Operator review uses `--include-resolved`.
    if !Database::has_column(conn, "reflections", "resolved_at")? {
        conn.execute_batch("ALTER TABLE reflections ADD COLUMN resolved_at TEXT;")?;
    }
    if !Database::has_column(conn, "reflections", "resolved_by_reflection_id")? {
        conn.execute_batch("ALTER TABLE reflections ADD COLUMN resolved_by_reflection_id TEXT;")?;
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_reflections_team_unresolved \
                 ON reflections(audience, resolved_at) \
                 WHERE audience = 'team' AND archived_at IS NULL AND deleted_at IS NULL;",
    )?;
    Ok(())
}

impl Database {
    /// Mark a bullpen post or signal as resolved (#362).
    ///
    /// Records `resolved_at = now` and an optional reflection id pointing at
    /// the reflection that holds the team's converged decision. Idempotent:
    /// re-resolving a row updates `resolved_at` to the latest call but
    /// preserves the reflection link unless a new one is provided.
    ///
    /// Returns `Ok(false)` when the id does not exist or is already
    /// soft-deleted; `Ok(true)` when a row was updated.
    pub fn resolve_post(
        &self,
        post_id: &str,
        resolved_by_reflection_id: Option<&str>,
    ) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE reflections \
             SET resolved_at = ?1, \
                 resolved_by_reflection_id = COALESCE(?2, resolved_by_reflection_id), \
                 updated_at = ?1 \
             WHERE id = ?3 AND deleted_at IS NULL",
            rusqlite::params![&now, resolved_by_reflection_id, post_id],
        )?;
        Ok(rows > 0)
    }

    /// Retrieve active (non-archived) bullpen posts, ordered newest first.
    pub fn get_board_posts(&self) -> Result<Vec<Reflection>> {
        self.get_board_posts_filtered(false)
    }

    /// Like `get_board_posts` but with opt-in switches for past-TTL,
    /// non-evergreen posts (#376) and resolved threads (#362). Used by
    /// `legion bullpen --include-stale` / `--include-resolved` for operator
    /// review. Agents must never reach this with either set to `true`.
    pub fn get_board_posts_filtered(&self, include_stale: bool) -> Result<Vec<Reflection>> {
        self.get_board_posts_filtered_full(
            include_stale,
            false,
            &crate::timerange::TimeRange::default(),
        )
    }

    /// Full filter: independently opt into stale and resolved rows.
    /// `range` applies #786's `created_at` predicate directly in the WHERE
    /// clause (`TimeRange::default()` is unbounded, a no-op) -- the DB
    /// backer for `board::bullpen_filtered_with_decay`, which is how
    /// `legion bullpen --since/--until/--on` filters the listing.
    pub fn get_board_posts_filtered_full(
        &self,
        include_stale: bool,
        include_resolved: bool,
        range: &crate::timerange::TimeRange,
    ) -> Result<Vec<Reflection>> {
        // `now` is bound as `Option<String>` (None when include_stale) so
        // the decay clause's placeholder is ALWAYS present in the SQL
        // text at a fixed position -- letting the range clause's
        // placeholders start at a fixed index regardless of include_stale,
        // instead of branching the query text on it (#786).
        let now: Option<String> = if include_stale {
            None
        } else {
            Some(Utc::now().to_rfc3339())
        };
        let resolved_clause = if include_resolved {
            ""
        } else {
            " AND resolved_at IS NULL"
        };
        let range_clause = crate::timerange::TimeRange::sql_clause(2);
        let sql = format!(
            "SELECT {REFLECTION_COLUMNS} \
             FROM reflections WHERE {ACTIVE_TEAM_POST_WHERE} \
             AND (?1 IS NULL OR evergreen = 1 OR expires_at > ?1){resolved_clause}{range_clause} \
             ORDER BY created_at DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params![now, range.since_bound()?, range.until_bound()?],
            map_reflection_row,
        )?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Retrieve up to `limit` active bullpen posts strictly after the given
    /// `(created_at, id)` cursor, ordered oldest first. Used by the MCP
    /// notifier thread to discover cross-process writes (from the CLI
    /// `legion post` command or from another MCP subprocess) that would
    /// otherwise be invisible to an in-process broadcast channel.
    ///
    /// The composite `(created_at, id)` cursor is the tiebreaker for two
    /// posts that share an identical `created_at` timestamp. Strict `>` on
    /// `created_at` alone is wrong when combined with `limit`: if the batch
    /// cap splits a tied-timestamp group, the next poll's cursor advances
    /// past the shared timestamp and subsequent rows at that timestamp are
    /// lost. Ordering and filtering by `(created_at, id)` eliminates this
    /// by giving every row a totally-ordered position (UUIDv7 ids embed a
    /// monotonic timestamp, so ties on `created_at` are almost always
    /// broken by `id` ordering anyway).
    ///
    /// `limit` caps the size of a single batch: if more rows exist beyond
    /// the cap, the cursor advances to the last row returned and the next
    /// poll catches the remainder.
    pub fn get_board_posts_since(
        &self,
        since_created_at: &str,
        since_id: &str,
        limit: usize,
    ) -> Result<Vec<Reflection>> {
        // strict `>` on the composite key: `(created_at > ?1) OR
        // (created_at = ?1 AND id > ?2)`. Flipping either comparator to
        // inclusive re-emits the cursor row on every poll tick and
        // produces an infinite duplicate-notification loop.
        //
        // Decay filter (#376): the channel notification path is the most
        // expensive token-cost surface for stale posts, so the filter applies
        // here too. Backdated inserts past TTL are silently skipped instead of
        // pushed.
        let now = Utc::now().to_rfc3339();
        let sql = format!(
            "SELECT {REFLECTION_COLUMNS} \
             FROM reflections \
             WHERE {ACTIVE_TEAM_POST_WHERE} \
               AND (evergreen = 1 OR expires_at > ?4) \
               AND resolved_at IS NULL \
               AND (created_at > ?1 OR (created_at = ?1 AND id > ?2)) \
             ORDER BY created_at ASC, id ASC \
             LIMIT ?3"
        );
        let mut stmt = self.conn.prepare(&sql)?;

        let rows = stmt.query_map(
            rusqlite::params![since_created_at, since_id, limit as i64, now],
            map_reflection_row,
        )?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Return the `(created_at, id)` of the most recent active team bullpen
    /// post, or `None` when the board is empty. Used by the MCP notifier
    /// thread as its startup cursor: seeding from the actual watermark of
    /// the rows the notifier is filtering against means a post committed in
    /// the same nanosecond as the notifier's seed is not dropped by a
    /// wall-clock race, and a future change that allows backdated inserts
    /// into non-team audience does not silently shift the notifier's
    /// starting point.
    pub fn get_board_cursor_watermark(&self) -> Result<Option<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT created_at, id FROM reflections \
             WHERE audience = 'team' AND archived_at IS NULL AND deleted_at IS NULL \
             ORDER BY created_at DESC, id DESC \
             LIMIT 1",
        )?;
        let result = stmt
            .query_row([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .optional()
            .map_err(LegionError::Database)?;
        Ok(result)
    }

    /// Retrieve active bullpen posts unread by the given reader repo and
    /// atomically mark them as read. Posts created during the read are NOT
    /// marked, so they remain unread on the next call.
    ///
    /// Used by the channel backlog fetch so agents only see each post once.
    /// Race-safe: uses a single timestamp for both the SELECT filter upper
    /// bound and the mark_read UPDATE, inside a transaction.
    ///
    /// Fast path: when there are no unread posts, skips the INSERT entirely --
    /// every idle channel connect would otherwise pay a write-lock acquire on
    /// board_reads for no reason.
    pub fn get_and_mark_unread_board_posts(&self, reader_repo: &str) -> Result<Vec<Reflection>> {
        let now = Utc::now().to_rfc3339();

        let txn = self.conn.unchecked_transaction()?;

        let sql = format!(
            "SELECT {REFLECTION_COLUMNS} \
             FROM reflections \
             WHERE {ACTIVE_TEAM_POST_WHERE} \
             AND (evergreen = 1 OR expires_at > ?2) \
             AND resolved_at IS NULL \
             AND created_at > COALESCE( \
                 (SELECT last_read_at FROM board_reads WHERE reader_repo = ?1), \
                 '' \
             ) \
             AND created_at <= ?2 \
             ORDER BY created_at DESC"
        );
        let mut stmt = txn.prepare(&sql)?;

        let rows = stmt.query_map(rusqlite::params![reader_repo, &now], map_reflection_row)?;
        let posts: Vec<Reflection> = rows
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)?;

        drop(stmt);

        if posts.is_empty() {
            // Nothing to mark. Dropping the txn is a rollback of a pure read,
            // no write-lock pressure on board_reads.
            return Ok(posts);
        }

        // The WHERE guard on last_read_at is defensive against concurrent
        // writers or clock skew: if another process somehow wrote a later
        // timestamp between our SELECT and this UPDATE, we do not stomp it.
        // Under normal single-writer use this branch never fires, but it
        // preserves "last_read_at is monotonic non-decreasing" under any
        // future concurrent access.
        txn.execute(
            "INSERT INTO board_reads (reader_repo, last_read_at) VALUES (?1, ?2) \
             ON CONFLICT(reader_repo) DO UPDATE SET last_read_at = excluded.last_read_at \
             WHERE excluded.last_read_at > board_reads.last_read_at",
            rusqlite::params![reader_repo, &now],
        )?;

        txn.commit()?;

        Ok(posts)
    }

    /// Retrieve archived bullpen posts, ordered newest first.
    pub fn get_archived_posts(&self) -> Result<Vec<Reflection>> {
        let sql = format!(
            "SELECT {REFLECTION_COLUMNS} \
             FROM reflections WHERE audience = 'team' AND archived_at IS NOT NULL AND deleted_at IS NULL ORDER BY created_at DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;

        let rows = stmt.query_map([], map_reflection_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Archive bullpen posts that all known readers have read.
    ///
    /// A post is archivable when every repo in board_reads has last_read_at
    /// after the post's created_at. Uses a single UPDATE with subquery to
    /// avoid race conditions between SELECT and UPDATE.
    /// Returns the number of posts archived.
    pub fn archive_read_posts(&self) -> Result<u64> {
        let now = Utc::now().to_rfc3339();

        let count = self.conn.execute(
            "UPDATE reflections SET archived_at = ?1, updated_at = ?1 \
             WHERE audience = 'team' AND archived_at IS NULL AND deleted_at IS NULL \
             AND created_at < (SELECT MIN(last_read_at) FROM board_reads)",
            rusqlite::params![now],
        )?;

        Ok(count as u64)
    }

    /// Count team posts that are unread by the given reader repo.
    ///
    /// If the reader has no entry in board_reads, all team posts are unread.
    /// Only counts non-archived posts.
    pub fn get_unread_count(&self, reader_repo: &str) -> Result<u64> {
        let now = Utc::now().to_rfc3339();
        let sql = format!(
            "SELECT COUNT(*) FROM reflections WHERE {ACTIVE_TEAM_POST_WHERE} \
             AND (evergreen = 1 OR expires_at > ?2) \
             AND resolved_at IS NULL \
             AND created_at > COALESCE( \
                 (SELECT last_read_at FROM board_reads WHERE reader_repo = ?1), \
                 '' \
             )"
        );
        let mut stmt = self.conn.prepare(&sql)?;

        let count: u64 = stmt
            .query_row(rusqlite::params![reader_repo, &now], |row| row.get(0))
            .map_err(LegionError::Database)?;

        Ok(count)
    }
    /// Mark all current bullpen posts as read for the given reader repo.
    ///
    /// Upserts the board_reads row with the current timestamp.
    pub fn mark_board_read(&self, reader_repo: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();

        self.conn.execute(
            "INSERT INTO board_reads (reader_repo, last_read_at) VALUES (?1, ?2) \
             ON CONFLICT(reader_repo) DO UPDATE SET last_read_at = excluded.last_read_at",
            (reader_repo, &now),
        )?;

        Ok(())
    }

    /// Read the channel-delivery cursor for `reader_repo`.
    ///
    /// Used by the MCP notifier at cold boot so a session that was offline
    /// while a directed signal was filed picks it up on the first poll tick
    /// after it comes back online. Returns `(last_read_at, last_read_id)`,
    /// matching what `advance_board_read_cursor` writes. `last_read_id` is
    /// an empty string for rows written before the #400 migration.
    pub fn get_board_read_cursor(&self, reader_repo: &str) -> Result<Option<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT last_read_at, last_read_id FROM board_reads WHERE reader_repo = ?1")?;
        stmt.query_row([reader_repo], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .optional()
        .map_err(LegionError::Database)
    }

    /// Advance the channel-delivery cursor for `reader_repo` to
    /// `(last_read_at, last_read_id)`.
    ///
    /// Forward-only: the upsert refuses to move the cursor backwards. The
    /// composite ordering is `(last_read_at, last_read_id)`, which mirrors
    /// the strict-`>` comparator used by `get_board_posts_since` so a row
    /// recorded here is excluded from re-emission on the next poll.
    /// Called by the MCP notifier after each successful delivery.
    ///
    /// `last_read_at` MUST share the offset shape every other legion write
    /// path uses -- `chrono::Utc::now().to_rfc3339()`, which renders a
    /// `+00:00` suffix (NOT `Z`). Lexicographic SQL `>` is correct only when
    /// every timestamp it compares is rendered with the same fixed offset
    /// string; mixing `+00:00` with `Z` (or any other offset) would
    /// mis-order rows that compare correctly chronologically. Every caller
    /// in-tree obeys this convention; the contract is documented here so
    /// future callers do not need to rediscover the constraint.
    pub fn advance_board_read_cursor(
        &self,
        reader_repo: &str,
        last_read_at: &str,
        last_read_id: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO board_reads (reader_repo, last_read_at, last_read_id) \
             VALUES (?1, ?2, ?3) \
             ON CONFLICT(reader_repo) DO UPDATE SET \
                 last_read_at = excluded.last_read_at, \
                 last_read_id = excluded.last_read_id \
             WHERE excluded.last_read_at > board_reads.last_read_at \
                OR (excluded.last_read_at = board_reads.last_read_at \
                    AND excluded.last_read_id > board_reads.last_read_id)",
            (reader_repo, last_read_at, last_read_id),
        )?;
        Ok(())
    }

    /// Find unhandled signals directed at a specific repo.
    ///
    /// `names` is the full addressable name set for this repo: the repo's
    /// `recipient()` value plus any `broadcast_tags`. A signal is returned
    /// when its text starts with (or contains mid-string) `@<name>` for any
    /// name in `names`, OR when it starts with `@all` / `@everyone`. A
    /// trailing `:` after the name (`@<name>: ...`) is decoration and
    /// matches too, mirroring `signal::recipient_token` (#612).
    ///
    /// Uses the `watch_handled` table for per-repo dedup keyed on `repo_name`
    /// (not on recipient or tags), so `@all` / `@everyone` broadcasts wake
    /// each repo exactly once, and a tag-addressed signal wakes each tagged
    /// repo exactly once. `repo_name` is also the self-signal exclusion key.
    ///
    /// When `names` is empty the function returns an empty vec (nothing to
    /// match). When `names` has one entry it matches only that name plus the
    /// reserved broadcasts, preserving backward-compatible behavior.
    pub fn get_unhandled_signals_for_repo(
        &self,
        repo_name: &str,
        names: &[String],
        since: Option<&str>,
    ) -> Result<Vec<Reflection>> {
        if names.is_empty() {
            return Ok(Vec::new());
        }

        let now = Utc::now().to_rfc3339();

        // Build LIKE patterns for every addressable name plus the reserved
        // broadcast aliases (`all`, `everyone`). Pattern construction is
        // delegated to `signal::address_like_patterns` -- the SQL projection
        // of the single addressing rule (#612) -- so each name matches four
        // forms: "@name ", "%@name ", "@name: ", "%@name: " (the trailing
        // `:` is decoration that `signal::recipient_token` trims, and the
        // wake path must agree with the parser on that or a colon-suffixed
        // directed signal delivers live but never wakes).
        //
        // Known residual divergence from the parser, pinned by tests below:
        // every pattern requires a literal space after the (possibly
        // colon-suffixed) name, so "@name\nverb ..." parses as a valid
        // signal but does not wake. See signal::address_like_patterns.
        //
        // Parameters are bound positionally: pattern slots first (?1..?N),
        // then repo_name (watch_handled join + self-exclusion), now
        // (expires_at check), and optionally since_ts.
        let mut patterns: Vec<String> = Vec::with_capacity((names.len() + 2) * 4);
        for reserved in ["all", "everyone"] {
            patterns.extend(crate::signal::address_like_patterns(reserved));
        }
        for name in names {
            patterns.extend(crate::signal::address_like_patterns(name));
        }

        // Fixed params follow the pattern slots.
        let p_repo = patterns.len() + 1; // repo_name
        let p_now = patterns.len() + 2; // now (expires_at check)
        let p_since = patterns.len() + 3; // since_ts (optional)

        let since_clause = if since.is_some() {
            format!(" AND r.created_at > ?{}", p_since)
        } else {
            String::new()
        };

        // Assemble the WHERE text fragment: one LIKE per pattern slot.
        let text_filter = (1..=patterns.len())
            .map(|i| format!("r.text LIKE ?{}", i))
            .collect::<Vec<String>>()
            .join(" OR ");

        // REFLECTION_COLUMNS is unprefixed; the names resolve to the `r`
        // alias because watch_handled (signal_id, repo_name, handled_at)
        // shares no column names with reflections.
        let query = format!(
            "SELECT {REFLECTION_COLUMNS} \
             FROM reflections r \
             LEFT JOIN watch_handled wh ON wh.signal_id = r.id AND wh.repo_name = ?{repo_param} \
             WHERE r.audience = 'team' AND r.deleted_at IS NULL \
               AND (r.evergreen = 1 OR r.expires_at > ?{now_param}) \
               AND r.resolved_at IS NULL \
               AND wh.signal_id IS NULL \
               AND ({text_filter}) \
               AND r.repo != ?{repo_param}{since_clause} \
             ORDER BY r.created_at ASC",
            repo_param = p_repo,
            now_param = p_now,
            text_filter = text_filter,
            since_clause = since_clause,
        );

        let mut stmt = self.conn.prepare(&query)?;

        // Bind params in slot order: pattern slots (reserved broadcasts,
        // then per-name), then repo_name, now, and optionally since_ts.
        use rusqlite::types::ToSql;
        let mut params: Vec<Box<dyn ToSql>> = patterns
            .into_iter()
            .map(|p| Box::new(p) as Box<dyn ToSql>)
            .collect();
        params.push(Box::new(repo_name.to_string()));
        params.push(Box::new(now));

        // Optional since_ts.
        let since_owned: Option<String> = since.map(str::to_string);
        if let Some(ref ts) = since_owned {
            params.push(Box::new(ts.clone()));
        }

        let param_refs: Vec<&dyn ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let rows = stmt.query_map(param_refs.as_slice(), map_reflection_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }

    /// Delete stale watch_handled records older than a given timestamp.
    pub fn prune_watch_handled(&self, older_than: &str) -> Result<u64> {
        let rows = self.conn.execute(
            "DELETE FROM watch_handled WHERE handled_at < ?1",
            [older_than],
        )?;
        Ok(rows as u64)
    }

    /// Mark a signal as handled by a specific repo.
    ///
    /// Inserts into `watch_handled` so this signal will not be returned
    /// for this repo again. Works for both targeted and @all signals.
    pub fn mark_signal_handled_for_repo(&self, signal_id: &str, repo_name: &str) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "INSERT OR IGNORE INTO watch_handled (signal_id, repo_name, handled_at) \
             VALUES (?1, ?2, ?3)",
            rusqlite::params![signal_id, repo_name, &now],
        )?;
        Ok(rows > 0)
    }

    /// Get recent bullpen posts (within last N hours by default).
    ///
    /// `range` (#786), when bounded, OVERRIDES the `hours` RECENCY cutoff
    /// entirely rather than composing with it -- an explicit `--since`/
    /// `--until`/`--on` on `legion surface` replaces the hardcoded recency
    /// window, it does not narrow it further. `TimeRange::default()`
    /// (unbounded) preserves the pre-#786 `hours`-cutoff behavior exactly.
    ///
    /// The decay/TTL filter (`evergreen = 1 OR expires_at > now`, #376) is
    /// a SEPARATE axis from recency and applies in BOTH branches: a
    /// `--since 30d` query should still hide posts an agent would never
    /// see via the normal bullpen (matching `get_board_posts_filtered_full`'s
    /// `include_stale`-gated default), not silently resurface expired
    /// non-evergreen posts just because a date range was given.
    pub fn get_recent_board_posts(
        &self,
        hours: i64,
        range: &crate::timerange::TimeRange,
    ) -> Result<Vec<Reflection>> {
        if range.is_unbounded() {
            let now = Utc::now();
            let cutoff = (now - chrono::Duration::hours(hours)).to_rfc3339();
            let now_str = now.to_rfc3339();
            let sql = format!(
                "SELECT {REFLECTION_COLUMNS} \
                 FROM reflections WHERE {ACTIVE_TEAM_POST_WHERE} \
                 AND (evergreen = 1 OR expires_at > ?2) \
                 AND resolved_at IS NULL \
                 AND created_at > ?1 ORDER BY created_at DESC"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map([&cutoff, &now_str], map_reflection_row)?;
            return rows
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(LegionError::Database);
        }

        let now_str = Utc::now().to_rfc3339();
        let range_clause = crate::timerange::TimeRange::sql_clause(2);
        let sql = format!(
            "SELECT {REFLECTION_COLUMNS} \
             FROM reflections WHERE {ACTIVE_TEAM_POST_WHERE} \
             AND (evergreen = 1 OR expires_at > ?1) \
             AND resolved_at IS NULL{range_clause} ORDER BY created_at DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params![now_str, range.since_bound()?, range.until_bound()?],
            map_reflection_row,
        )?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(LegionError::Database)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::db::testutil::test_db;
    use uuid::Uuid;

    #[test]
    fn get_board_posts_returns_only_team() {
        let db = test_db();
        db.insert_reflection("kelex", "private note", "self")
            .unwrap();
        db.insert_reflection("rafters", "shared insight", "team")
            .unwrap();
        let posts = db.get_board_posts().unwrap();
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].audience, "team");
    }

    #[test]
    fn get_board_posts_since_excludes_cursor_row_and_self_posts() {
        let db = test_db();

        // Insert three rows with increasing created_at: older, middle, newer.
        // Use insert_reflection so created_at is a real ISO 8601 stamp.
        let older = db.insert_reflection("kelex", "old", "team").unwrap();
        // Private reflection between team posts -- must NOT appear.
        db.insert_reflection("kelex", "not shared", "self").unwrap();
        let newer = db.insert_reflection("rafters", "new", "team").unwrap();

        // Cursor at `(older.created_at, older.id)` must return only `newer`
        // (strict `>` on the composite key).
        let batch = db
            .get_board_posts_since(&older.created_at, &older.id, 100)
            .expect("query");
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].id, newer.id);
        assert_eq!(batch[0].audience, "team");
    }

    #[test]
    fn get_board_posts_since_breaks_ties_on_id_component() {
        // Two posts with an identical `created_at` must still be visited in
        // deterministic order by id, and splitting a tied group with a tight
        // LIMIT must not lose rows: the cursor advances to `(created_at, id)`
        // of the last row returned, and the next query finds the tied-but-
        // higher-id row via the `created_at = ? AND id > ?` branch.
        let db = test_db();
        let shared_ts = "2026-04-11T12:00:00.000000000+00:00";

        // Insert two rows with IDENTICAL created_at (bypassing insert_reflection
        // so we can force the timestamp collision). UUIDv7 ids naturally sort
        // in the same order they are generated, so order_a < order_b.
        let id_a = "01000000-0000-7000-8000-000000000001";
        let id_b = "01000000-0000-7000-8000-000000000002";

        // expires_at far in the future so the #376 decay filter does not
        // exclude these rows. The test predates the decay column.
        let far_future = "2099-01-01T00:00:00+00:00";
        db.conn
            .execute(
                "INSERT INTO reflections (id, repo, text, created_at, audience, expires_at) \
                 VALUES (?1, 'kelex', 'tied A', ?2, 'team', ?3)",
                rusqlite::params![id_a, shared_ts, far_future],
            )
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO reflections (id, repo, text, created_at, audience, expires_at) \
                 VALUES (?1, 'kelex', 'tied B', ?2, 'team', ?3)",
                rusqlite::params![id_b, shared_ts, far_future],
            )
            .unwrap();

        // First batch with limit=1 must return id_a and advance the cursor
        // to `(shared_ts, id_a)`.
        let batch = db
            .get_board_posts_since("2026-04-11T00:00:00+00:00", "", 1)
            .expect("query");
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].id, id_a);

        // Second batch using the advanced cursor must find id_b via the
        // tiebreaker branch -- this is the regression guard against the
        // "tied timestamp + strict > on created_at alone = silent row drop"
        // bug surfaced in PR #222 review.
        let batch2 = db
            .get_board_posts_since(shared_ts, id_a, 10)
            .expect("query");
        assert_eq!(batch2.len(), 1);
        assert_eq!(batch2[0].id, id_b);
    }

    #[test]
    fn get_board_posts_since_honors_limit_and_ordering() {
        let db = test_db();

        // Seed a sentinel row so we have a stable pre-cursor.
        let sentinel = db.insert_reflection("seed", "sentinel", "team").unwrap();

        // Insert five team posts after the sentinel. insert_reflection writes
        // `created_at = Utc::now().to_rfc3339()` with nanosecond precision
        // via chrono; if the OS clock resolution ever collapses sequential
        // inserts into identical stamps, the composite cursor (covered by
        // `get_board_posts_since_breaks_ties_on_id_component`) still keeps
        // this test deterministic via UUIDv7 ordering.
        let mut expected_ids = Vec::new();
        for i in 0..5 {
            let r = db
                .insert_reflection("kelex", &format!("msg {i}"), "team")
                .unwrap();
            expected_ids.push(r.id);
        }

        // Request at most 3 rows after the sentinel -- must return the first
        // three in insertion order.
        let batch = db
            .get_board_posts_since(&sentinel.created_at, &sentinel.id, 3)
            .expect("query");
        assert_eq!(batch.len(), 3);
        assert_eq!(batch[0].id, expected_ids[0]);
        assert_eq!(batch[1].id, expected_ids[1]);
        assert_eq!(batch[2].id, expected_ids[2]);

        // Advance the cursor to the last row returned; the next call returns
        // the remaining two.
        let next = db
            .get_board_posts_since(&batch[2].created_at, &batch[2].id, 3)
            .expect("query");
        assert_eq!(next.len(), 2);
        assert_eq!(next[0].id, expected_ids[3]);
        assert_eq!(next[1].id, expected_ids[4]);

        // Cursor at the newest row returns an empty batch (idle poll path).
        let idle = db
            .get_board_posts_since(&next[1].created_at, &next[1].id, 10)
            .expect("query");
        assert!(idle.is_empty());
    }

    #[test]
    fn get_board_posts_since_excludes_archived() {
        // The query must filter out archived_at rows so a bullpen archive
        // pass does not re-notify every archived post on next startup.
        let db = test_db();
        let live = db.insert_reflection("kelex", "live", "team").unwrap();
        let _archived = db
            .insert_reflection("kelex", "will archive", "team")
            .unwrap();

        // Archive the second row directly (no public archive helper for a
        // single id -- the test exercises the invariant the SQL relies on).
        db.conn
            .execute(
                "UPDATE reflections SET archived_at = '2026-04-11T00:00:00+00:00' WHERE text = 'will archive'",
                [],
            )
            .unwrap();

        // Cursor from the very beginning should still return only the live
        // row.
        let batch = db
            .get_board_posts_since("2026-01-01T00:00:00+00:00", "", 100)
            .expect("query");
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].id, live.id);
    }

    #[test]
    fn get_board_cursor_watermark_empty_and_populated() {
        let db = test_db();

        // Empty table -> None.
        assert!(db.get_board_cursor_watermark().unwrap().is_none());

        // Single self reflection -> None (watermark is team-only).
        db.insert_reflection("kelex", "private", "self").unwrap();
        assert!(db.get_board_cursor_watermark().unwrap().is_none());

        // Add a team post -> watermark is that row.
        let team = db.insert_reflection("kelex", "shared", "team").unwrap();
        let watermark = db.get_board_cursor_watermark().unwrap();
        assert_eq!(watermark, Some((team.created_at.clone(), team.id.clone())));

        // Add a newer team post -> watermark advances.
        let newer = db.insert_reflection("rafters", "shared 2", "team").unwrap();
        let watermark = db.get_board_cursor_watermark().unwrap();
        assert_eq!(watermark, Some((newer.created_at, newer.id)));
    }

    #[test]
    fn unread_count_all_unread_when_no_reads() {
        let db = test_db();
        db.insert_reflection("rafters", "post 1", "team").unwrap();
        db.insert_reflection("kelex", "post 2", "team").unwrap();
        assert_eq!(db.get_unread_count("legion").unwrap(), 2);
    }

    #[test]
    fn mark_board_read_resets_unread_count() {
        let db = test_db();
        db.insert_reflection("rafters", "old post", "team").unwrap();
        db.mark_board_read("kelex").unwrap();
        assert_eq!(db.get_unread_count("kelex").unwrap(), 0);
    }

    #[test]
    fn get_and_mark_unread_delivers_once() {
        let db = test_db();
        db.insert_reflection("rafters", "post 1", "team").unwrap();
        db.insert_reflection("kelex", "post 2", "team").unwrap();

        // First call delivers both posts.
        let first = db.get_and_mark_unread_board_posts("legion").unwrap();
        assert_eq!(first.len(), 2);

        // Second call delivers nothing -- they were marked read.
        let second = db.get_and_mark_unread_board_posts("legion").unwrap();
        assert!(
            second.is_empty(),
            "expected empty on second call, got {}",
            second.len()
        );
    }

    #[test]
    fn get_and_mark_unread_delivers_new_posts_after_mark() {
        let db = test_db();
        db.insert_reflection("rafters", "first", "team").unwrap();

        // Read the first post -- marks as read.
        let first = db.get_and_mark_unread_board_posts("legion").unwrap();
        assert_eq!(first.len(), 1);

        // New post arrives after the read.
        db.insert_reflection("kelex", "second", "team").unwrap();

        // Second call delivers only the new post.
        let second = db.get_and_mark_unread_board_posts("legion").unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].text, "second");
    }

    #[test]
    fn get_and_mark_unread_is_per_reader() {
        let db = test_db();
        db.insert_reflection("rafters", "post", "team").unwrap();

        // legion reads -- marked for legion only.
        let legion_posts = db.get_and_mark_unread_board_posts("legion").unwrap();
        assert_eq!(legion_posts.len(), 1);

        // kelex still sees it as unread.
        let kelex_posts = db.get_and_mark_unread_board_posts("kelex").unwrap();
        assert_eq!(kelex_posts.len(), 1);
    }

    #[test]
    fn get_board_posts_ordered_newest_first() {
        let db = test_db();
        db.insert_reflection("kelex", "first post", "team").unwrap();
        db.insert_reflection("rafters", "second post", "team")
            .unwrap();
        let posts = db.get_board_posts().unwrap();
        assert_eq!(posts.len(), 2);
        // Newest first means second post should be first in results
        assert_eq!(posts[0].text, "second post");
        assert_eq!(posts[1].text, "first post");
    }

    #[test]
    fn mark_board_read_is_idempotent() {
        let db = test_db();
        db.insert_reflection("rafters", "a post", "team").unwrap();
        db.mark_board_read("kelex").unwrap();
        db.mark_board_read("kelex").unwrap();
        assert_eq!(db.get_unread_count("kelex").unwrap(), 0);
    }

    #[test]
    fn get_board_read_cursor_none_for_fresh_repo() {
        let db = test_db();
        assert!(db.get_board_read_cursor("kessel").unwrap().is_none());
    }

    #[test]
    fn advance_board_read_cursor_creates_then_moves_forward() {
        let db = test_db();
        db.advance_board_read_cursor("kessel", "2026-05-04T01:00:00+00:00", "id-a")
            .unwrap();
        assert_eq!(
            db.get_board_read_cursor("kessel").unwrap(),
            Some(("2026-05-04T01:00:00+00:00".to_string(), "id-a".to_string()))
        );

        // Forward move accepted.
        db.advance_board_read_cursor("kessel", "2026-05-04T02:00:00+00:00", "id-b")
            .unwrap();
        assert_eq!(
            db.get_board_read_cursor("kessel").unwrap(),
            Some(("2026-05-04T02:00:00+00:00".to_string(), "id-b".to_string()))
        );
    }

    #[test]
    fn advance_board_read_cursor_refuses_backward_move() {
        let db = test_db();
        db.advance_board_read_cursor("kessel", "2026-05-04T02:00:00+00:00", "id-b")
            .unwrap();
        // Older timestamp must not overwrite.
        db.advance_board_read_cursor("kessel", "2026-05-04T01:00:00+00:00", "id-a")
            .unwrap();
        assert_eq!(
            db.get_board_read_cursor("kessel").unwrap(),
            Some(("2026-05-04T02:00:00+00:00".to_string(), "id-b".to_string()))
        );
    }

    #[test]
    fn advance_board_read_cursor_breaks_tie_on_id() {
        let db = test_db();
        let ts = "2026-05-04T02:00:00+00:00";
        db.advance_board_read_cursor("kessel", ts, "id-a").unwrap();
        // Same timestamp, larger id -> accepted.
        db.advance_board_read_cursor("kessel", ts, "id-b").unwrap();
        assert_eq!(
            db.get_board_read_cursor("kessel").unwrap(),
            Some((ts.to_string(), "id-b".to_string()))
        );
        // Same timestamp, smaller id -> refused.
        db.advance_board_read_cursor("kessel", ts, "id-a").unwrap();
        assert_eq!(
            db.get_board_read_cursor("kessel").unwrap(),
            Some((ts.to_string(), "id-b".to_string()))
        );
    }

    #[test]
    fn unread_count_tracks_new_posts_after_read() {
        let db = test_db();
        db.insert_reflection("rafters", "old post", "team").unwrap();
        db.mark_board_read("kelex").unwrap();
        assert_eq!(db.get_unread_count("kelex").unwrap(), 0);

        // New post after marking read should be unread
        // Small sleep to ensure timestamp differs
        std::thread::sleep(std::time::Duration::from_millis(10));
        db.insert_reflection("platform", "new post", "team")
            .unwrap();
        assert_eq!(db.get_unread_count("kelex").unwrap(), 1);
    }

    // -- Bullpen decay (#376) --------------------------------------------

    fn t_now() -> chrono::DateTime<Utc> {
        Utc::now()
    }

    #[test]
    fn ttl_default_post_is_48_hours() {
        let (e, ev) = compute_post_ttl("team", None, None, "broadcast", t_now());
        assert!(!ev);
        let exp = chrono::DateTime::parse_from_rfc3339(&e.expect("expires_at")).unwrap();
        let delta = exp.signed_duration_since(t_now()).num_hours();
        assert!(
            (47..=48).contains(&delta),
            "default TTL must be 48h, got {delta}"
        );
    }

    #[test]
    fn ttl_signal_post_is_7_days() {
        let (e, _) = compute_post_ttl("team", None, None, "@smugglr review:requested", t_now());
        let exp = chrono::DateTime::parse_from_rfc3339(&e.expect("expires_at")).unwrap();
        let hours = exp.signed_duration_since(t_now()).num_hours();
        assert!(
            (167..=168).contains(&hours),
            "signal TTL must be 7d (~168h), got {hours}"
        );
    }

    #[test]
    fn ttl_design_tag_is_14_days() {
        let (e, _) = compute_post_ttl(
            "team",
            None,
            Some("design,blueprint"),
            "design proposal",
            t_now(),
        );
        let exp = chrono::DateTime::parse_from_rfc3339(&e.expect("expires_at")).unwrap();
        let hours = exp.signed_duration_since(t_now()).num_hours();
        assert!(
            (335..=336).contains(&hours),
            "design TTL must be 14d (~336h), got {hours}"
        );
    }

    #[test]
    fn ttl_identity_domain_is_evergreen() {
        let (e, ev) = compute_post_ttl("team", Some("identity"), None, "who I am", t_now());
        assert_eq!(e, None, "evergreen rows have no expires_at");
        assert!(ev);
    }

    #[test]
    fn ttl_doctrine_tag_is_evergreen() {
        let (e, ev) = compute_post_ttl("team", None, Some("doctrine"), "doctrine post", t_now());
        assert_eq!(e, None);
        assert!(ev);
    }

    #[test]
    fn ttl_non_team_audience_is_skipped() {
        let (e, ev) = compute_post_ttl("self", None, None, "private", t_now());
        assert_eq!(e, None);
        assert!(!ev);
    }

    #[test]
    fn get_board_posts_filters_expired() {
        let db = test_db();
        // Fresh team post -- visible.
        db.insert_reflection("kelex", "fresh broadcast", "team")
            .unwrap();
        // Manually-aged team post (created 10 days ago, expires 48h after that).
        let aged = "2026-04-15T00:00:00+00:00";
        let aged_exp = "2026-04-17T00:00:00+00:00";
        db.conn
            .execute(
                "INSERT INTO reflections (id, repo, text, created_at, audience, expires_at, evergreen) \
                 VALUES (?1, 'kelex', 'stale post', ?2, 'team', ?3, 0)",
                rusqlite::params![Uuid::now_v7().to_string(), aged, aged_exp],
            )
            .unwrap();

        let posts = db.get_board_posts().unwrap();
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].text, "fresh broadcast");
    }

    #[test]
    fn get_board_posts_include_stale_returns_expired() {
        let db = test_db();
        let aged = "2026-04-15T00:00:00+00:00";
        let aged_exp = "2026-04-17T00:00:00+00:00";
        db.conn
            .execute(
                "INSERT INTO reflections (id, repo, text, created_at, audience, expires_at, evergreen) \
                 VALUES (?1, 'kelex', 'stale post', ?2, 'team', ?3, 0)",
                rusqlite::params![Uuid::now_v7().to_string(), aged, aged_exp],
            )
            .unwrap();
        let active = db.get_board_posts_filtered(false).unwrap();
        assert!(active.is_empty());
        let all = db.get_board_posts_filtered(true).unwrap();
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn get_board_posts_keeps_evergreen() {
        let db = test_db();
        let aged = "2026-01-01T00:00:00+00:00";
        db.conn
            .execute(
                "INSERT INTO reflections (id, repo, text, created_at, audience, expires_at, evergreen) \
                 VALUES (?1, 'kelex', 'doctrine post', ?2, 'team', NULL, 1)",
                rusqlite::params![Uuid::now_v7().to_string(), aged],
            )
            .unwrap();
        let posts = db.get_board_posts().unwrap();
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].text, "doctrine post");
    }

    #[test]
    fn get_board_posts_since_filters_expired() {
        let db = test_db();
        let aged = "2026-04-15T00:00:00+00:00";
        let aged_exp = "2026-04-17T00:00:00+00:00";
        db.conn
            .execute(
                "INSERT INTO reflections (id, repo, text, created_at, audience, expires_at, evergreen) \
                 VALUES (?1, 'kelex', 'stale signal', ?2, 'team', ?3, 0)",
                rusqlite::params![Uuid::now_v7().to_string(), aged, aged_exp],
            )
            .unwrap();
        // Cursor before the aged row -- with decay filter, batch must be empty.
        let batch = db
            .get_board_posts_since("2026-01-01T00:00:00+00:00", "", 100)
            .unwrap();
        assert!(batch.is_empty(), "stale post must not push notifications");
    }

    #[test]
    fn get_unhandled_signals_filters_expired() {
        let db = test_db();
        let aged = "2026-04-15T00:00:00+00:00";
        let aged_exp = "2026-04-17T00:00:00+00:00";
        db.conn
            .execute(
                "INSERT INTO reflections (id, repo, text, created_at, audience, expires_at, evergreen) \
                 VALUES (?1, 'platform', '@kessel reply please ', ?2, 'team', ?3, 0)",
                rusqlite::params![Uuid::now_v7().to_string(), aged, aged_exp],
            )
            .unwrap();
        let signals = db
            .get_unhandled_signals_for_repo("kessel", &["kessel".to_string()], None)
            .unwrap();
        assert!(
            signals.is_empty(),
            "wake loop must not deliver stale @kessel signals"
        );
    }

    // -- Addressing-rule parity at the wake query (#612) ------------------

    #[test]
    fn colon_suffixed_directed_signal_is_returned() {
        // '@kessel: ...' delivers on the live channel (the parser trims the
        // trailing ':'); pre-#612 it never matched the wake LIKE patterns,
        // so it delivered live but woke nobody. The wake query must agree
        // with signal::recipient_token on the colon rule.
        let db = test_db();
        db.insert_reflection("platform", "@kessel: question -- can you review?", "team")
            .unwrap();
        let signals = db
            .get_unhandled_signals_for_repo("kessel", &["kessel".to_string()], None)
            .unwrap();
        assert_eq!(
            signals.len(),
            1,
            "colon-suffixed directed signal must reach the wake path"
        );
    }

    #[test]
    fn colon_suffixed_broadcast_is_returned() {
        let db = test_db();
        db.insert_reflection("platform", "@all: announce -- shipped", "team")
            .unwrap();
        let signals = db
            .get_unhandled_signals_for_repo("kessel", &["kessel".to_string()], None)
            .unwrap();
        assert_eq!(
            signals.len(),
            1,
            "colon-suffixed @all broadcast must reach the wake path"
        );
    }

    #[test]
    fn colon_suffix_does_not_widen_to_prefix_names() {
        // '@kesseltwo: ...' must not match the 'kessel' patterns: the ':'
        // variants still require the full name before the colon.
        let db = test_db();
        db.insert_reflection("platform", "@kesseltwo: question -- not for kessel", "team")
            .unwrap();
        let signals = db
            .get_unhandled_signals_for_repo("kessel", &["kessel".to_string()], None)
            .unwrap();
        assert!(
            signals.is_empty(),
            "colon patterns must not match a longer name sharing the prefix"
        );
    }

    #[test]
    fn newline_after_name_does_not_wake_pinned_divergence() {
        // Documented residual divergence from signal::recipient_token (see
        // signal::address_like_patterns): the LIKE patterns require a
        // literal space after the name, so a newline-delimited signal
        // parses as addressed but does not wake. Pinned here so any change
        // to that behavior is a conscious one.
        let db = test_db();
        db.insert_reflection("platform", "@kessel\nquestion -- review?", "team")
            .unwrap();
        assert!(crate::signal::is_addressed_to(
            "@kessel\nquestion -- review?",
            "kessel"
        ));
        let signals = db
            .get_unhandled_signals_for_repo("kessel", &["kessel".to_string()], None)
            .unwrap();
        assert!(
            signals.is_empty(),
            "newline-after-name is the accepted parser/SQL divergence"
        );
    }

    #[test]
    fn insert_team_post_sets_decay_metadata() {
        let db = test_db();
        let r = db
            .insert_reflection("kelex", "fresh broadcast", "team")
            .unwrap();
        let (expires_at, evergreen): (Option<String>, i32) = db
            .conn
            .query_row(
                "SELECT expires_at, evergreen FROM reflections WHERE id = ?1",
                [&r.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(expires_at.is_some(), "team post must get expires_at");
        assert_eq!(evergreen, 0);
    }

    // -- Resolution marker (#362) ----------------------------------------

    #[test]
    fn resolve_post_marks_row_resolved() {
        let db = test_db();
        let r = db
            .insert_reflection("kelex", "fresh broadcast", "team")
            .unwrap();
        let updated = db.resolve_post(&r.id, None).unwrap();
        assert!(updated);

        let resolved_at: Option<String> = db
            .conn
            .query_row(
                "SELECT resolved_at FROM reflections WHERE id = ?1",
                [&r.id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(resolved_at.is_some(), "resolved_at must be populated");
    }

    #[test]
    fn resolve_post_links_reflection_id() {
        let db = test_db();
        let r = db
            .insert_reflection("kelex", "fresh broadcast", "team")
            .unwrap();
        let conclusion = db
            .insert_reflection("kelex", "team agreed on X", "self")
            .unwrap();
        db.resolve_post(&r.id, Some(&conclusion.id)).unwrap();

        let linked: Option<String> = db
            .conn
            .query_row(
                "SELECT resolved_by_reflection_id FROM reflections WHERE id = ?1",
                [&r.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(linked, Some(conclusion.id));
    }

    #[test]
    fn resolve_post_returns_false_for_missing_id() {
        let db = test_db();
        let updated = db
            .resolve_post("00000000-0000-0000-0000-000000000000", None)
            .unwrap();
        assert!(!updated);
    }

    #[test]
    fn get_board_posts_filters_resolved() {
        let db = test_db();
        let kept = db
            .insert_reflection("kelex", "open thread", "team")
            .unwrap();
        let r = db
            .insert_reflection("kelex", "converged thread", "team")
            .unwrap();
        db.resolve_post(&r.id, None).unwrap();

        let posts = db.get_board_posts().unwrap();
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].id, kept.id);
    }

    #[test]
    fn get_board_posts_include_resolved_returns_resolved() {
        let db = test_db();
        let r = db
            .insert_reflection("kelex", "converged thread", "team")
            .unwrap();
        db.resolve_post(&r.id, None).unwrap();
        let active = db
            .get_board_posts_filtered_full(false, false, &crate::timerange::TimeRange::default())
            .unwrap();
        assert!(active.is_empty());
        let with_resolved = db
            .get_board_posts_filtered_full(false, true, &crate::timerange::TimeRange::default())
            .unwrap();
        assert_eq!(with_resolved.len(), 1);
    }

    #[test]
    fn get_recent_board_posts_range_overrides_hours_cutoff() {
        // A post created well outside the `hours` recency window must
        // still surface when an explicit --since/--until range covers it
        // (#786): the range OVERRIDES the hardcoded cutoff, it does not
        // narrow it.
        let db = test_db();
        let old_ts = "2020-01-01T12:00:00+00:00";
        let far_future_expiry = "2099-01-01T00:00:00+00:00";
        db.conn
            .execute(
                "INSERT INTO reflections (id, repo, text, created_at, audience, expires_at) \
                 VALUES ('01000000-0000-7000-8000-00000000000a', 'kelex', 'ancient post', ?1, 'team', ?2)",
                rusqlite::params![old_ts, far_future_expiry],
            )
            .unwrap();

        // Default 24h lookback: the ancient post is invisible.
        let recent = db
            .get_recent_board_posts(24, &crate::timerange::TimeRange::default())
            .unwrap();
        assert!(recent.is_empty());

        // Explicit range covering 2020-01-01: the same post surfaces.
        let range =
            crate::timerange::TimeRange::parse(Some("2019-12-31"), Some("2020-01-02"), None)
                .unwrap();
        let ranged = db.get_recent_board_posts(24, &range).unwrap();
        assert_eq!(ranged.len(), 1);
        assert_eq!(ranged[0].text, "ancient post");
    }

    #[test]
    fn get_recent_board_posts_bounded_range_still_excludes_expired_posts() {
        // The decay/TTL filter (#376) is a separate axis from recency and
        // must still apply when an explicit range is given -- an explicit
        // --since must not resurrect posts the normal (unbounded) bullpen
        // already treats as expired.
        let db = test_db();
        let old_ts = "2020-01-01T12:00:00+00:00";
        let already_expired = "2020-01-02T00:00:00+00:00";
        db.conn
            .execute(
                "INSERT INTO reflections (id, repo, text, created_at, audience, expires_at) \
                 VALUES ('01000000-0000-7000-8000-00000000000b', 'kelex', 'stale post', ?1, 'team', ?2)",
                rusqlite::params![old_ts, already_expired],
            )
            .unwrap();

        let range =
            crate::timerange::TimeRange::parse(Some("2019-12-31"), Some("2020-01-02"), None)
                .unwrap();
        let ranged = db.get_recent_board_posts(24, &range).unwrap();
        assert!(
            ranged.is_empty(),
            "an explicit date range must not bypass the #376 decay/TTL filter"
        );
    }

    #[test]
    fn get_board_posts_since_skips_resolved() {
        let db = test_db();
        let r = db
            .insert_reflection("kelex", "converged thread", "team")
            .unwrap();
        db.resolve_post(&r.id, None).unwrap();
        let batch = db
            .get_board_posts_since("2026-01-01T00:00:00+00:00", "", 100)
            .unwrap();
        assert!(batch.is_empty(), "channel must not push resolved posts");
    }

    #[test]
    fn get_unhandled_signals_skips_resolved() {
        let db = test_db();
        let r = db
            .insert_reflection("platform", "@kessel needs answer ", "team")
            .unwrap();
        db.resolve_post(&r.id, None).unwrap();
        let signals = db
            .get_unhandled_signals_for_repo("kessel", &["kessel".to_string()], None)
            .unwrap();
        assert!(
            signals.is_empty(),
            "wake loop must not deliver resolved signals"
        );
    }

    #[test]
    fn get_unread_count_skips_resolved() {
        let db = test_db();
        let r = db.insert_reflection("kelex", "converged", "team").unwrap();
        db.resolve_post(&r.id, None).unwrap();
        let count = db.get_unread_count("smugglr").unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn insert_self_reflection_skips_decay() {
        let db = test_db();
        let r = db
            .insert_reflection("kelex", "private note", "self")
            .unwrap();
        let (expires_at, evergreen): (Option<String>, i32) = db
            .conn
            .query_row(
                "SELECT expires_at, evergreen FROM reflections WHERE id = ?1",
                [&r.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(expires_at, None, "self audience never expires");
        assert_eq!(evergreen, 0);
    }
}
