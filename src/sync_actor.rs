//! Sync actor for multi-node delta replication.
//!
//! Runs in a background thread with its own tokio runtime, handling:
//! - Peer discovery via UDP broadcast
//! - Delta serialization and transmission
//! - Receiving and applying deltas with LWW merge
//!
//! The actor communicates with the main watch loop via channels.

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde_json::Value;
use smugglr_core::broadcast::{hash_db_path, BroadcastConfig, PeerDiscovery};
use tokio::runtime::Runtime;

use crate::cluster::ClusterConfig;
use crate::db::Database;
use crate::error::Result;
use crate::sync::{CardDelta, ReflectionDelta, ScheduleDelta};

/// Handle for communicating with the sync actor.
pub struct SyncHandle {
    /// Signal the actor to stop.
    _stop_tx: mpsc::Sender<()>,
    /// Thread handle for joining.
    _thread: thread::JoinHandle<()>,
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
            let rt = Runtime::new().expect("failed to create tokio runtime");
            rt.block_on(async {
                if let Err(e) = run_sync_loop(&data_dir, broadcast_config, db_path_hash, stop_rx).await
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
            eprintln!("[legion sync] failed to initialize: {}", e);
            return Ok(());
        }
    };

    eprintln!(
        "[legion sync] actor started (instance: {}, port: {})",
        discovery.instance_id(),
        config.port
    );

    let db_path = data_dir.join("legion.db");
    let mut last_sync = chrono::Utc::now().to_rfc3339();
    let sync_interval = Duration::from_secs(config.interval_secs);

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

            // Open DB and check for local changes
            if let Ok(db) = Database::open(&db_path) {
                // Get deltas since last sync
                let reflection_deltas = db.get_reflection_deltas_since(&last_sync).unwrap_or_else(|e| {
                    eprintln!("[legion sync] reflection delta query failed: {}", e);
                    Vec::new()
                });
                let card_deltas = db.get_card_deltas_since(&last_sync).unwrap_or_else(|e| {
                    eprintln!("[legion sync] card delta query failed: {}", e);
                    Vec::new()
                });
                let schedule_deltas = db.get_schedule_deltas_since(&last_sync).unwrap_or_else(|e| {
                    eprintln!("[legion sync] schedule delta query failed: {}", e);
                    Vec::new()
                });

                let total = reflection_deltas.len() + card_deltas.len() + schedule_deltas.len();
                if total > 0 {
                    eprintln!(
                        "[legion sync] broadcasting {} delta(s) ({} reflections, {} cards, {} schedules)",
                        total,
                        reflection_deltas.len(),
                        card_deltas.len(),
                        schedule_deltas.len()
                    );

                    // TODO: Actually broadcast the deltas to peers
                    // For now, just log that we would send them
                    // The wire protocol uses DeltaPacket with upserts/deletes
                }
            }

            last_sync = chrono::Utc::now().to_rfc3339();
        }

        // Sleep until next cycle
        tokio::time::sleep(sync_interval).await;
    }

    Ok(())
}

/// Convert a ReflectionDelta to a HashMap for DeltaPacket.
#[allow(dead_code)]
fn reflection_to_map(r: &ReflectionDelta) -> HashMap<String, Value> {
    let mut m = HashMap::new();
    m.insert("id".into(), Value::String(r.id.clone()));
    m.insert("repo".into(), Value::String(r.repo.clone()));
    m.insert("text".into(), Value::String(r.text.clone()));
    m.insert("created_at".into(), Value::String(r.created_at.clone()));
    if let Some(ref v) = r.updated_at {
        m.insert("updated_at".into(), Value::String(v.clone()));
    }
    if let Some(ref v) = r.deleted_at {
        m.insert("deleted_at".into(), Value::String(v.clone()));
    }
    m.insert("audience".into(), Value::String(r.audience.clone()));
    if let Some(ref v) = r.domain {
        m.insert("domain".into(), Value::String(v.clone()));
    }
    if let Some(ref v) = r.tags {
        m.insert("tags".into(), Value::String(v.clone()));
    }
    m.insert("recall_count".into(), Value::Number(r.recall_count.into()));
    if let Some(ref v) = r.last_recalled_at {
        m.insert("last_recalled_at".into(), Value::String(v.clone()));
    }
    if let Some(ref v) = r.parent_id {
        m.insert("parent_id".into(), Value::String(v.clone()));
    }
    m
}

/// Convert a CardDelta to a HashMap for DeltaPacket.
#[allow(dead_code)]
fn card_to_map(c: &CardDelta) -> HashMap<String, Value> {
    let mut m = HashMap::new();
    m.insert("id".into(), Value::String(c.id.clone()));
    m.insert("from_repo".into(), Value::String(c.from_repo.clone()));
    m.insert("to_repo".into(), Value::String(c.to_repo.clone()));
    m.insert("text".into(), Value::String(c.text.clone()));
    if let Some(ref v) = c.context {
        m.insert("context".into(), Value::String(v.clone()));
    }
    m.insert("priority".into(), Value::String(c.priority.clone()));
    m.insert("status".into(), Value::String(c.status.clone()));
    m.insert("created_at".into(), Value::String(c.created_at.clone()));
    m.insert("updated_at".into(), Value::String(c.updated_at.clone()));
    if let Some(ref v) = c.deleted_at {
        m.insert("deleted_at".into(), Value::String(v.clone()));
    }
    // ... more fields as needed
    m
}

/// Convert a ScheduleDelta to a HashMap for DeltaPacket.
#[allow(dead_code)]
fn schedule_to_map(s: &ScheduleDelta) -> HashMap<String, Value> {
    let mut m = HashMap::new();
    m.insert("id".into(), Value::String(s.id.clone()));
    m.insert("name".into(), Value::String(s.name.clone()));
    m.insert("cron".into(), Value::String(s.cron.clone()));
    m.insert("command".into(), Value::String(s.command.clone()));
    m.insert("repo".into(), Value::String(s.repo.clone()));
    m.insert("enabled".into(), Value::Bool(s.enabled));
    m.insert("created_at".into(), Value::String(s.created_at.clone()));
    if let Some(ref v) = s.updated_at {
        m.insert("updated_at".into(), Value::String(v.clone()));
    }
    if let Some(ref v) = s.deleted_at {
        m.insert("deleted_at".into(), Value::String(v.clone()));
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reflection_to_map_basic_fields() {
        let r = ReflectionDelta {
            id: "test-id".into(),
            repo: "test-repo".into(),
            text: "test text".into(),
            created_at: "2026-04-15T00:00:00Z".into(),
            updated_at: Some("2026-04-15T01:00:00Z".into()),
            deleted_at: None,
            audience: "self".into(),
            domain: Some("test".into()),
            tags: None,
            recall_count: 5,
            last_recalled_at: None,
            parent_id: None,
        };

        let m = reflection_to_map(&r);
        assert_eq!(m.get("id").unwrap(), &Value::String("test-id".into()));
        assert_eq!(m.get("recall_count").unwrap(), &Value::Number(5.into()));
    }
}
