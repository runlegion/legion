use std::collections::VecDeque;

use uuid::Uuid;

use crate::error::Result;

/// A persisted system health sample for telemetry and spawn gating.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HealthSample {
    pub id: String,
    pub hostname: String,
    pub sampled_at: String,
    pub cpu_usage_pct: f64,
    pub load_avg_1: Option<f64>,
    pub load_avg_5: Option<f64>,
    pub load_avg_15: Option<f64>,
    pub cpu_core_count: i32,
    pub mem_total_bytes: i64,
    pub mem_used_bytes: i64,
    pub mem_usage_pct: f64,
    pub swap_total_bytes: Option<i64>,
    pub swap_used_bytes: Option<i64>,
    pub cpu_temp_celsius: Option<f64>,
    pub agents_active: i32,
    pub pressure: f64,
}

impl HealthSample {
    /// Compute swap usage percentage from total/used bytes.
    pub fn swap_pct(&self) -> f64 {
        match (self.swap_total_bytes, self.swap_used_bytes) {
            (Some(total), Some(used)) if total > 0 => (used as f64 / total as f64) * 100.0,
            _ => 0.0,
        }
    }
}

/// A single point-in-time system snapshot (not persisted directly).
#[derive(Debug, Clone)]
pub struct HealthSnapshot {
    pub cpu_usage_pct: f64,
    pub load_avg_1: Option<f64>,
    pub load_avg_5: Option<f64>,
    pub load_avg_15: Option<f64>,
    pub cpu_core_count: i32,
    pub mem_total_bytes: i64,
    pub mem_used_bytes: i64,
    pub mem_usage_pct: f64,
    pub swap_total_bytes: Option<i64>,
    pub swap_used_bytes: Option<i64>,
    pub cpu_temp_celsius: Option<f64>,
}

/// Collects system metrics and computes a composite pressure score.
pub struct HealthSampler {
    system: sysinfo::System,
    components: sysinfo::Components,
    window: VecDeque<f64>,
    window_size: usize,
    hostname: String,
    last_snapshot: Option<HealthSnapshot>,
}

impl HealthSampler {
    /// Create a new sampler. Calls `refresh_cpu_usage()` once so the next
    /// `sample()` call returns real values (sysinfo needs two calls).
    pub fn new(window_size: usize) -> Self {
        let mut system = sysinfo::System::new();
        system.refresh_cpu_usage();

        let hostname = sysinfo::System::host_name().unwrap_or_else(|| {
            eprintln!("[legion] warning: could not determine hostname, using 'unknown'");
            "unknown".to_string()
        });

        Self {
            system,
            components: sysinfo::Components::new_with_refreshed_list(),
            window: VecDeque::with_capacity(window_size),
            window_size,
            hostname,
            last_snapshot: None,
        }
    }

    /// Take a system snapshot, push pressure to rolling window, return the snapshot.
    pub fn sample(&mut self) -> HealthSnapshot {
        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        self.components.refresh(false);

        let cpu_usage_pct: f64 = self.system.global_cpu_usage() as f64;
        let cpu_core_count: i32 = self.system.cpus().len() as i32;

        let load_avg = sysinfo::System::load_average();
        // On Windows, load averages are always 0.0 -- store as None
        let (load_avg_1, load_avg_5, load_avg_15) = if cfg!(windows) {
            (None, None, None)
        } else {
            (
                Some(load_avg.one),
                Some(load_avg.five),
                Some(load_avg.fifteen),
            )
        };

        let mem_total: i64 = self.system.total_memory() as i64;
        let mem_used: i64 = self.system.used_memory() as i64;
        let mem_usage_pct: f64 = if mem_total > 0 {
            (mem_used as f64 / mem_total as f64) * 100.0
        } else {
            0.0
        };

        let swap_total: i64 = self.system.total_swap() as i64;
        let swap_used: i64 = self.system.used_swap() as i64;
        let (swap_total_opt, swap_used_opt) = if swap_total > 0 {
            (Some(swap_total), Some(swap_used))
        } else {
            (None, None)
        };

        // Find the hottest CPU-related component
        let cpu_temp: Option<f64> = self
            .components
            .iter()
            .filter(|c| {
                let label: &str = c.label();
                label.contains("CPU")
                    || label.contains("cpu")
                    || label.contains("Core")
                    || label.contains("core")
            })
            .filter_map(|c| c.temperature().map(|t| t as f64))
            .reduce(f64::max);

        let snapshot = HealthSnapshot {
            cpu_usage_pct,
            load_avg_1,
            load_avg_5,
            load_avg_15,
            cpu_core_count,
            mem_total_bytes: mem_total,
            mem_used_bytes: mem_used,
            mem_usage_pct,
            swap_total_bytes: swap_total_opt,
            swap_used_bytes: swap_used_opt,
            cpu_temp_celsius: cpu_temp,
        };

        let pressure: f64 = compute_pressure(&snapshot);
        if self.window.len() >= self.window_size {
            self.window.pop_front();
        }
        self.window.push_back(pressure);

        self.last_snapshot = Some(snapshot.clone());
        snapshot
    }

    /// Compute rolling-window average pressure (0.0-100.0).
    pub fn pressure(&self) -> f64 {
        if self.window.is_empty() {
            return 0.0;
        }
        let sum: f64 = self.window.iter().sum();
        sum / self.window.len() as f64
    }

    /// Convert latest snapshot + pressure to a persistable HealthSample.
    pub fn to_health_sample(&self, agents_active: i32) -> Result<HealthSample> {
        let snap: &HealthSnapshot = self.last_snapshot.as_ref().ok_or_else(|| {
            crate::error::LegionError::Health(
                "sample() must be called before to_health_sample()".to_string(),
            )
        })?;

        Ok(HealthSample {
            id: Uuid::now_v7().to_string(),
            hostname: self.hostname.clone(),
            sampled_at: chrono::Utc::now().to_rfc3339(),
            cpu_usage_pct: snap.cpu_usage_pct,
            load_avg_1: snap.load_avg_1,
            load_avg_5: snap.load_avg_5,
            load_avg_15: snap.load_avg_15,
            cpu_core_count: snap.cpu_core_count,
            mem_total_bytes: snap.mem_total_bytes,
            mem_used_bytes: snap.mem_used_bytes,
            mem_usage_pct: snap.mem_usage_pct,
            swap_total_bytes: snap.swap_total_bytes,
            swap_used_bytes: snap.swap_used_bytes,
            cpu_temp_celsius: snap.cpu_temp_celsius,
            agents_active,
            pressure: self.pressure(),
        })
    }

    /// Should we spawn? Returns false if rolling pressure exceeds threshold.
    /// Returns true if no data yet (allow spawning by default).
    pub fn can_spawn(&self, threshold: f64) -> bool {
        if self.window.is_empty() {
            return true;
        }
        self.pressure() < threshold
    }

    pub fn hostname(&self) -> &str {
        &self.hostname
    }
}

/// Compute instantaneous pressure from a single snapshot.
///
/// Takes the worst of CPU usage and memory usage, with penalties
/// for swap usage and thermal throttling.
fn compute_pressure(snapshot: &HealthSnapshot) -> f64 {
    let mut pressure: f64 = snapshot.cpu_usage_pct;

    // Memory pressure takes priority if higher
    pressure = pressure.max(snapshot.mem_usage_pct);

    // Swap usage contributes directly -- no penalty multiplier.
    // On workstations (16GB Macs), swap at 70-85% is normal during
    // multi-agent sessions, not an emergency.
    if let (Some(total), Some(used)) = (snapshot.swap_total_bytes, snapshot.swap_used_bytes)
        && total > 0
    {
        let swap_pct: f64 = (used as f64 / total as f64) * 100.0;
        pressure = pressure.max(swap_pct);
    }

    // Thermal throttling -- if CPU temp > 90C, hard pressure
    if let Some(temp) = snapshot.cpu_temp_celsius {
        if temp > 90.0 {
            pressure = pressure.max(95.0);
        } else if temp > 80.0 {
            pressure = pressure.max(temp);
        }
    }

    pressure.clamp(0.0, 100.0)
}

// -- Display helpers for CLI (#91) -------------------------------------------

/// Render a bar gauge of width 20 using pipe characters.
pub fn render_gauge(pct: f64, width: usize) -> String {
    let filled: usize = ((pct / 100.0) * width as f64).round() as usize;
    let filled: usize = filled.min(width);
    let empty: usize = width - filled;
    format!("{}{}", "|".repeat(filled), ".".repeat(empty))
}

/// Format bytes as a human-readable size (e.g., "12.4 GB").
pub fn format_bytes(bytes: i64) -> String {
    let gb: f64 = bytes as f64 / 1_073_741_824.0;
    if gb >= 1.0 {
        format!("{:.1} GB", gb)
    } else {
        let mb: f64 = bytes as f64 / 1_048_576.0;
        format!("{:.0} MB", mb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampler_returns_data_after_warmup() {
        let mut sampler = HealthSampler::new(6);
        std::thread::sleep(std::time::Duration::from_millis(250));
        let snap = sampler.sample();
        // We are running, so at least some metrics should be populated
        assert!(snap.cpu_usage_pct >= 0.0);
        assert!(snap.mem_total_bytes > 0);
        assert!(snap.mem_used_bytes > 0);
        assert!(snap.cpu_core_count > 0);
    }

    #[test]
    fn pressure_reflects_worst_metric() {
        // High memory, low CPU -- pressure should be at least memory %
        let snap = HealthSnapshot {
            cpu_usage_pct: 10.0,
            load_avg_1: None,
            load_avg_5: None,
            load_avg_15: None,
            cpu_core_count: 4,
            mem_total_bytes: 16_000_000_000,
            mem_used_bytes: 12_000_000_000,
            mem_usage_pct: 75.0,
            swap_total_bytes: None,
            swap_used_bytes: None,
            cpu_temp_celsius: None,
        };
        let p: f64 = compute_pressure(&snap);
        assert!(p >= 75.0, "pressure {} should be >= 75.0 (mem)", p);
    }

    #[test]
    fn can_spawn_respects_threshold() {
        let mut sampler = HealthSampler::new(6);
        // Push known pressure values
        sampler.window.push_back(50.0);
        sampler.window.push_back(60.0);
        // avg = 55.0
        assert!(sampler.can_spawn(80.0), "should spawn below threshold");
        assert!(!sampler.can_spawn(50.0), "should not spawn above threshold");
    }

    #[test]
    fn rolling_window_caps_at_size() {
        let mut sampler = HealthSampler::new(3);
        for i in 0..8 {
            sampler.window.push_back(i as f64 * 10.0);
            if sampler.window.len() > sampler.window_size {
                sampler.window.pop_front();
            }
        }
        assert_eq!(sampler.window.len(), 3);
    }

    #[test]
    fn swap_contributes_to_pressure() {
        let snap = HealthSnapshot {
            cpu_usage_pct: 30.0,
            load_avg_1: None,
            load_avg_5: None,
            load_avg_15: None,
            cpu_core_count: 4,
            mem_total_bytes: 16_000_000_000,
            mem_used_bytes: 6_000_000_000,
            mem_usage_pct: 37.5,
            swap_total_bytes: Some(4_000_000_000),
            swap_used_bytes: Some(2_000_000_000), // 50% swap
            cpu_temp_celsius: None,
        };
        let p: f64 = compute_pressure(&snap);
        // 50% swap is the highest metric, no penalty added
        assert!(
            (p - 50.0).abs() < 0.1,
            "pressure {} should be ~50.0 (swap direct)",
            p
        );
    }

    #[test]
    fn thermal_throttle_increases_pressure() {
        let snap = HealthSnapshot {
            cpu_usage_pct: 30.0,
            load_avg_1: None,
            load_avg_5: None,
            load_avg_15: None,
            cpu_core_count: 4,
            mem_total_bytes: 16_000_000_000,
            mem_used_bytes: 6_000_000_000,
            mem_usage_pct: 37.5,
            swap_total_bytes: None,
            swap_used_bytes: None,
            cpu_temp_celsius: Some(92.0),
        };
        let p: f64 = compute_pressure(&snap);
        assert!(p >= 95.0, "pressure {} should be >= 95.0 (thermal)", p);
    }

    #[test]
    fn pressure_empty_window_returns_zero() {
        let sampler = HealthSampler::new(6);
        assert!((sampler.pressure() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn can_spawn_true_on_empty_window() {
        let sampler = HealthSampler::new(6);
        assert!(sampler.can_spawn(80.0), "empty window should allow spawn");
    }

    #[test]
    fn render_gauge_basic() {
        assert_eq!(render_gauge(50.0, 20), "||||||||||..........");
        assert_eq!(render_gauge(0.0, 20), "....................");
        assert_eq!(render_gauge(100.0, 20), "||||||||||||||||||||");
    }

    #[test]
    fn format_bytes_gb() {
        assert_eq!(format_bytes(16_000_000_000), "14.9 GB");
        assert_eq!(format_bytes(1_073_741_824), "1.0 GB");
    }

    #[test]
    fn format_bytes_mb() {
        assert_eq!(format_bytes(500_000_000), "477 MB");
    }

    #[test]
    fn to_health_sample_roundtrip() {
        let mut sampler = HealthSampler::new(6);
        std::thread::sleep(std::time::Duration::from_millis(250));
        sampler.sample();
        let hs: HealthSample = sampler.to_health_sample(3).unwrap();
        assert_eq!(hs.agents_active, 3);
        assert!(!hs.id.is_empty());
        assert!(!hs.hostname.is_empty());
        assert!(!hs.sampled_at.is_empty());
    }
}
