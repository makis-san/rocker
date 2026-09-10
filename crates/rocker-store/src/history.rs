//! Time-series and audit history, in `redb` (PLAN §6).
//!
//! Two tables:
//! - `usage` — one row per resource sample, keyed by `(container id, ts_ms)`.
//!   Written at ~1 Hz per open container, pruned on a timer to a retention
//!   window. Sample commits use [`Durability::None`] (a crash loses at most the
//!   last few seconds of graph history); the periodic prune commits with
//!   `Immediate` and doubles as the flush point.
//! - `exec_audit` — one row per completed terminal session, keyed by a
//!   millisecond-plus-sequence stamp so two sessions in the same millisecond
//!   don't collide. Audit rows always commit `Immediate`.
//!
//! Values are JSON so the schema can evolve without a migration; at this volume
//! the size overhead over a packed layout is not worth the fragility.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use redb::{Database, Durability, ReadableDatabase, ReadableTableMetadata, TableDefinition};
use rocker_core::{ExecAudit, StatSample};

use crate::{Result, StoreError};

/// `(container id, ts_ms)` → JSON [`StatSample`].
const USAGE: TableDefinition<(&str, u64), &str> = TableDefinition::new("usage");
/// `ts_ms << 12 | seq` → JSON [`ExecAudit`].
const EXEC_AUDIT: TableDefinition<u64, &str> = TableDefinition::new("exec_audit");

/// Low 12 bits of an audit key are a per-process sequence, so sessions that
/// start in the same millisecond get distinct keys.
const SEQ_BITS: u32 = 12;

fn map<E: Into<redb::Error>>(e: E) -> StoreError {
    StoreError::Db(e.into())
}

pub struct HistoryStore {
    db: Database,
    audit_seq: AtomicU64,
}

impl HistoryStore {
    /// Open (creating if needed) the history database at `path`, making the
    /// parent directory first.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let db = Database::create(path).map_err(map)?;
        // Touch both tables so a fresh db has them and reads never race a
        // first write.
        let txn = db.begin_write().map_err(map)?;
        {
            txn.open_table(USAGE).map_err(map)?;
            txn.open_table(EXEC_AUDIT).map_err(map)?;
        }
        txn.commit().map_err(map)?;
        Ok(Self {
            db,
            audit_seq: AtomicU64::new(0),
        })
    }

    // ---- usage samples ------------------------------------------------

    /// Append one resource sample for `container_id`. Best-effort durability —
    /// see the module docs.
    pub fn record_sample(&self, container_id: &str, s: &StatSample) -> Result<()> {
        let json = serde_json::to_string(s).map_err(|e| StoreError::Parse(e.to_string()))?;
        let mut txn = self.db.begin_write().map_err(map)?;
        txn.set_durability(Durability::None).map_err(map)?;
        {
            let mut t = txn.open_table(USAGE).map_err(map)?;
            t.insert((container_id, s.ts_ms), json.as_str())
                .map_err(map)?;
        }
        txn.commit().map_err(map)?;
        Ok(())
    }

    /// Samples for `container_id` at or after `since_ms`, oldest first,
    /// down-sampled to at most `max_points` by evenly striding (the newest
    /// sample is always kept).
    pub fn samples_since(
        &self,
        container_id: &str,
        since_ms: u64,
        max_points: usize,
    ) -> Result<Vec<StatSample>> {
        let txn = self.db.begin_read().map_err(map)?;
        let t = txn.open_table(USAGE).map_err(map)?;
        let lo = (container_id, since_ms);
        let hi = (container_id, u64::MAX);
        let mut all: Vec<StatSample> = Vec::new();
        for row in t.range(lo..=hi).map_err(map)? {
            let (_, v) = row.map_err(map)?;
            if let Ok(s) = serde_json::from_str::<StatSample>(v.value()) {
                all.push(s);
            }
        }
        Ok(stride(all, max_points))
    }

    /// Drop usage samples older than `before_ms`. Commits `Immediate`, so it
    /// also flushes the `Durability::None` sample writes since the last prune.
    /// Returns how many rows were removed.
    pub fn prune_samples(&self, before_ms: u64) -> Result<u64> {
        let txn = self.db.begin_write().map_err(map)?;
        let removed = {
            let mut t = txn.open_table(USAGE).map_err(map)?;
            let before = t.len().map_err(map)?;
            t.retain(|(_, ts), _| ts >= before_ms).map_err(map)?;
            before - t.len().map_err(map)?
        };
        txn.commit().map_err(map)?;
        Ok(removed)
    }

    // ---- exec audit -------------------------------------------------

    /// Append one completed-session audit row.
    pub fn record_exec(&self, a: &ExecAudit) -> Result<()> {
        let seq = self.audit_seq.fetch_add(1, Ordering::Relaxed) & ((1 << SEQ_BITS) - 1);
        let key = (a.ts_ms << SEQ_BITS) | seq;
        let json = serde_json::to_string(a).map_err(|e| StoreError::Parse(e.to_string()))?;
        let txn = self.db.begin_write().map_err(map)?;
        {
            let mut t = txn.open_table(EXEC_AUDIT).map_err(map)?;
            t.insert(key, json.as_str()).map_err(map)?;
        }
        txn.commit().map_err(map)?;
        Ok(())
    }

    /// The most recent audit rows, newest first, capped at `limit`.
    pub fn recent_exec(&self, limit: usize) -> Result<Vec<ExecAudit>> {
        let txn = self.db.begin_read().map_err(map)?;
        let t = txn.open_table(EXEC_AUDIT).map_err(map)?;
        let mut out = Vec::new();
        for row in t.range::<u64>(..).map_err(map)?.rev() {
            if out.len() >= limit {
                break;
            }
            let (_, v) = row.map_err(map)?;
            if let Ok(a) = serde_json::from_str::<ExecAudit>(v.value()) {
                out.push(a);
            }
        }
        Ok(out)
    }

    /// Drop audit rows whose session started before `before_ms`.
    pub fn prune_exec(&self, before_ms: u64) -> Result<u64> {
        let txn = self.db.begin_write().map_err(map)?;
        let removed = {
            let mut t = txn.open_table(EXEC_AUDIT).map_err(map)?;
            let before = t.len().map_err(map)?;
            t.retain(|k, _| (k >> SEQ_BITS) >= before_ms).map_err(map)?;
            before - t.len().map_err(map)?
        };
        txn.commit().map_err(map)?;
        Ok(removed)
    }
}

/// Evenly reduce `v` to at most `max` elements, spread across the range and
/// always including the first and last.
fn stride<T: Clone>(v: Vec<T>, max: usize) -> Vec<T> {
    if max == 0 || v.len() <= max {
        return v;
    }
    if max == 1 {
        return v.into_iter().next_back().into_iter().collect();
    }
    let n = v.len();
    let mut out: Vec<T> = Vec::with_capacity(max);
    let mut last_idx = usize::MAX;
    for k in 0..max {
        let idx = (k * (n - 1) + (max - 1) / 2) / (max - 1);
        if idx != last_idx {
            out.push(v[idx].clone());
            last_idx = idx;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(ts_ms: u64, cpu: f32) -> StatSample {
        StatSample {
            ts_ms,
            cpu_pct: cpu,
            cpu_cores: 2.0,
            mem_used: 1_000,
            mem_limit: 4_000,
            net_rx: 0,
            net_tx: 0,
            blk_read: 0,
            blk_write: 0,
            pids: 3,
        }
    }

    #[test]
    fn samples_round_trip_and_prune() {
        let dir = std::env::temp_dir().join(format!("rocker-hist-{}", std::process::id()));
        let path = dir.join("h.redb");
        let _ = std::fs::remove_file(&path);
        let h = HistoryStore::open(&path).expect("open");

        for i in 0..10u64 {
            h.record_sample("cid-a", &sample(1_000 + i * 1_000, i as f32))
                .unwrap();
        }
        h.record_sample("cid-b", &sample(5_000, 99.0)).unwrap();

        let got = h.samples_since("cid-a", 4_000, 100).unwrap();
        assert_eq!(got.len(), 7, "ts 4000..=10000 inclusive");
        assert_eq!(got.first().unwrap().ts_ms, 4_000);
        assert_eq!(got.last().unwrap().ts_ms, 10_000);

        // Down-sampling keeps the newest point.
        let few = h.samples_since("cid-a", 0, 3).unwrap();
        assert!(few.len() <= 3 && !few.is_empty());
        assert_eq!(few.last().unwrap().ts_ms, 10_000);

        // cid-a: 1000,2000,3000,4000,5000 dropped (5); cid-b: 5000 dropped (1).
        let removed = h.prune_samples(6_000).unwrap();
        assert_eq!(removed, 6);
        let left_a = h.samples_since("cid-a", 0, 999).unwrap();
        assert!(left_a.iter().all(|s| s.ts_ms >= 6_000));
        assert!(h.samples_since("cid-b", 0, 999).unwrap().is_empty());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn exec_audit_newest_first() {
        let dir = std::env::temp_dir().join(format!("rocker-audit-{}", std::process::id()));
        let path = dir.join("a.redb");
        let _ = std::fs::remove_file(&path);
        let h = HistoryStore::open(&path).expect("open");

        for i in 0..5u64 {
            h.record_exec(&ExecAudit {
                ts_ms: 10_000 + i * 1_000,
                connection_id: "local".into(),
                container: format!("c{i}"),
                container_name: format!("name-{i}"),
                argv: vec!["/bin/sh".into()],
                exit_code: Some(0),
                duration_secs: Some(i),
            })
            .unwrap();
        }
        // Two in the same millisecond must both survive.
        for _ in 0..2 {
            h.record_exec(&ExecAudit {
                ts_ms: 20_000,
                connection_id: "local".into(),
                container: "same".into(),
                container_name: "same".into(),
                argv: vec!["/bin/bash".into()],
                exit_code: None,
                duration_secs: None,
            })
            .unwrap();
        }

        let recent = h.recent_exec(4).unwrap();
        assert_eq!(recent.len(), 4);
        assert_eq!(recent[0].ts_ms, 20_000);
        assert_eq!(recent[1].ts_ms, 20_000);
        assert_eq!(recent[2].ts_ms, 14_000);

        let removed = h.prune_exec(14_000).unwrap();
        assert_eq!(removed, 4, "ts 10000, 11000, 12000, 13000");

        let _ = std::fs::remove_file(&path);
    }
}
