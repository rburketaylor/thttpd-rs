//! Live runtime state: hot-swappable config/weights via `arc-swap`, a drain
//! flag, and an atomically-written `state.json` snapshot.
//!
//! Weight changes arrive over the control socket (see [`crate::control`]) and
//! are applied both to the `ArcSwap<ProxyConfig>` and to the pool's per-backend
//! weights (the router reads weights from the pool). Rollback is *semantic*:
//! the target backend's weight becomes 100 and every other backend's weight
//! becomes 0 — it does NOT rely on `u32::MAX` weights.

use crate::config::{ProxyConfig, RoutingMode};
use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Default grace period (seconds) to let in-flight requests finish during a
/// drain before force-closing stragglers. Overridable per-drain via
/// [`LiveState::set_drain_grace`] (the operator's `drain --timeout` value).
const DEFAULT_DRAIN_GRACE_SECS: u64 = 30;

pub struct LiveState {
    pub config: Arc<ArcSwap<ProxyConfig>>,
    pub draining: Arc<AtomicBool>,
    drain_grace: AtomicU64,
    pub started_at: Instant,
    /// Serializes mutating control commands (set-weight / rollback / drain) and
    /// state-snapshot writes so a concurrent control handler and the periodic
    /// state writer can't interleave: the `ArcSwap` read-modify-write in
    /// [`set_weights`]/[`rollback`] and the `state.json` snapshot+write are each
    /// performed while holding this lock, preventing lost updates and torn
    /// snapshots. Async because it is held across the snapshot+write in async
    /// control/state-writer tasks.
    pub control_lock: tokio::sync::Mutex<()>,
}

impl LiveState {
    pub fn new(cfg: ProxyConfig) -> Self {
        Self {
            config: Arc::new(ArcSwap::from_pointee(cfg)),
            draining: Arc::new(AtomicBool::new(false)),
            drain_grace: AtomicU64::new(DEFAULT_DRAIN_GRACE_SECS),
            started_at: Instant::now(),
            control_lock: tokio::sync::Mutex::new(()),
        }
    }
    pub fn start_drain(&self) {
        self.draining.store(true, Ordering::SeqCst);
    }
    pub fn is_draining(&self) -> bool {
        self.draining.load(Ordering::SeqCst)
    }
    /// Bound (seconds) the drain join gives in-flight requests before
    /// force-closing their connections. Set from the operator's
    /// `drain --timeout` before [`start_drain`].
    pub fn set_drain_grace(&self, secs: u64) {
        self.drain_grace.store(secs.max(1), Ordering::SeqCst);
    }
    pub fn drain_grace(&self) -> Duration {
        Duration::from_secs(self.drain_grace.load(Ordering::SeqCst))
    }
    /// Apply a set of weight overrides to the live config and the pool.
    /// `pool` is the backend pool whose per-backend weights are updated in
    /// lockstep so the router sees them immediately.
    pub fn set_weights(
        &self,
        pool: &crate::backend::BackendPool,
        weights: &std::collections::HashMap<String, u32>,
    ) -> anyhow::Result<()> {
        // Validate names first.
        for name in weights.keys() {
            anyhow::ensure!(
                pool.get(name).is_some(),
                "unknown backend in set-weight: {name}"
            );
        }
        // Reject updates that would leave no routable backend, mirroring
        // config::validate's `total_weight > 0` rule. Otherwise the router has
        // no candidates and returns 503 for every request until repaired.
        let prospective_total: u64 = pool
            .iter()
            .map(|b| weights.get(&b.name).copied().unwrap_or_else(|| b.weight()) as u64)
            .sum();
        anyhow::ensure!(
            prospective_total > 0,
            "set-weight would leave total weight at 0; keep at least one backend > 0"
        );
        // Apply to the pool (router reads weights here).
        for (name, weight) in weights {
            if let Some(b) = pool.get(name) {
                b.set_weight(*weight);
            }
        }
        // Mirror into a fresh ProxyConfig snapshot so state.json reflects it.
        let mut snap = (**self.config.load()).clone();
        for (name, weight) in weights {
            if let Some(b) = snap.backends.get_mut(name) {
                b.weight = *weight;
            }
        }
        self.config.store(Arc::new(snap));
        Ok(())
    }

    /// Semantic rollback: target backend → weight 100, all others → 0.
    ///
    /// In shadow mode, weights do not decide the live backend, so rollback also
    /// promotes the target to `routing.primary_backend` and mirrors to the
    /// previous primary.
    pub fn rollback(&self, pool: &crate::backend::BackendPool, target: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            pool.get(target).is_some(),
            "unknown backend in rollback target: {target}"
        );
        let mut weights = std::collections::HashMap::new();
        for b in pool.iter() {
            weights.insert(b.name.clone(), if b.name == target { 100 } else { 0 });
        }
        for (name, weight) in &weights {
            if let Some(b) = pool.get(name) {
                b.set_weight(*weight);
            }
        }

        let mut snap = (**self.config.load()).clone();
        for (name, weight) in &weights {
            if let Some(b) = snap.backends.get_mut(name) {
                b.weight = *weight;
            }
        }
        if matches!(snap.routing.mode, RoutingMode::Shadow) {
            let previous_primary = snap.routing.primary_backend.clone();
            if previous_primary.as_deref() != Some(target) {
                snap.routing.primary_backend = Some(target.to_string());
                snap.routing.shadow_backend = previous_primary
                    .filter(|name| name != target)
                    .or_else(|| {
                        snap.routing
                            .shadow_backend
                            .clone()
                            .filter(|name| name != target)
                    })
                    .or_else(|| {
                        snap.backends
                            .keys()
                            .find(|name| name.as_str() != target)
                            .cloned()
                    });
            }
        }
        self.config.store(Arc::new(snap));
        Ok(())
    }
}

/// Snapshot of runtime state written to `state.json`.
#[derive(Debug, Serialize, Deserialize)]
pub struct StateSnapshot {
    pub uptime_secs: u64,
    pub draining: bool,
    pub backends: Vec<BackendSnapshot>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BackendSnapshot {
    pub name: String,
    pub address: String,
    pub weight: u32,
    pub health: String,
}

/// Write `state.json` atomically (temp file + rename) so a mid-write reader
/// never sees a partial file.
pub fn write_state_atomic(path: &std::path::Path, snapshot: &StateSnapshot) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(snapshot)?;
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn snapshot(state: &LiveState, pool: &crate::backend::BackendPool) -> StateSnapshot {
    let backends = pool
        .iter()
        .map(|b| BackendSnapshot {
            name: b.name.clone(),
            address: b.config.read().address.clone(),
            weight: b.weight(),
            health: format!("{:?}", b.health()),
        })
        .collect();
    StateSnapshot {
        uptime_secs: state.started_at.elapsed().as_secs(),
        draining: state.is_draining(),
        backends,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BackendPool;
    use crate::config::{BackendConfig, ProxyConfig, RoutingConfig, RoutingMode};
    use std::collections::HashMap;

    fn two_backend_cfg() -> ProxyConfig {
        let mut backends = HashMap::new();
        backends.insert(
            "c".into(),
            BackendConfig {
                address: "127.0.0.1:8081".into(),
                weight: 95,
                health_path: "/".into(),
            },
        );
        backends.insert(
            "rust".into(),
            BackendConfig {
                address: "127.0.0.1:8082".into(),
                weight: 5,
                health_path: "/".into(),
            },
        );
        let cfg = ProxyConfig {
            listen: "127.0.0.1:8080".into(),
            log_level: "info".into(),
            state_path: "/tmp/state.json".into(),
            control_socket: "/tmp/control.sock".into(),
            metrics: Default::default(),
            shadow: Default::default(),
            backends,
            routing: Default::default(),
            health: Default::default(),
            circuit_breaker: Default::default(),
        };
        crate::config::validate(&cfg).ok();
        cfg
    }

    #[test]
    fn weight_update_visible_to_router() {
        let cfg = two_backend_cfg();
        let pool = BackendPool::from_config(&cfg.backends);
        let state = LiveState::new(cfg);
        let mut w = HashMap::new();
        w.insert("rust".into(), 100u32);
        w.insert("c".into(), 0u32);
        state.set_weights(&pool, &w).unwrap();
        assert_eq!(pool.get("rust").unwrap().weight(), 100);
        assert_eq!(pool.get("c").unwrap().weight(), 0);
    }

    #[test]
    fn drain_flag_propagates_immediately() {
        let cfg = two_backend_cfg();
        let state = LiveState::new(cfg);
        assert!(!state.is_draining());
        state.start_drain();
        assert!(state.is_draining());
    }

    #[test]
    fn rollback_is_semantic_not_u32_max_weight() {
        let cfg = two_backend_cfg();
        let pool = BackendPool::from_config(&cfg.backends);
        let state = LiveState::new(cfg);
        state.rollback(&pool, "c").unwrap();
        assert_eq!(pool.get("c").unwrap().weight(), 100);
        assert_eq!(pool.get("rust").unwrap().weight(), 0);
        // Roll back the other way.
        state.rollback(&pool, "rust").unwrap();
        assert_eq!(pool.get("rust").unwrap().weight(), 100);
        assert_eq!(pool.get("c").unwrap().weight(), 0);
    }

    #[test]
    fn shadow_rollback_updates_live_primary_backend() {
        let mut cfg = two_backend_cfg();
        cfg.routing = RoutingConfig {
            mode: RoutingMode::Shadow,
            primary_backend: Some("c".into()),
            shadow_backend: Some("rust".into()),
            exclude_paths: Vec::new(),
        };
        let pool = BackendPool::from_config(&cfg.backends);
        let state = LiveState::new(cfg);

        state.rollback(&pool, "rust").unwrap();

        let snap = state.config.load();
        assert_eq!(snap.routing.primary_backend.as_deref(), Some("rust"));
        assert_eq!(snap.routing.shadow_backend.as_deref(), Some("c"));
        assert_eq!(pool.get("rust").unwrap().weight(), 100);
        assert_eq!(pool.get("c").unwrap().weight(), 0);
    }

    #[test]
    fn rollback_unknown_target_errors() {
        let cfg = two_backend_cfg();
        let pool = BackendPool::from_config(&cfg.backends);
        let state = LiveState::new(cfg);
        assert!(state.rollback(&pool, "nope").is_err());
    }

    #[test]
    fn set_weight_rejects_all_zero() {
        // Regression: a live update that sets every backend to weight 0 must
        // be rejected, otherwise the router has no candidates and 503s all
        // traffic until another control command repairs it.
        let cfg = two_backend_cfg();
        let pool = BackendPool::from_config(&cfg.backends);
        let state = LiveState::new(cfg);
        let mut w = HashMap::new();
        w.insert("c".into(), 0u32);
        w.insert("rust".into(), 0u32);
        let err = state.set_weights(&pool, &w).unwrap_err().to_string();
        assert!(
            err.contains("total weight at 0"),
            "expected all-zero rejection, got: {err}"
        );
        // Weights are unchanged.
        assert_eq!(pool.get("c").unwrap().weight(), 95);
        assert_eq!(pool.get("rust").unwrap().weight(), 5);
    }

    #[test]
    fn set_weight_partial_update_uses_existing() {
        // Updating only one backend keeps the other's current weight; the
        // prospective total must account for unchanged backends.
        let cfg = two_backend_cfg();
        let pool = BackendPool::from_config(&cfg.backends);
        let state = LiveState::new(cfg);
        let mut w = HashMap::new();
        w.insert("c".into(), 0u32); // rust stays at 5 → total 5 > 0, allowed
        state.set_weights(&pool, &w).unwrap();
        assert_eq!(pool.get("c").unwrap().weight(), 0);
        assert_eq!(pool.get("rust").unwrap().weight(), 5);
    }

    #[test]
    fn state_file_written_atomically() {
        let cfg = two_backend_cfg();
        let pool = BackendPool::from_config(&cfg.backends);
        let state = LiveState::new(cfg);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let snap = snapshot(&state, &pool);
        write_state_atomic(&path, &snap).unwrap();
        // The file exists and parses as valid JSON (no partial writes).
        let text = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(v["backends"].is_array());
        // The temp file is gone.
        assert!(!path.with_extension("json.tmp").exists());
    }
}
