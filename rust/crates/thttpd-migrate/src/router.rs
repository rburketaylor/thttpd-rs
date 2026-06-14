//! Weighted request routing across healthy backends.
//!
//! Active-active and canary mode select a backend by weighted random draw.
//! Shadow mode always serves the configured primary backend and, separately,
//! mirrors the request to the shadow backend (handled in [`crate::shadow`]).

use crate::backend::{Backend, BackendPool};
use crate::config::{RoutingConfig, RoutingMode};
use hyper::Request;
use rand::Rng;
use std::sync::Arc;
use std::time::Instant;

/// Clone so a shadow task can take its own copy into a `tokio::spawn` future.
#[derive(Clone)]
pub struct RoutingDecision {
    pub backend: Arc<Backend>,
    pub shadow: Option<Arc<Backend>>, // for shadow mode
    pub request_id: String,
    pub started_at: Instant,
}

/// `_req` is intentionally unused: path-exclusion is decided by the server handler.
/// It is kept in the signature so future request-affinity routing has a hook.
pub fn decide<B>(
    _req: &Request<B>,
    pool: &BackendPool,
    routing: &RoutingConfig,
) -> Option<RoutingDecision> {
    let request_id = uuid::Uuid::new_v4().to_string();

    let chosen = match routing.mode {
        // Shadow mode must always serve the configured primary backend.
        RoutingMode::Shadow => {
            let name = routing.primary_backend.as_deref()?;
            let backend = pool.get(name)?;
            if !backend.is_routable() || !pool.breaker_can_route(&backend.name) {
                return None;
            }
            if !pool.breaker_admit(&backend.name) {
                return None;
            }
            backend
        }
        // Canary is operationally distinct but mechanically the same weighted
        // selection as active-active.
        RoutingMode::ActiveActive | RoutingMode::Canary => {
            // Enumerate candidates with NON-MUTATING eligibility so cooled-open
            // breakers are not promoted to half-open (and their single probe
            // claimed) during filtering. Only the backend actually selected
            // below may claim a probe via `breaker_admit`.
            let mut candidates: Vec<Arc<Backend>> = pool
                .iter()
                .filter(|b| b.is_routable() && b.weight() > 0 && pool.breaker_can_route(&b.name))
                .cloned()
                .collect();
            if candidates.is_empty() {
                return None;
            }
            // Select by weighted random draw, then claim the probe on the
            // chosen backend. If admission loses the single probe slot to a
            // concurrent caller (`breaker_admit` false — the only way
            // can_route=true yet admit=false), drop that backend and retry
            // among the remaining candidates. Return None only when no
            // eligible/admittable candidate remains.
            let chosen = loop {
                let total: u32 = candidates.iter().map(|b| b.weight()).sum();
                if total == 0 {
                    break None;
                }
                let mut rng = rand::rng();
                let pick = rng.random_range(0..total);
                let mut acc = 0u32;
                let mut selected: Option<Arc<Backend>> = None;
                for b in &candidates {
                    acc += b.weight();
                    if pick < acc {
                        selected = Some(b.clone());
                        break;
                    }
                }
                // Fall back to the last candidate (defensive against rounding).
                let selected = match selected.or_else(|| candidates.last().cloned()) {
                    Some(b) => b,
                    None => break None,
                };
                if pool.breaker_admit(&selected.name) {
                    break Some(selected);
                }
                // Admission lost the probe race: remove this backend and retry.
                candidates.retain(|b| b.name != selected.name);
                if candidates.is_empty() {
                    break None;
                }
            };
            chosen?
        }
    };

    let shadow = if matches!(routing.mode, RoutingMode::Shadow) {
        // Shadow eligibility is checked non-mutating first, then the probe is
        // claimed only on the shadow backend that will actually receive the
        // mirrored request. If admission loses the single probe slot to a
        // concurrent caller, skip mirroring for this request rather than
        // leaking a probe.
        routing
            .shadow_backend
            .as_deref()
            .and_then(|n| pool.get(n))
            .filter(|b| b.name != chosen.name && b.is_routable() && pool.breaker_can_route(&b.name))
            .filter(|b| pool.breaker_admit(&b.name))
    } else {
        None
    };

    Some(RoutingDecision {
        backend: chosen,
        shadow,
        request_id,
        started_at: Instant::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Health;
    use crate::circuit::State;
    use crate::config::BackendConfig;
    use std::collections::HashMap;

    fn pool(weights: &[(&str, u32)]) -> BackendPool {
        let mut backends = HashMap::new();
        for (name, weight) in weights {
            backends.insert(
                (*name).to_string(),
                BackendConfig {
                    address: format!("127.0.0.1:80{}", 80 + name.len()),
                    weight: *weight,
                    health_path: "/".into(),
                },
            );
        }
        BackendPool::from_config(&backends)
    }

    fn active_active() -> RoutingConfig {
        RoutingConfig {
            mode: RoutingMode::ActiveActive,
            ..Default::default()
        }
    }

    /// Build a pool with an explicit breaker config (used to make breakers
    /// trip on a single failure for deterministic regression tests).
    fn pool_with_cfg(weights: &[(&str, u32)], cfg: crate::config::CircuitConfig) -> BackendPool {
        let mut backends = HashMap::new();
        for (name, weight) in weights {
            backends.insert(
                (*name).to_string(),
                BackendConfig {
                    address: format!("127.0.0.1:{}", 9000 + name.len()),
                    weight: *weight,
                    health_path: "/".into(),
                },
            );
        }
        BackendPool::with_breaker_cfg(&backends, cfg)
    }

    /// A sensitive breaker config: a single failure trips it.
    fn sensitive_cfg() -> crate::config::CircuitConfig {
        crate::config::CircuitConfig {
            error_rate_threshold: 0.5,
            window_secs: 30,
            min_requests: 1,
        }
    }

    /// Trip `name`'s breaker then force cool-off so it is eligible for a
    /// half-open probe (can_route == true, probe unclaimed).
    fn trip_and_cool(pool: &BackendPool, name: &str) {
        let b = pool.breakers.get(name).unwrap();
        b.record(false); // min_requests=1, 100% error rate -> trips immediately
        assert_eq!(b.state(), State::Open, "{name} should be tripped");
        b.force_cooled();
        assert!(b.can_route(), "cooled + probe-free => candidate");
        assert!(!b.probe_claimed());
    }

    #[test]
    fn weighted_selection_respects_weights() {
        let pool = pool(&[("a", 90), ("b", 10)]);
        let routing = active_active();
        let req = Request::builder().body(()).unwrap();
        let mut a_hits = 0usize;
        let total = 20_000;
        for _ in 0..total {
            let d = decide(&req, &pool, &routing).unwrap();
            if d.backend.name == "a" {
                a_hits += 1;
            }
        }
        let ratio = a_hits as f64 / total as f64;
        // Expect ~0.90; allow ±5%.
        assert!(
            (0.85..=0.95).contains(&ratio),
            "expected ~0.90, got {ratio}"
        );
    }

    #[test]
    fn unhealthy_backends_excluded() {
        let pool = pool(&[("a", 50), ("b", 50)]);
        pool.get("b").unwrap().health.store(
            Health::Unhealthy as u8,
            std::sync::atomic::Ordering::Relaxed,
        );
        let routing = active_active();
        let req = Request::builder().body(()).unwrap();
        for _ in 0..1000 {
            let d = decide(&req, &pool, &routing).unwrap();
            assert_eq!(
                d.backend.name, "a",
                "unhealthy backend must never be picked"
            );
        }
    }

    #[test]
    fn zero_weight_excluded() {
        let pool = pool(&[("a", 0), ("b", 1)]);
        let routing = active_active();
        let req = Request::builder().body(()).unwrap();
        for _ in 0..1000 {
            let d = decide(&req, &pool, &routing).unwrap();
            assert_eq!(d.backend.name, "b");
        }
    }

    #[test]
    fn no_routable_returns_none() {
        let pool = pool(&[("a", 1)]);
        pool.get("a").unwrap().health.store(
            Health::Unhealthy as u8,
            std::sync::atomic::Ordering::Relaxed,
        );
        let routing = active_active();
        let req = Request::builder().body(()).unwrap();
        assert!(decide(&req, &pool, &routing).is_none());
    }

    #[test]
    fn shadow_mode_always_picks_primary() {
        let pool = pool(&[("a", 0), ("b", 100)]);
        let routing = RoutingConfig {
            mode: RoutingMode::Shadow,
            primary_backend: Some("a".into()),
            shadow_backend: Some("b".into()),
            ..Default::default()
        };
        let req = Request::builder().body(()).unwrap();
        for _ in 0..1000 {
            let d = decide(&req, &pool, &routing).unwrap();
            assert_eq!(d.backend.name, "a", "shadow mode must serve the primary");
            assert_eq!(d.shadow.unwrap().name, "b");
        }
    }

    #[test]
    fn shadow_mode_primary_obeys_breaker() {
        let pool = pool_with_cfg(&[("a", 1), ("b", 1)], sensitive_cfg());
        pool.record_outcome("a", false);
        assert_eq!(pool.breakers.get("a").unwrap().state(), State::Open);
        let routing = RoutingConfig {
            mode: RoutingMode::Shadow,
            primary_backend: Some("a".into()),
            shadow_backend: Some("b".into()),
            ..Default::default()
        };
        let req = Request::builder().body(()).unwrap();
        assert!(
            decide(&req, &pool, &routing).is_none(),
            "shadow primary must not bypass an open breaker"
        );
    }

    #[test]
    fn shadow_mode_primary_claims_half_open_probe() {
        let pool = pool_with_cfg(&[("a", 1), ("b", 1)], sensitive_cfg());
        trip_and_cool(&pool, "a");
        let routing = RoutingConfig {
            mode: RoutingMode::Shadow,
            primary_backend: Some("a".into()),
            shadow_backend: Some("b".into()),
            ..Default::default()
        };
        let req = Request::builder().body(()).unwrap();
        let d = decide(&req, &pool, &routing).expect("cooled primary should get one probe");
        assert_eq!(d.backend.name, "a");
        let breaker = pool.breakers.get("a").unwrap();
        assert_eq!(breaker.state(), State::HalfOpen);
        assert!(breaker.probe_claimed());
        assert!(
            decide(&req, &pool, &routing).is_none(),
            "second shadow request must not steal the in-flight probe"
        );
    }

    #[test]
    fn candidate_filtering_does_not_consume_nonselected_probe() {
        // P1 regression (half-open probe consumed before selection):
        // enumerating candidates must NOT claim a cooled-open breaker's probe.
        // After deciding, exactly the SELECTED backend is admitted (HalfOpen,
        // probe claimed); the NON-selected backend stays Open with its probe
        // untouched — otherwise it would be stuck HalfOpen forever with no
        // outcome to resolve it.
        let pool = pool_with_cfg(&[("a", 50), ("b", 50)], sensitive_cfg());
        trip_and_cool(&pool, "a");
        trip_and_cool(&pool, "b");
        let routing = active_active();
        let req = Request::builder().body(()).unwrap();
        let d = decide(&req, &pool, &routing).expect("at least one candidate");

        let (sel, other) = if d.backend.name == "a" {
            ("a", "b")
        } else {
            ("b", "a")
        };
        let sel_br = pool.breakers.get(sel).unwrap();
        let other_br = pool.breakers.get(other).unwrap();
        // Selected: admitted -> HalfOpen, probe claimed.
        assert_eq!(sel_br.state(), State::HalfOpen, "selected backend admitted");
        assert!(sel_br.probe_claimed(), "selected backend claimed the probe");
        // Non-selected: untouched -> Open, no probe claimed (the bug would
        // leave it HalfOpen with a claimed probe and no resolving outcome).
        assert_eq!(
            other_br.state(),
            State::Open,
            "non-selected must NOT be promoted to HalfOpen"
        );
        assert!(
            !other_br.probe_claimed(),
            "non-selected probe must NOT be claimed"
        );
        // It remains a valid candidate for the next request.
        assert!(other_br.can_route());
    }

    #[test]
    fn selected_cooled_backend_admits_and_resolves() {
        // The selected cooled-open backend is admitted (enters HalfOpen, allows
        // only one probe), then resolves cleanly via record_outcome.
        let pool = pool_with_cfg(&[("a", 1), ("b", 1)], sensitive_cfg());
        trip_and_cool(&pool, "a");
        trip_and_cool(&pool, "b");
        let routing = active_active();
        let req = Request::builder().body(()).unwrap();
        let d = decide(&req, &pool, &routing).expect("candidate");

        let sel_br = pool.breakers.get(&d.backend.name).unwrap();
        assert_eq!(sel_br.state(), State::HalfOpen);
        assert!(sel_br.probe_claimed());
        // Only one probe: a second admit on the now-HalfOpen backend is denied.
        assert!(!sel_br.try_admit(), "HalfOpen admits only one probe");
        // Resolving with a success closes the circuit and releases the slot.
        pool.record_outcome(&d.backend.name, true);
        assert_eq!(sel_br.state(), State::Closed);
        assert!(!sel_br.probe_claimed());
    }

    #[test]
    fn decide_returns_none_when_all_candidates_open_uncooled() {
        // All backends open and NOT cooled -> can_route false -> no candidates.
        let pool = pool_with_cfg(&[("a", 1), ("b", 1)], sensitive_cfg());
        for name in ["a", "b"] {
            let b = pool.breakers.get(name).unwrap();
            b.record(false);
            assert_eq!(b.state(), State::Open, "{name} tripped");
            // deliberately NOT cooled
        }
        let routing = active_active();
        let req = Request::builder().body(()).unwrap();
        assert!(decide(&req, &pool, &routing).is_none());
    }

    #[test]
    fn retry_on_admit_race_claims_one_probe_per_backend() {
        // `can_route` true yet `breaker_admit` false happens only under the
        // concurrent race (two callers pick the same cooled-open backend).
        // The retry path must claim exactly one probe per backend and never
        // return None while a candidate remains. No outcomes are recorded, so
        // each backend admits at most once; all four must eventually be
        // admitted despite contention.
        use std::sync::atomic::{AtomicUsize, Ordering};
        let pool = pool_with_cfg(&[("a", 1), ("b", 1), ("c", 1), ("d", 1)], sensitive_cfg());
        for name in ["a", "b", "c", "d"] {
            trip_and_cool(&pool, name);
        }
        let routing = active_active();
        let successes = AtomicUsize::new(0);
        let nones = AtomicUsize::new(0);
        std::thread::scope(|s| {
            for _ in 0..8 {
                s.spawn(|| {
                    let req = Request::builder().body(()).unwrap();
                    for _ in 0..200 {
                        match decide(&req, &pool, &routing) {
                            Some(_) => {
                                successes.fetch_add(1, Ordering::Relaxed);
                            }
                            None => {
                                nones.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                });
            }
        });
        assert_eq!(
            successes.load(Ordering::Relaxed),
            4,
            "exactly one probe claimed per backend, no premature None"
        );
        assert!(
            nones.load(Ordering::Relaxed) > 0,
            "candidates exhaust once all probes are claimed"
        );
    }
}
