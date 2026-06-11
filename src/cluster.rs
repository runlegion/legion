//! Multi-node cluster sync via LAN broadcast with encryption.
//!
//! Legion nodes on the same subnet discover each other via UDP broadcast
//! and synchronize reflections, cards, and schedules using delta packets.
//! All traffic is encrypted with XChaCha20-Poly1305 using a pre-shared key.
//!
//! Trust model: the PSK is the entire boundary. Schedules sync across the
//! cluster and the schedule runner executes their commands, so possession
//! of the secret (or compromise of any one node) is command execution on
//! every node. Share the key accordingly. Discovery announcements are
//! unauthenticated by design (availability only -- delta payloads stay
//! AEAD-sealed); an on-LAN spoofer can misdirect traffic but not read or
//! forge it.
//!
//! Configuration is stored in `<data_dir>/cluster.toml`:
//! ```toml
//! enabled = true
//! secret = "64-hex-chars"
//! port = 31337
//! instance_id = "hostname"
//! last_sync = "2026-04-15T00:00:00Z"
//! ```

use crate::error::{LegionError, Result};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// Cluster configuration stored in cluster.toml.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterConfig {
    /// Whether cluster sync is enabled
    #[serde(default)]
    pub enabled: bool,

    /// 256-bit pre-shared key, hex-encoded (64 hex chars)
    pub secret: Option<String>,

    /// UDP port for broadcast (default: 31337)
    #[serde(default = "default_port")]
    pub port: u16,

    /// Instance identity (defaults to hostname)
    pub instance_id: Option<String>,

    /// Last successful sync timestamp
    pub last_sync: Option<String>,

    /// Preserve unknown fields on round-trip
    #[serde(flatten)]
    pub extra: toml::Table,
}

fn default_port() -> u16 {
    31337
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            secret: None,
            port: default_port(),
            instance_id: None,
            last_sync: None,
            extra: toml::Table::new(),
        }
    }
}

impl ClusterConfig {
    /// Load config from cluster.toml, or return default if it doesn't exist.
    pub fn load(data_dir: &Path) -> Result<Self> {
        let path = data_dir.join("cluster.toml");
        if path.exists() {
            let content = fs::read_to_string(&path)?;
            toml::from_str(&content)
                .map_err(|e| LegionError::Config(format!("failed to parse cluster.toml: {e}")))
        } else {
            Ok(Self::default())
        }
    }

    /// Save config to cluster.toml.
    pub fn save(&self, data_dir: &Path) -> Result<()> {
        let path = data_dir.join("cluster.toml");
        let content = toml::to_string_pretty(self)
            .map_err(|e| LegionError::Config(format!("failed to serialize cluster config: {e}")))?;
        fs::write(&path, &content)?;

        // Set restrictive permissions on Unix (contains secret key)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(&path, perms)?;
        }

        Ok(())
    }

    /// Resolve the instance ID, falling back to hostname.
    pub fn resolve_instance_id(&self) -> String {
        self.instance_id.clone().unwrap_or_else(|| {
            hostname::get()
                .ok()
                .and_then(|h| h.into_string().ok())
                .unwrap_or_else(|| "unknown".to_string())
        })
    }
}

/// Generate a new 256-bit key as a hex string.
pub fn generate_key() -> String {
    let mut key = [0u8; 32];
    rand::rng().fill_bytes(&mut key);
    hex::encode(key)
}

/// Validate a hex-encoded 256-bit key.
pub fn validate_key(key: &str) -> Result<()> {
    let bytes =
        hex::decode(key).map_err(|_| LegionError::Config("key must be valid hex".to_string()))?;
    if bytes.len() != 32 {
        return Err(LegionError::Config(
            "key must be 64 hex characters (256-bit)".to_string(),
        ));
    }
    Ok(())
}

/// Handle cluster subcommands.
pub fn handle_cluster_command(data_dir: &Path, action: crate::ClusterAction) -> Result<()> {
    use crate::ClusterAction;

    match action {
        ClusterAction::Init { key, port } => {
            let secret = match key {
                Some(k) => {
                    validate_key(&k)?;
                    k
                }
                None => {
                    let k = generate_key();
                    eprintln!("[legion] generated cluster key (save this!):");
                    println!("{k}");
                    k
                }
            };

            let mut config = ClusterConfig::load(data_dir)?;
            config.secret = Some(secret);
            config.port = port;
            config.save(data_dir)?;

            eprintln!("[legion] cluster initialized on port {port}");
            eprintln!("[legion] run `legion cluster enable` to start syncing");
        }

        ClusterAction::Key => {
            let config = ClusterConfig::load(data_dir)?;
            match config.secret {
                Some(key) => println!("{key}"),
                None => {
                    return Err(LegionError::Config(
                        "no cluster key configured. Run `legion cluster init` first.".to_string(),
                    ));
                }
            }
        }

        ClusterAction::Enable => {
            let mut config = ClusterConfig::load(data_dir)?;
            if config.secret.is_none() {
                return Err(LegionError::Config(
                    "no cluster key configured. Run `legion cluster init` first.".to_string(),
                ));
            }
            config.enabled = true;
            config.save(data_dir)?;
            eprintln!("[legion] cluster sync enabled");
            eprintln!("[legion] sync will start on next `legion watch` run");
        }

        ClusterAction::Disable => {
            let mut config = ClusterConfig::load(data_dir)?;
            config.enabled = false;
            config.save(data_dir)?;
            eprintln!("[legion] cluster sync disabled");
        }

        ClusterAction::Status => {
            let config = ClusterConfig::load(data_dir)?;

            println!("Cluster Status");
            println!("--------------");
            println!("enabled:     {}", if config.enabled { "yes" } else { "no" });
            println!(
                "key:         {}",
                if config.secret.is_some() {
                    "configured"
                } else {
                    "not set"
                }
            );
            println!("port:        {}", config.port);
            println!("instance_id: {}", config.resolve_instance_id());
            println!(
                "last_sync:   {}",
                config.last_sync.as_deref().unwrap_or("never")
            );

            // TODO: Show discovered peers when sync actor is running
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn generate_key_produces_valid_hex() {
        let key = generate_key();
        assert_eq!(key.len(), 64);
        validate_key(&key).unwrap();
    }

    #[test]
    fn validate_key_rejects_short_key() {
        let result = validate_key("abcd");
        assert!(result.is_err());
    }

    #[test]
    fn validate_key_rejects_invalid_hex() {
        let result =
            validate_key("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz");
        assert!(result.is_err());
    }

    #[test]
    fn config_round_trip() {
        let dir = TempDir::new().unwrap();
        let config = ClusterConfig {
            secret: Some(generate_key()),
            enabled: true,
            port: 12345,
            ..Default::default()
        };

        config.save(dir.path()).unwrap();
        let loaded = ClusterConfig::load(dir.path()).unwrap();

        assert!(loaded.enabled);
        assert_eq!(loaded.port, 12345);
        assert!(loaded.secret.is_some());
    }

    #[test]
    fn config_preserves_unknown_fields() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("cluster.toml");

        // Write config with an unknown field
        fs::write(
            &path,
            r#"
enabled = true
secret = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
port = 31337
custom_field = "preserved"
"#,
        )
        .unwrap();

        let config = ClusterConfig::load(dir.path()).unwrap();
        config.save(dir.path()).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("custom_field"));
    }
}
