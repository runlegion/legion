//! Sync actor for multi-node delta replication (#536).
//!
//! Runs in a background thread with its own tokio runtime, handling:
//! - Peer discovery via UDP broadcast (smugglr-core `PeerDiscovery`)
//! - Delta serialization, encryption, and unicast transmission to peers
//! - Receiving and applying deltas with LWW merge
//!
//! Transport convention: discovery announcements broadcast on the configured
//! `port`; delta data flows over a second UDP socket on `port + 1`
//! ([`DATA_PORT_OFFSET`]). Every node in a cluster shares one `cluster.toml`
//! shape (same port, same secret), so the data port is derivable without
//! carrying it in the announcement. All delta traffic is sealed with
//! XChaCha20-Poly1305 via smugglr-core's `maybe_encrypt`/`maybe_decrypt`.
//!
//! `last_sync` advances and persists to `cluster.toml` only after a cycle in
//! which every computed packet was handed to the socket without error, so
//! `legion cluster status` reflects reality and a failed cycle retries the
//! same window.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use smugglr_core::broadcast::{
    BroadcastConfig, DeltaPacket, PeerDiscovery, ReplayGuard, hash_db_path, maybe_decrypt,
    maybe_encrypt, split_delta,
};
use tokio::net::UdpSocket;
use tokio::runtime::Runtime;

use crate::cluster::ClusterConfig;
use crate::db::Database;
use crate::error::Result;
use crate::sync::{CardDelta, PersonaWakeLeaseDelta, ReflectionDelta, ScheduleDelta};

/// Delta data flows on `discovery port + DATA_PORT_OFFSET`.
const DATA_PORT_OFFSET: u16 = 1;

/// Wire-envelope headroom passed to `split_delta`: XChaCha20-Poly1305 adds a
/// 24-byte nonce and 16-byte tag; the rest is margin for base overhead.
const SEAL_RESERVE: usize = 64;

/// Table names on the wire. The receive path matches on these exactly.
const TABLE_REFLECTIONS: &str = "reflections";
const TABLE_CARDS: &str = "cards";
const TABLE_SCHEDULES: &str = "schedules";
const TABLE_LEASES: &str = "persona_wake_leases";

/// Handle for communicating with the sync actor.
pub struct SyncHandle {
    /// Signal the actor to stop.
    _stop_tx: mpsc::Sender<()>,
    /// Thread handle for joining.
    _thread: thread::JoinHandle<()>,
}

/// Load `cluster.toml` and spawn the sync actor when sync is enabled and a
/// secret is configured. The single spawn gate shared by `legion watch` and
/// the daemon's watch task (#536 defect 1: the daemon previously had no spawn
/// at all). Sync is optional and never fatal: config errors and spawn
/// failures log under `log_prefix` and return None so the caller's loop runs
/// regardless.
pub fn spawn_sync_if_enabled(data_dir: &Path, log_prefix: &str) -> Option<SyncHandle> {
    match ClusterConfig::load(data_dir) {
        Ok(cc) if cc.enabled && cc.secret.is_some() => {
            eprintln!(
                "{log_prefix} cluster sync enabled on port {} (instance: {})",
                cc.port,
                cc.resolve_instance_id()
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
    pub fn spawn(data_dir: &Path, config: ClusterConfig) -> Result<Self> {
        let db_path = data_dir.join("legion.db");
        let db_path_str = db_path.to_string_lossy().to_string();
        let db_path_hash = hash_db_path(&db_path_str);

        let secret = config.secret.clone();
        let broadcast_config = BroadcastConfig {
            port: config.port,
            interval_secs: 30,
            instance_id: Some(config.resolve_instance_id()),
            secret,
        };

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
            rt.block_on(async {
                if let Err(e) =
                    run_sync_loop(&data_dir, broadcast_config, db_path_hash, stop_rx).await
                {
                    eprintln!("[legion sync] actor error: {}", e);
                }
            });
        });

        Ok(Self {
            _stop_tx: stop_tx,
            _thread: thread,
        })
    }
}

/// Main sync loop running in tokio.
async fn run_sync_loop(
    data_dir: &Path,
    config: BroadcastConfig,
    db_path_hash: String,
    stop_rx: mpsc::Receiver<()>,
) -> Result<()> {
    let discovery = match PeerDiscovery::new(config.clone(), db_path_hash.clone()).await {
        Ok(d) => d,
        Err(e) => {
            return Err(crate::error::LegionError::Config(format!(
                "sync actor failed to initialize: {}",
                e
            )));
        }
    };

    let key = config.encryption_key().map_err(|e| {
        crate::error::LegionError::Config(format!("sync actor key derivation failed: {e}"))
    })?;

    let data_port = config.port + DATA_PORT_OFFSET;
    let data_socket = UdpSocket::bind(("0.0.0.0", data_port)).await.map_err(|e| {
        crate::error::LegionError::Config(format!("sync actor bind data port {data_port}: {e}"))
    })?;
    let data_socket = std::sync::Arc::new(data_socket);

    eprintln!(
        "[legion sync] actor started (instance: {}, discovery port: {}, data port: {})",
        discovery.instance_id(),
        config.port,
        data_port
    );

    // Receive side: own task, shared socket. Applies sealed DeltaPackets to
    // the local DB with replay protection. Never crashes the actor: every
    // failure logs and the loop continues.
    let recv_db_path = data_dir.join("legion.db");
    let recv_socket = data_socket.clone();
    let recv_key = key;
    let recv_self_id = discovery.instance_id().to_string();
    tokio::spawn(async move {
        receive_loop(recv_socket, recv_key, recv_self_id, recv_db_path).await;
    });

    let db_path = data_dir.join("legion.db");
    let mut last_sync = ClusterConfig::load(data_dir)
        .ok()
        .and_then(|cc| cc.last_sync)
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
    let sync_interval = Duration::from_secs(config.interval_secs);
    let source_id = discovery.instance_id().to_string();
    let mut seq: u64 = 0;

    loop {
        // Check for stop signal (non-blocking)
        // Break on Ok (explicit stop) or Disconnected (sender dropped)
        match stop_rx.try_recv() {
            Ok(()) => {
                eprintln!("[legion sync] stopping (signal received)");
                break;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                eprintln!("[legion sync] stopping (handle dropped)");
                break;
            }
            Err(mpsc::TryRecvError::Empty) => {
                // No signal yet, continue loop
            }
        }

        // Discover peers
        let peers = match discovery.discover_once(Duration::from_secs(5)).await {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[legion sync] discovery error: {}", e);
                Vec::new()
            }
        };

        if !peers.is_empty() {
            eprintln!("[legion sync] {} peer(s) online", peers.len());

            let cycle_start = chrono::Utc::now().to_rfc3339();
            if let Ok(db) = Database::open(&db_path) {
                let packets = match build_delta_packets(&db, &last_sync, &source_id, &mut seq) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("[legion sync] delta build failed: {e}");
                        Vec::new()
                    }
                };

                if !packets.is_empty() {
                    let upsert_total: usize = packets.iter().map(|p| p.upserts.len()).sum();
                    eprintln!(
                        "[legion sync] broadcasting {} delta row(s) in {} packet(s) to {} peer(s)",
                        upsert_total,
                        packets.len(),
                        peers.len()
                    );

                    let mut all_sent = true;
                    for packet in &packets {
                        let bytes = match packet.to_bytes().and_then(|b| maybe_encrypt(&b, &key)) {
                            Ok(b) => b,
                            Err(e) => {
                                eprintln!("[legion sync] packet seal failed: {e}");
                                all_sent = false;
                                continue;
                            }
                        };
                        for peer in &peers {
                            let target = std::net::SocketAddr::new(
                                peer.addr.ip(),
                                config.port + DATA_PORT_OFFSET,
                            );
                            if let Err(e) = data_socket.send_to(&bytes, target).await {
                                eprintln!(
                                    "[legion sync] send to {} ({}) failed: {e}",
                                    peer.instance_id, target
                                );
                                all_sent = false;
                            }
                        }
                    }

                    // Advance and persist last_sync only after a fully
                    // successful cycle (#536 defects 2+3); a failed peer
                    // retries the same window next cycle.
                    if all_sent {
                        last_sync = cycle_start;
                        persist_last_sync(data_dir, &last_sync);
                    }
                }
            }
        }

        // Sleep until next cycle
        tokio::time::sleep(sync_interval).await;
    }

    Ok(())
}

/// Compute per-table deltas since `last_sync` and pack them into
/// datagram-sized `DeltaPacket`s. Rows travel as their serde maps; tombstones
/// ride in `upserts` with `deleted_at` set (the LWW apply handles them), so
/// `deletes` stays empty.
fn build_delta_packets(
    db: &Database,
    last_sync: &str,
    source_id: &str,
    seq: &mut u64,
) -> Result<Vec<DeltaPacket>> {
    let mut packets = Vec::new();

    let reflections = db.get_reflection_deltas_since(last_sync)?;
    pack_table(
        &mut packets,
        source_id,
        seq,
        TABLE_REFLECTIONS,
        &reflections,
    )?;
    let cards = db.get_card_deltas_since(last_sync)?;
    pack_table(&mut packets, source_id, seq, TABLE_CARDS, &cards)?;
    let schedules = db.get_schedule_deltas_since(last_sync)?;
    pack_table(&mut packets, source_id, seq, TABLE_SCHEDULES, &schedules)?;
    let leases = db.get_persona_wake_lease_deltas_since(last_sync)?;
    pack_table(&mut packets, source_id, seq, TABLE_LEASES, &leases)?;

    Ok(packets)
}

/// Serialize one table's delta rows into MTU-sized packets, advancing `seq`
/// per packet so the receiver's `ReplayGuard` sees a strictly increasing
/// sequence per source.
fn pack_table<T: serde::Serialize>(
    packets: &mut Vec<DeltaPacket>,
    source_id: &str,
    seq: &mut u64,
    table: &str,
    rows: &[T],
) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let upserts = rows
        .iter()
        .map(|r| {
            serde_json::to_value(r)
                .map_err(|e| {
                    crate::error::LegionError::Config(format!("delta serialize ({table}): {e}"))
                })
                .and_then(|v| match v {
                    serde_json::Value::Object(m) => Ok(m.into_iter().collect()),
                    other => Err(crate::error::LegionError::Config(format!(
                        "delta row for {table} is not an object: {other}"
                    ))),
                })
        })
        .collect::<Result<Vec<_>>>()?;

    let split = split_delta(source_id, *seq, table, upserts, Vec::new(), SEAL_RESERVE)
        .map_err(|e| crate::error::LegionError::Config(format!("delta split ({table}): {e}")))?;
    // split_delta emits parts sharing one seq; bump once per table batch.
    *seq += 1;
    packets.extend(split);
    Ok(())
}

/// Receive sealed DeltaPackets and apply them to the local DB. Runs for the
/// actor's lifetime; every failure path logs and continues.
async fn receive_loop(
    socket: std::sync::Arc<UdpSocket>,
    key: Option<[u8; 32]>,
    self_id: String,
    db_path: PathBuf,
) {
    let mut guard = ReplayGuard::new();
    let mut buf = vec![0u8; 65_536];
    loop {
        let (len, from) = match socket.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[legion sync] recv error: {e}");
                continue;
            }
        };
        let plaintext = match maybe_decrypt(&buf[..len], &key) {
            Ok(Some(p)) => p,
            Ok(None) => continue, // not for us / not decryptable with our key
            Err(e) => {
                eprintln!("[legion sync] decrypt error from {from}: {e}");
                continue;
            }
        };
        let packet = match DeltaPacket::from_bytes(&plaintext) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[legion sync] bad packet from {from}: {e}");
                continue;
            }
        };
        if packet.source_id == self_id {
            continue; // our own broadcast reflected back
        }
        if !guard.check(&packet.source_id, packet.seq) {
            continue; // replay or out-of-order duplicate
        }
        match Database::open(&db_path) {
            Ok(db) => {
                let applied = apply_packet(&db, &packet);
                eprintln!(
                    "[legion sync] applied {applied}/{} row(s) from {} (table: {})",
                    packet.upserts.len(),
                    packet.source_id,
                    packet.table
                );
            }
            Err(e) => eprintln!("[legion sync] db open for apply failed: {e}"),
        }
    }
}

/// Apply one packet's rows to the local DB via the per-table LWW apply
/// methods. Returns the number of rows applied without error; per-row
/// failures log and do not abort the packet.
fn apply_packet(db: &Database, packet: &DeltaPacket) -> usize {
    let mut applied = 0;
    for row in &packet.upserts {
        let value = serde_json::Value::Object(row.clone().into_iter().collect());
        let result = match packet.table.as_str() {
            TABLE_REFLECTIONS => serde_json::from_value::<ReflectionDelta>(value)
                .map_err(|e| e.to_string())
                .and_then(|d| db.apply_reflection_delta(&d).map_err(|e| e.to_string())),
            TABLE_CARDS => serde_json::from_value::<CardDelta>(value)
                .map_err(|e| e.to_string())
                .and_then(|d| db.apply_card_delta(&d).map_err(|e| e.to_string())),
            TABLE_SCHEDULES => serde_json::from_value::<ScheduleDelta>(value)
                .map_err(|e| e.to_string())
                .and_then(|d| db.apply_schedule_delta(&d).map_err(|e| e.to_string())),
            TABLE_LEASES => serde_json::from_value::<PersonaWakeLeaseDelta>(value)
                .map_err(|e| e.to_string())
                .and_then(|d| {
                    db.apply_persona_wake_lease_delta(&d)
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                }),
            other => Err(format!("unknown table '{other}'")),
        };
        match result {
            Ok(()) => applied += 1,
            Err(e) => eprintln!("[legion sync] apply failed (table: {}): {e}", packet.table),
        }
    }
    applied
}

/// Persist `last_sync` to cluster.toml so `legion cluster status` reflects
/// reality (#536 defect 3). Best-effort: a write failure logs and the
/// in-memory cursor still advances.
fn persist_last_sync(data_dir: &Path, ts: &str) {
    match ClusterConfig::load(data_dir) {
        Ok(mut cc) => {
            cc.last_sync = Some(ts.to_string());
            if let Err(e) = cc.save(data_dir) {
                eprintln!("[legion sync] failed to persist last_sync: {e}");
            }
        }
        Err(e) => eprintln!("[legion sync] failed to reload cluster.toml: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::ReflectionMeta;

    fn temp_data_dir() -> PathBuf {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_path_buf();
        std::mem::forget(dir);
        path
    }

    #[test]
    fn spawn_gate_none_when_disabled_or_secretless() {
        let dir = temp_data_dir();
        // No cluster.toml at all -> default config -> disabled -> None.
        assert!(spawn_sync_if_enabled(&dir, "[test]").is_none());

        // Enabled but no secret -> None.
        let cc = ClusterConfig {
            enabled: true,
            secret: None,
            ..Default::default()
        };
        cc.save(&dir).expect("save");
        assert!(spawn_sync_if_enabled(&dir, "[test]").is_none());
    }

    #[test]
    fn packet_seal_unseal_round_trip() {
        let key = Some([7u8; 32]);
        let mut seq = 0;
        let mut packets = Vec::new();
        let rows = vec![ReflectionDelta {
            id: "r-1".into(),
            repo: "legion".into(),
            text: "hello".into(),
            created_at: "2026-06-10T00:00:00Z".into(),
            updated_at: Some("2026-06-10T01:00:00Z".into()),
            deleted_at: None,
            audience: "self".into(),
            domain: None,
            tags: None,
            recall_count: 0,
            last_recalled_at: None,
            parent_id: None,
        }];
        pack_table(&mut packets, "node-a", &mut seq, TABLE_REFLECTIONS, &rows).expect("pack");
        assert_eq!(packets.len(), 1);
        assert_eq!(seq, 1);

        let sealed = maybe_encrypt(&packets[0].to_bytes().expect("bytes"), &key).expect("seal");
        let opened = maybe_decrypt(&sealed, &key).expect("unseal").expect("some");
        let packet = DeltaPacket::from_bytes(&opened).expect("parse");
        assert_eq!(packet.table, TABLE_REFLECTIONS);
        assert_eq!(packet.upserts.len(), 1);
        assert_eq!(
            packet.upserts[0].get("id").and_then(|v| v.as_str()),
            Some("r-1")
        );
    }

    #[test]
    fn apply_packet_round_trips_reflection_into_db() {
        let dir = temp_data_dir();
        let db = Database::open(&dir.join("legion.db")).expect("open");

        let rows = vec![ReflectionDelta {
            id: "0190a000-0000-7000-8000-000000000001".into(),
            repo: "legion".into(),
            text: "synced from peer".into(),
            created_at: "2026-06-10T00:00:00Z".into(),
            updated_at: Some("2026-06-10T01:00:00Z".into()),
            deleted_at: None,
            audience: "self".into(),
            domain: Some("test".into()),
            tags: None,
            recall_count: 0,
            last_recalled_at: None,
            parent_id: None,
        }];
        let mut seq = 0;
        let mut packets = Vec::new();
        pack_table(&mut packets, "node-b", &mut seq, TABLE_REFLECTIONS, &rows).expect("pack");

        let applied = apply_packet(&db, &packets[0]);
        assert_eq!(applied, 1);

        // Read back via a direct row query (get_reflections_by_ids carries a
        // known column-shift bug being fixed separately under #606).
        let text: String = db
            .conn
            .query_row(
                "SELECT text FROM reflections WHERE id = ?1",
                ["0190a000-0000-7000-8000-000000000001"],
                |r| r.get(0),
            )
            .expect("read back");
        assert_eq!(text, "synced from peer");
    }

    #[test]
    fn apply_packet_unknown_table_applies_zero() {
        let dir = temp_data_dir();
        let db = Database::open(&dir.join("legion.db")).expect("open");
        let mut packet = DeltaPacket::new("node-c".into(), 0, "no_such_table".into());
        packet.upserts.push(std::collections::HashMap::from([(
            "id".to_string(),
            serde_json::Value::String("x".into()),
        )]));
        assert_eq!(apply_packet(&db, &packet), 0);
    }

    #[test]
    fn persist_last_sync_survives_reload() {
        let dir = temp_data_dir();
        let cc = ClusterConfig {
            enabled: true,
            secret: Some("ab".repeat(32)),
            ..Default::default()
        };
        cc.save(&dir).expect("save");

        persist_last_sync(&dir, "2026-06-10T12:00:00Z");
        let reloaded = ClusterConfig::load(&dir).expect("reload");
        assert_eq!(reloaded.last_sync.as_deref(), Some("2026-06-10T12:00:00Z"));
        // Other fields survive the round-trip.
        assert!(reloaded.enabled);
        assert_eq!(reloaded.secret.as_deref(), Some("ab".repeat(32).as_str()));
    }

    // ReflectionMeta imported for the receive-side test below.
    #[test]
    fn apply_packet_lww_does_not_clobber_newer_local() {
        let dir = temp_data_dir();
        let db = Database::open(&dir.join("legion.db")).expect("open");

        // Local row written now (fresh updated_at).
        let local = db
            .insert_reflection_with_meta(
                "legion",
                "local newer text",
                "self",
                &ReflectionMeta::default(),
            )
            .expect("insert");

        // Peer delta carrying an OLDER updated_at must not overwrite.
        let rows = vec![ReflectionDelta {
            id: local.id.clone(),
            repo: "legion".into(),
            text: "stale peer text".into(),
            created_at: "2020-01-01T00:00:00Z".into(),
            updated_at: Some("2020-01-01T00:00:00Z".into()),
            deleted_at: None,
            audience: "self".into(),
            domain: None,
            tags: None,
            recall_count: 0,
            last_recalled_at: None,
            parent_id: None,
        }];
        let mut seq = 0;
        let mut packets = Vec::new();
        pack_table(&mut packets, "node-d", &mut seq, TABLE_REFLECTIONS, &rows).expect("pack");
        apply_packet(&db, &packets[0]);

        let text: String = db
            .conn
            .query_row(
                "SELECT text FROM reflections WHERE id = ?1",
                [local.id.as_str()],
                |r| r.get(0),
            )
            .expect("read back");
        assert_eq!(text, "local newer text");
    }
}
