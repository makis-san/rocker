//! One resource-usage sample for a container, already reduced from the Engine's
//! raw cgroup counters into the handful of numbers the Stats tab plots.
//!
//! The CPU delta maths (current vs. previous cgroup totals) happens in
//! `rocker-engine`; by the time a sample reaches a front-end it is a plain
//! reading.

use serde::{Deserialize, Serialize};

/// A single stats reading. Byte counters (`net_*`, `blk_*`) are cumulative
/// since the container started; the UI differentiates consecutive samples to
/// show a rate.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct StatSample {
    /// Wall-clock time the sample was taken, Unix milliseconds. Stamped by the
    /// engine when it emits the sample; the history plot and the `redb` store
    /// both key off it. `0` for a sample built without a clock (tests).
    #[serde(default)]
    pub ts_ms: u64,
    /// CPU use as a percentage of one core (so 250.0 == 2.5 cores busy).
    pub cpu_pct: f32,
    /// Cores visible to the container, for scaling the CPU axis.
    pub cpu_cores: f32,
    pub mem_used: u64,
    pub mem_limit: u64,
    /// Cumulative bytes received / sent across all interfaces.
    pub net_rx: u64,
    pub net_tx: u64,
    /// Cumulative bytes read / written to block devices.
    pub blk_read: u64,
    pub blk_write: u64,
    pub pids: u64,
}

impl StatSample {
    /// Memory use as a fraction of the limit, clamped to `0.0..=1.0`.
    pub fn mem_frac(&self) -> f32 {
        if self.mem_limit == 0 {
            return 0.0;
        }
        (self.mem_used as f64 / self.mem_limit as f64).clamp(0.0, 1.0) as f32
    }
}
