use crate::config::BackendConfig;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Healthy = 0,
    Degraded = 1,
    Unhealthy = 2,
}

impl Health {
    pub fn from_u8(value: u8) -> Self {
        match value {
            0 => Health::Healthy,
            1 => Health::Degraded,
            _ => Health::Unhealthy,
        }
    }
}

pub struct Backend {
    pub name: String,
    pub config: parking_lot::RwLock<BackendConfig>,
    pub health: AtomicU8, // Health
    pub consecutive_failures: AtomicU32,
    pub consecutive_successes: AtomicU32,
}

impl Backend {
    pub fn new(name: String, config: BackendConfig) -> Arc<Self> {
        Arc::new(Self {
            name,
            config: parking_lot::RwLock::new(config),
            health: AtomicU8::new(Health::Healthy as u8),
            consecutive_failures: AtomicU32::new(0),
            consecutive_successes: AtomicU32::new(0),
        })
    }
    pub fn health(&self) -> Health {
        Health::from_u8(self.health.load(Ordering::Relaxed))
    }
    pub fn is_routable(&self) -> bool {
        self.health() != Health::Unhealthy
    }
    /// Current routing weight (hot-swappable via the control plane in Phase 7).
    pub fn weight(&self) -> u32 {
        self.config.read().weight
    }
    pub fn set_weight(&self, weight: u32) {
        self.config.write().weight = weight;
    }
}

pub struct BackendPool {
    pub backends: HashMap<String, Arc<Backend>>,
    pub breakers: crate::circuit::Breakers,
}

impl BackendPool {
    pub fn from_config(backends: &HashMap<String, BackendConfig>) -> Self {
        let map = backends
            .iter()
            .map(|(name, cfg)| (name.clone(), Backend::new(name.clone(), cfg.clone())))
            .collect::<HashMap<_, _>>();
        let breakers = crate::circuit::Breakers::from_config(
            backends,
            crate::config::CircuitConfig::default(),
        );
        Self {
            backends: map,
            breakers,
        }
    }
    pub fn get(&self, name: &str) -> Option<Arc<Backend>> {
        self.backends.get(name).cloned()
    }
    pub fn iter(&self) -> impl Iterator<Item = &Arc<Backend>> {
        self.backends.values()
    }
    /// Non-mutating eligibility: can `name` be considered as a routing
    /// candidate right now? Safe to call while enumerating candidates — it
    /// never stores state or claims a probe. See [`Breaker::can_route`].
    pub fn breaker_can_route(&self, name: &str) -> bool {
        self.breakers
            .get(name)
            .map(|b| b.can_route())
            .unwrap_or(true)
    }

    /// Side-effectful probe admission for `name`. Call **only** on the backend
    /// actually selected for routing or shadow mirroring: it claims the single
    /// half-open probe. See [`Breaker::try_admit`].
    pub fn breaker_admit(&self, name: &str) -> bool {
        self.breakers
            .get(name)
            .map(|b| b.try_admit())
            .unwrap_or(true)
    }

    /// Release a claimed probe for `name` without recording an outcome.
    /// See [`crate::circuit::Breaker::release_probe`].
    pub fn breaker_release_probe(&self, name: &str) {
        if let Some(b) = self.breakers.get(name) {
            b.release_probe();
        }
    }

    /// True if the backend's circuit breaker currently allows traffic.
    ///
    /// **Deprecated for routing paths.** This is side-effectful (it claims a
    /// half-open probe), so filtering candidates with it leaks probes onto
    /// backends that are never selected. Use [`breaker_can_route`] for
    /// candidate filtering and [`breaker_admit`] on the selected backend.
    /// Retained for tests and backward compatibility.
    pub fn breaker_allows(&self, name: &str) -> bool {
        self.breakers.get(name).map(|b| b.allows()).unwrap_or(true)
    }

    /// Record a request outcome into the named backend's circuit breaker.
    pub fn record_outcome(&self, name: &str, success: bool) {
        if let Some(b) = self.breakers.get(name) {
            b.record(success);
        }
    }
}

impl BackendPool {
    pub fn with_breaker_cfg(
        backends: &HashMap<String, BackendConfig>,
        cfg: crate::config::CircuitConfig,
    ) -> Self {
        let map = backends
            .iter()
            .map(|(name, cfg)| (name.clone(), Backend::new(name.clone(), cfg.clone())))
            .collect::<HashMap<_, _>>();
        let breakers = crate::circuit::Breakers::from_config(backends, cfg);
        Self {
            backends: map,
            breakers,
        }
    }
}
