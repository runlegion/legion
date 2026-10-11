//! Cluster sync on smugglr-core's masterless multicast gossip (#1397).
//!
//! Runs in a background thread with its own tokio runtime. One loop
//! multicasts a `primary_key -> content_hash` digest of every synced table
//! every `interval_secs`, and handles every datagram that arrives in between:
//! a node that hears a digest covering rows it lacks asks for them, and
//! whoever holds them answers. Late joiners and partition rejoins converge
//! through that same path. Membership is possession of the cluster secret;
//! every datagram is XChaCha20-Poly1305 sealed by smugglr.
//!
//! Wire change: nodes on 0.43.x (legion's own delta transport) and this
//! build do not talk to each other. Every host upgrades together.

use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use smugglr_core::broadcast::BroadcastConfig;
use smugglr_core::config::{Config, ConflictResolution, SyncConfig};
use smugglr_core::multicast::{DEFAULT_GROUP, Gossip, GossipEvent, RECV_BUF};
use smugglr_core::{LocalDb, daemon};
use tokio::runtime::Runtime;

use crate::cluster::ClusterConfig;
use crate::db::Database;
use crate::error::{LegionError, Result};

/// The tables gossip replicates. Every other table stays local.
const SYNCED_TABLES: [&str; 4] = [
    "reflections",
    "schedules",
    "persona_wake_leases",
    "host_status",
];

/// Columns left out of the content hash, so a difference in them alone
/// never makes a peer ask for the row:
///
/// - `embedding`: derived from `text` by the local model; a BLOB that
///   smugglr would otherwise compare byte for byte.
/// - `recall_count`, `last_recalled_at`: per-node usage counters, bumped on
///   every local recall.
/// - `expires_at`, `evergreen`: bullpen TTL, recomputed by a local backfill
///   (`db::board`) that does not bump `updated_at`.
///
/// smugglr 0.5 still ships these columns when it serves a row, so a pulled
/// row carries the sender's values; see [`restore_gossiped_embeddings`].
const EXCLUDED_COLUMNS: [&str; 5] = [
    "embedding",
    "recall_count",
    "last_recalled_at",
    "expires_at",
    "evergreen",
];

/// The `newer_wins` ordering key: `max(created_at, updated_at, deleted_at)`,
/// so a tombstone that stamps only `deleted_at` still wins over a live row.
const ORDERING_COLUMNS: [&str; 3] = ["created_at", "updated_at", "deleted_at"];

/// Handle for communicating with the sync actor.
pub struct SyncHandle {
    /// Signal the actor to stop.
    _stop_tx: mpsc::Sender<()>,
    /// Thread handle for joining.
    _thread: thread::JoinHandle<()>,
}

/// Load `cluster.toml` and spawn the sync actor when sync is enabled and a
/// secret is configured. The single spawn gate shared by `legion watch` and
/// the daemon's watch task. Sync is optional and never fatal: config errors
/// and spawn failures log under `log_prefix` and return None so the caller's
/// loop runs regardless.
pub fn spawn_sync_if_enabled(data_dir: &Path, log_prefix: &str) -> Option<SyncHandle> {
    match ClusterConfig::load(data_dir) {
        Ok(cc) if cc.enabled && cc.secret.is_some() => {
            eprintln!(
                "{log_prefix} cluster sync enabled on port {} (instance: {})",
                cc.port,
                crate::host::hostname()
            );
            match SyncHandle::spawn(data_dir, cc) {
                Ok(handle) => Some(handle),
                Err(e) => {
                    eprintln!("{log_prefix} failed to start sync actor: {e}");
                    None
                }
            }
        }
        Ok(_) => None,
        Err(e) => {
            eprintln!("{log_prefix} cluster config error (sync disabled): {e}");
            None
        }
    }
}

impl SyncHandle {
    /// Spawn the sync actor in a background thread.
    ///
    /// Refuses a config without a secret: smugglr's `Gossip` only warns on
    /// a keyless bind, and a keyless node applies rows from any host on the
    /// subnet.
    pub fn spawn(data_dir: &Path, config: ClusterConfig) -> Result<Self> {
        let broadcast = broadcast_config(&config);
        broadcast
            .encryption_key()
            .map_err(|e| LegionError::Config(format!("sync actor key decode failed: {e}")))?
            .ok_or_else(|| {
                LegionError::Config(
                    "sync requires a cluster secret; refusing to run unencrypted".into(),
                )
            })?;

        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let data_dir = data_dir.to_path_buf();

        let thread = thread::spawn(move || {
            let rt = match Runtime::new() {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("[legion sync] failed to create tokio runtime: {e}");
                    return;
                }
            };
            rt.block_on(run_sync_loop(&data_dir, &broadcast, &stop_rx));
        });

        Ok(Self {
            _stop_tx: stop_tx,
            _thread: thread,
        })
    }
}

/// The gossip node's identity, key, and apply policy.
fn broadcast_config(cc: &ClusterConfig) -> BroadcastConfig {
    BroadcastConfig {
        port: cc.port,
        interval_secs: cc.interval_secs,
        instance_id: Some(crate::host::hostname()),
        secret: cc.secret.clone(),
        conflict_resolution: ConflictResolution::NewerWins,
        ordering_columns: ORDERING_COLUMNS.iter().map(|c| (*c).to_string()).collect(),
    }
}

/// The smugglr config gossip reads: which tables sync and which columns
/// stay out of the content hash.
fn gossip_config(db_path: &Path) -> Config {
    Config {
        cloudflare_account_id: None,
        cloudflare_api_token: None,
        database_id: None,
        local_db: Some(db_path.to_string_lossy().to_string()),
        sync: SyncConfig {
            tables: SYNCED_TABLES.iter().map(|t| (*t).to_string()).collect(),
            exclude_columns: EXCLUDED_COLUMNS.iter().map(|c| (*c).to_string()).collect(),
            ..SyncConfig::default()
        },
        stash: None,
        target: None,
        broadcast: None,
    }
}

/// Turn every embedding a peer delivered as hex TEXT back into its BLOB.
///
/// smugglr 0.5 serves a requested row with every column and encodes a BLOB
/// as lowercase hex, which the apply binds as TEXT. Every node runs the same
/// embedding model, so the decoded vector is valid here. `unhex` yields NULL
/// for a value that is not hex, and the embedding backfill
/// (`embedding IS NULL`) recomputes those.
fn restore_gossiped_embeddings(db: &Database) -> Result<usize> {
    Ok(db.conn.execute(
        "UPDATE reflections SET embedding = unhex(embedding) WHERE typeof(embedding) = 'text'",
        [],
    )?)
}

/// What the loop does after one handled datagram, beyond smugglr's own
/// apply: restore embeddings when reflections changed.
fn after_event(db: &Database, event: &GossipEvent) {
    if let GossipEvent::Applied { table, rows, .. } = event
        && table == "reflections"
        && *rows > 0
        && let Err(e) = restore_gossiped_embeddings(db)
    {
        eprintln!("[legion sync] embedding restore failed: {e}");
    }
}

/// Main sync loop. Never returns an error: every failure logs with
/// `[legion sync]` and the loop retries on the next interval, so sync can
/// never take the daemon down.
async fn run_sync_loop(data_dir: &Path, broadcast: &BroadcastConfig, stop_rx: &mpsc::Receiver<()>) {
    let db_path = data_dir.join("legion.db");
    let config = gossip_config(&db_path);
    let interval = Duration::from_secs(broadcast.interval_secs.max(1));

    // Bind and open retry each interval rather than giving up, so a port
    // that is briefly busy at boot does not leave the node unsynced.
    let (gossip, local, db) = loop {
        if stop_requested(stop_rx) {
            return;
        }
        match open_node(&db_path, broadcast).await {
            Ok(node) => break node,
            Err(e) => {
                eprintln!("[legion sync] {e}; retrying in {}s", interval.as_secs());
                tokio::time::sleep(interval).await;
            }
        }
    };
    eprintln!(
        "[legion sync] gossip started (instance: {}, port: {}, heartbeat: {}s)",
        crate::host::hostname(),
        broadcast.port,
        interval.as_secs()
    );

    let mut heartbeat = tokio::time::interval(interval);
    let mut buf = vec![0u8; RECV_BUF];
    loop {
        if stop_requested(stop_rx) {
            return;
        }
        tokio::select! {
            _ = heartbeat.tick() => {
                match gossip.broadcast_digests(&local, &config).await {
                    Ok(_) => persist_last_sync(data_dir, &chrono::Utc::now().to_rfc3339()),
                    Err(e) => eprintln!("[legion sync] digest send failed: {e}"),
                }
            }
            handled = gossip.recv_and_handle(&mut buf, &local, &config) => {
                match handled {
                    Ok(event) => after_event(&db, &event),
                    Err(e) => {
                        eprintln!("[legion sync] datagram handling failed: {e}");
                        // Back off so a persistent socket error cannot hot-spin.
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            }
        }
    }
}

/// Bind the gossip socket and open both connections to the store.
///
/// legion's `Database` opens first so the schema (including `host_status`)
/// exists before smugglr reads it.
async fn open_node(
    db_path: &Path,
    broadcast: &BroadcastConfig,
) -> Result<(Gossip, LocalDb, Database)> {
    let db = Database::open(db_path)?;
    let local = LocalDb::open(db_path)
        .map_err(|e| LegionError::Config(format!("sync store open failed: {e}")))?;
    let gossip = Gossip::bind(broadcast, DEFAULT_GROUP)
        .await
        .map_err(|e| LegionError::Config(format!("gossip bind failed: {e}")))?;
    Ok((gossip, local, db))
}

/// True once the handle has asked the actor to stop or been dropped.
fn stop_requested(stop_rx: &mpsc::Receiver<()>) -> bool {
    match stop_rx.try_recv() {
        Ok(()) => {
            eprintln!("[legion sync] stopping (signal received)");
            true
        }
        Err(mpsc::TryRecvError::Disconnected) => {
            eprintln!("[legion sync] stopping (handle dropped)");
            true
        }
        Err(mpsc::TryRecvError::Empty) => false,
    }
}

/// Persist `last_sync` to cluster.toml after a digest round that sent
/// without error. Best-effort: a write failure logs.
///
/// smugglr's `update_last_sync` rewrites an existing `last_sync` line in
/// place. When there is none yet it would add a `[sync]` table, which
/// `ClusterConfig` does not read, so the first write goes through
/// `ClusterConfig::save` to create the top-level line.
fn persist_last_sync(data_dir: &Path, ts: &str) {
    let result = match ClusterConfig::load(data_dir) {
        Ok(cc) if cc.last_sync.is_some() => {
            daemon::update_last_sync(&data_dir.join("cluster.toml"), ts)
                .map_err(|e| LegionError::Config(e.to_string()))
        }
        Ok(mut cc) => {
            cc.last_sync = Some(ts.to_string());
            cc.save(data_dir)
        }
        Err(e) => Err(e),
    };
    if let Err(e) = result {
        eprintln!("[legion sync] failed to persist last_sync: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::net::Ipv4Addr;

    use smugglr_core::broadcast::{DeltaPacket, maybe_encrypt};
    use smugglr_core::multicast::{Body, IgnoreReason, Msg};

    use crate::db::ReflectionMeta;

    /// Never the product port or group, so a running daemon cannot interfere.
    const TEST_PORT: u16 = 39_413;
    const TEST_GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 141, 3);

    /// One gossip node over a legion-schema temp store.
    struct Node {
        gossip: Gossip,
        local: LocalDb,
        db: Database,
        config: Config,
        key: Option<[u8; 32]>,
        _dir: tempfile::TempDir,
    }

    async fn node(instance: &str, secret: &str) -> Node {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("legion.db");
        let db = Database::open(&db_path).expect("open legion db");
        let local = LocalDb::open(&db_path).expect("open smugglr db");
        let cc = ClusterConfig {
            enabled: true,
            secret: Some(secret.to_string()),
            port: TEST_PORT,
            ..Default::default()
        };
        let mut broadcast = broadcast_config(&cc);
        broadcast.instance_id = Some(instance.to_string());
        let key = broadcast.encryption_key().expect("key decodes");
        let gossip = Gossip::bind(&broadcast, TEST_GROUP).await.expect("bind");
        Node {
            gossip,
            local,
            db,
            config: gossip_config(&db_path),
            key,
            _dir: dir,
        }
    }

    /// Seal `body` the way `sender` puts it on the wire. smugglr keeps its
    /// own `seal` private, so this builds the same `Msg` envelope and seals
    /// it with the public AEAD helper.
    fn seal(sender: &Node, body: Body) -> Vec<u8> {
        let version = DeltaPacket::new(String::new(), 0, String::new()).version;
        let plain = Msg { version, body }.to_bytes().expect("serialize");
        maybe_encrypt(&plain, &sender.key).expect("seal")
    }

    /// Deliver one body from `from` to `to`, run legion's post-apply step,
    /// and return the event and any response bodies.
    async fn route(from: &Node, body: Body, to: &Node) -> (GossipEvent, Vec<Body>) {
        let datagram = seal(from, body);
        let (event, out) = to
            .gossip
            .handle(&datagram, &to.local, &to.config)
            .await
            .expect("handle");
        after_event(&to.db, &event);
        (event, out)
    }

    /// Run one full heartbeat from `from` to `to`: every digest, the Wants
    /// it provokes, and the Deltas that answer them. Returns the Wants sent.
    async fn converge(from: &Node, to: &Node) -> usize {
        let digests = from
            .gossip
            .digest_bodies(&from.local, &from.config)
            .await
            .expect("digests");
        let mut wants = 0;
        for digest in digests {
            let (_ev, out) = route(from, digest, to).await;
            for want in out {
                wants += 1;
                let (_ev, deltas) = route(to, want, from).await;
                for delta in deltas {
                    route(from, delta, to).await;
                }
            }
        }
        wants
    }

    fn key(byte: &str) -> String {
        byte.repeat(32)
    }

    fn text_of(db: &Database, id: &str) -> Option<String> {
        db.conn
            .query_row("SELECT text FROM reflections WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .ok()
    }

    #[tokio::test]
    async fn two_nodes_converge_on_reflection_schedule_and_host_status() {
        let a = node("node-a", &key("ab")).await;
        let b = node("node-b", &key("ab")).await;

        let r = a
            .db
            .insert_reflection_with_meta("legion", "gossiped", "self", &ReflectionMeta::default())
            .expect("reflection");
        let sched =
            a.db.insert_schedule("nightly", "*/5m", "echo hi", "legion", None, None)
                .expect("schedule");
        a.db.upsert_host_status(&crate::health::HostStatus {
            hostname: "node-a".into(),
            cpu_pct: 12.5,
            mem_pct: 40.0,
            load_1m: Some(0.5),
            agents_active: 1,
            created_at: "2026-10-10T10:00:00+00:00".into(),
            updated_at: "2026-10-10T10:00:00+00:00".into(),
        })
        .expect("host status");

        assert!(converge(&a, &b).await > 0, "B must ask for A's rows");

        assert_eq!(text_of(&b.db, &r.id).as_deref(), Some("gossiped"));
        let sched_name: String =
            b.db.conn
                .query_row("SELECT name FROM schedules WHERE id = ?1", [&sched], |r| {
                    r.get(0)
                })
                .expect("schedule arrived");
        assert_eq!(sched_name, "nightly");
        let hosts = b.db.list_host_status().expect("hosts");
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].hostname, "node-a");

        assert_eq!(converge(&a, &b).await, 0, "converged nodes send no Want");
    }

    #[tokio::test]
    async fn foreign_key_datagram_is_ignored_and_changes_nothing() {
        let a = node("node-a", &key("ab")).await;
        let b = node("node-b", &key("cd")).await;
        a.db.insert_reflection("legion", "secret", "self")
            .expect("reflection");

        let digests = a
            .gossip
            .digest_bodies(&a.local, &a.config)
            .await
            .expect("digests");
        for digest in digests {
            let (event, out) = route(&a, digest, &b).await;
            assert_eq!(event, GossipEvent::Ignored(IgnoreReason::Undecryptable));
            assert!(out.is_empty());
        }
        let count: i64 =
            b.db.conn
                .query_row("SELECT COUNT(*) FROM reflections", [], |r| r.get(0))
                .expect("count");
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn excluded_column_differences_produce_matching_digests() {
        let a = node("node-a", &key("ab")).await;
        let b = node("node-b", &key("ab")).await;
        let r =
            a.db.insert_reflection("legion", "same row", "self")
                .expect("reflection");
        converge(&a, &b).await;

        // Diverge only in excluded columns.
        a.db.conn
            .execute(
                "UPDATE reflections SET embedding = x'0102', recall_count = 7 WHERE id = ?1",
                [&r.id],
            )
            .expect("diverge a");
        b.db.conn
            .execute(
                "UPDATE reflections SET embedding = x'0304', recall_count = 2 WHERE id = ?1",
                [&r.id],
            )
            .expect("diverge b");

        let a_bodies = a
            .gossip
            .digest_bodies(&a.local, &a.config)
            .await
            .expect("a digests");
        let b_bodies = b
            .gossip
            .digest_bodies(&b.local, &b.config)
            .await
            .expect("b digests");
        assert_eq!(reflection_hashes(a_bodies), reflection_hashes(b_bodies));
        assert_eq!(converge(&a, &b).await, 0, "no Want for excluded columns");
    }

    #[tokio::test]
    async fn pulled_embedding_arrives_as_the_senders_blob() {
        let a = node("node-a", &key("ab")).await;
        let b = node("node-b", &key("ab")).await;
        let r =
            a.db.insert_reflection("legion", "embedded", "self")
                .expect("reflection");
        let vector: Vec<u8> = (0u8..=255).collect();
        a.db.store_embedding(&r.id, &vector).expect("embed");
        let bad =
            a.db.insert_reflection("legion", "garbled", "self")
                .expect("reflection");

        converge(&a, &b).await;

        assert_eq!(
            b.db.get_embedding(&r.id).expect("read embedding"),
            Some(vector),
            "receiver holds the sender's vector as a BLOB"
        );
        let embeddings = b.db.get_embeddings(None).expect("every row reads");
        assert_eq!(embeddings.len(), 1);

        // A TEXT embedding that is not hex decodes to NULL, which the
        // embedding backfill picks up.
        b.db.conn
            .execute(
                "UPDATE reflections SET embedding = 'not-hex' WHERE id = ?1",
                [&bad.id],
            )
            .expect("garble");
        restore_gossiped_embeddings(&b.db).expect("restore");
        let missing = b.db.get_ids_without_embeddings().expect("missing");
        assert!(missing.iter().any(|(id, _)| id == &bad.id));
        b.db.get_embeddings(None).expect("every row still reads");
    }

    #[tokio::test]
    async fn newer_tombstone_wins_over_older_live_row() {
        let a = node("node-a", &key("ab")).await;
        let b = node("node-b", &key("ab")).await;
        let r =
            a.db.insert_reflection("legion", "to delete", "self")
                .expect("reflection");
        converge(&a, &b).await;

        a.db.soft_delete_reflection(&r.id).expect("tombstone");
        converge(&a, &b).await;

        let deleted_at: Option<String> =
            b.db.conn
                .query_row(
                    "SELECT deleted_at FROM reflections WHERE id = ?1",
                    [&r.id],
                    |row| row.get(0),
                )
                .expect("row");
        assert!(deleted_at.is_some(), "the tombstone replaced the live row");

        // The older live copy does not resurrect it in the other direction.
        b.db.conn
            .execute("UPDATE reflections SET deleted_at = NULL, updated_at = '2020-01-01T00:00:00+00:00' WHERE id = ?1", [&r.id])
            .expect("stale live copy");
        converge(&b, &a).await;
        let a_deleted: Option<String> =
            a.db.conn
                .query_row(
                    "SELECT deleted_at FROM reflections WHERE id = ?1",
                    [&r.id],
                    |row| row.get(0),
                )
                .expect("row");
        assert!(a_deleted.is_some(), "an older live row must not win");
    }

    #[test]
    fn spawn_gate_none_when_disabled_or_secretless() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(spawn_sync_if_enabled(dir.path(), "[test]").is_none());

        let cc = ClusterConfig {
            enabled: true,
            secret: None,
            ..Default::default()
        };
        cc.save(dir.path()).expect("save");
        assert!(spawn_sync_if_enabled(dir.path(), "[test]").is_none());
    }

    #[test]
    fn spawn_refuses_without_secret() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = SyncHandle::spawn(dir.path(), ClusterConfig::default())
            .err()
            .expect("secretless spawn must fail");
        assert!(matches!(err, LegionError::Config(m) if m.contains("secret")));
    }

    #[test]
    fn persist_last_sync_writes_then_rewrites_top_level_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cc = ClusterConfig {
            enabled: true,
            secret: Some(key("ab")),
            ..Default::default()
        };
        cc.save(dir.path()).expect("save");

        persist_last_sync(dir.path(), "2026-10-10T12:00:00+00:00");
        let first = ClusterConfig::load(dir.path()).expect("reload");
        assert_eq!(
            first.last_sync.as_deref(),
            Some("2026-10-10T12:00:00+00:00")
        );

        persist_last_sync(dir.path(), "2026-10-10T12:01:00+00:00");
        let second = ClusterConfig::load(dir.path()).expect("reload");
        assert_eq!(
            second.last_sync.as_deref(),
            Some("2026-10-10T12:01:00+00:00")
        );
        assert!(second.enabled);
        assert_eq!(second.secret, Some(key("ab")));
    }

    fn reflection_hashes(bodies: Vec<Body>) -> HashMap<String, String> {
        bodies
            .into_iter()
            .filter_map(|body| match body {
                Body::Digest(d) if d.table == "reflections" => Some(d.hashes),
                _ => None,
            })
            .flatten()
            .collect()
    }
}
