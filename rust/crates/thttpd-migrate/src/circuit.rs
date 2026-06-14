//! Circuit breaker.
//!
//! A per-backend rolling window of request outcomes. The circuit trips (opens)
//! when the error rate over the window exceeds the configured threshold AND
//! the minimum request volume is met. After a cool-off, the breaker enters
//! half-open and allows a single probe; a success closes it, a failure
//! re-opens it.

use crate::config::CircuitConfig;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Closed = 0,
    Open = 1,
    HalfOpen = 2,
}

impl State {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => State::Closed,
            1 => State::Open,
            _ => State::HalfOpen,
        }
    }
}

/// Cool-off before an open breaker allows a half-open probe.
const COOL_OFF: Duration = Duration::from_secs(5);

struct Window {
    errors: u32,
    total: u32,
    started: Instant,
}

pub struct Breaker {
    state: AtomicU8,
    opened_at: Mutex<Option<Instant>>,
    /// Guards half-open so exactly one probe request reaches the backend.
    /// Claimed (set true) atomically when promoting Open→HalfOpen; released in
    /// [`record`] when the probe's outcome resolves the state.
    probe_in_flight: AtomicBool,
    window: Mutex<Window>,
    cfg: CircuitConfig,
}

impl Breaker {
    pub fn new(cfg: CircuitConfig) -> Self {
        Self {
            state: AtomicU8::new(State::Closed as u8),
            opened_at: Mutex::new(None),
            probe_in_flight: AtomicBool::new(false),
            window: Mutex::new(Window {
                errors: 0,
                total: 0,
                started: Instant::now(),
            }),
            cfg,
        }
    }

    /// Record a request outcome and update circuit state.
    pub fn record(&self, success: bool) {
        // If half-open: a single outcome resolves the probe. Release the probe
        // slot first so the next cool-off cycle can admit another probe.
        if self.state() == State::HalfOpen {
            self.probe_in_flight.store(false, Ordering::Release);
            if success {
                self.close();
            } else {
                self.trip();
            }
            return;
        }

        let mut w = self.window.lock();
        if w.started.elapsed() > Duration::from_secs(self.cfg.window_secs) {
            *w = Window {
                errors: 0,
                total: 0,
                started: Instant::now(),
            };
        }
        w.total += 1;
        if !success {
            w.errors += 1;
        }
        if w.total >= self.cfg.min_requests {
            let rate = w.errors as f64 / w.total as f64;
            if rate > self.cfg.error_rate_threshold {
                drop(w);
                self.trip();
            }
        }
    }

    /// Non-mutating eligibility check: can this backend be considered as a
    /// routing candidate right now?
    ///
    /// - Closed → eligible.
    /// - Open → eligible only once the cool-off has elapsed AND no probe is
    ///   already in flight (so the next [`try_admit`] can claim it).
    /// - HalfOpen → not eligible (a probe is already being served).
    ///
    /// **This is side-effect free.** It never stores state, promotes the
    /// breaker, or claims a probe. Use it to enumerate routing candidates.
    /// Only the backend actually selected for routing/shadow mirroring may
    /// call [`try_admit`] to claim the single half-open probe.
    pub fn can_route(&self) -> bool {
        match self.state() {
            State::Closed => true,
            State::Open => {
                let cooled = match *self.opened_at.lock() {
                    Some(t) => t.elapsed() >= COOL_OFF,
                    None => false,
                };
                cooled && !self.probe_in_flight.load(Ordering::Acquire)
            }
            // The probe is already in flight; this backend is not selectable.
            State::HalfOpen => false,
        }
    }

    /// Side-effectful probe admission. Call **only** on the backend actually
    /// selected for routing or shadow mirroring — never during candidate
    /// enumeration (use [`can_route`] for that).
    ///
    /// - Closed → admit (no state change).
    /// - Open after cool-off → atomically claim the single probe slot
    ///   (`probe_in_flight` false→true), promote to HalfOpen, and return true
    ///   on success. Concurrent callers that lose the CAS return false.
    /// - Open before cool-off → reject.
    /// - HalfOpen → reject (a probe is already in flight).
    pub fn try_admit(&self) -> bool {
        match self.state() {
            State::Closed => true,
            State::Open => {
                // Promote to half-open once the cool-off elapses.
                let elapsed = match *self.opened_at.lock() {
                    Some(t) => t.elapsed() >= COOL_OFF,
                    None => false,
                };
                if elapsed {
                    // Atomically claim the single probe. Concurrent callers that
                    // lose the CAS stay blocked until `record` resolves it.
                    if self
                        .probe_in_flight
                        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
                        .is_ok()
                    {
                        self.state.store(State::HalfOpen as u8, Ordering::Relaxed);
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
            State::HalfOpen => {
                // The probe is already in flight; block everyone else.
                false
            }
        }
    }

    /// Legacy side-effectful admission: equivalent to [`try_admit`].
    ///
    /// **Deprecated for routing paths.** This mutates an open breaker into
    /// half-open and claims the single probe, so calling it while *filtering*
    /// candidates leaks probes onto backends that are never selected. Use
    /// [`can_route`] (non-mutating) for candidate filtering and [`try_admit`]
    /// on the single selected backend. Retained for tests and backward
    /// compatibility.
    pub fn allows(&self) -> bool {
        self.try_admit()
    }

    pub fn state(&self) -> State {
        State::from_u8(self.state.load(Ordering::Relaxed))
    }

    fn trip(&self) {
        self.state.store(State::Open as u8, Ordering::Relaxed);
        *self.opened_at.lock() = Some(Instant::now());
        // Reset the window so a fresh sample is collected after recovery.
        let mut w = self.window.lock();
        *w = Window {
            errors: 0,
            total: 0,
            started: Instant::now(),
        };
    }

    fn close(&self) {
        self.state.store(State::Closed as u8, Ordering::Relaxed);
        *self.opened_at.lock() = None;
        let mut w = self.window.lock();
        *w = Window {
            errors: 0,
            total: 0,
            started: Instant::now(),
        };
    }
}

#[cfg(test)]
impl Breaker {
    /// Test-only: rewind `opened_at` past the cool-off so the breaker is
    /// eligible for a half-open probe without sleeping. Lets router/regression
    /// tests drive the cooled-open state without depending on the 5s `COOL_OFF`.
    pub(crate) fn force_cooled(&self) {
        *self.opened_at.lock() = Some(Instant::now() - COOL_OFF - Duration::from_millis(10));
    }

    /// Test-only: whether the single half-open probe slot is currently claimed.
    pub(crate) fn probe_claimed(&self) -> bool {
        self.probe_in_flight.load(Ordering::SeqCst)
    }
}

/// Per-backend breaker registry.
pub struct Breakers {
    inner: std::collections::HashMap<String, Breaker>,
}

impl Breakers {
    pub fn from_config(
        backends: &std::collections::HashMap<String, crate::config::BackendConfig>,
        cfg: CircuitConfig,
    ) -> Self {
        let inner = backends
            .keys()
            .map(|name| (name.clone(), Breaker::new(cfg.clone())))
            .collect();
        Self { inner }
    }

    pub fn get(&self, name: &str) -> Option<&Breaker> {
        self.inner.get(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(min_requests: u32, threshold: f64) -> CircuitConfig {
        CircuitConfig {
            error_rate_threshold: threshold,
            window_secs: 30,
            min_requests,
        }
    }

    #[test]
    fn below_min_requests_does_not_trip() {
        let b = Breaker::new(cfg(20, 0.5));
        // 5 errors out of 5 (100% error rate) but below min_requests.
        for _ in 0..5 {
            b.record(false);
        }
        assert_eq!(b.state(), State::Closed);
        assert!(b.allows());
    }

    #[test]
    fn error_rate_above_threshold_trips() {
        let b = Breaker::new(cfg(10, 0.5));
        // 6 errors / 10 total = 60% > 50% threshold.
        for _ in 0..4 {
            b.record(true);
        }
        for _ in 0..6 {
            b.record(false);
        }
        assert_eq!(b.state(), State::Open);
        assert!(!b.allows());
    }

    #[test]
    fn low_error_rate_does_not_trip() {
        let b = Breaker::new(cfg(10, 0.5));
        for _ in 0..9 {
            b.record(true);
        }
        b.record(false); // 10% error rate
        assert_eq!(b.state(), State::Closed);
    }

    #[test]
    fn half_open_probe_recovers() {
        let b = Breaker::new(cfg(5, 0.5));
        // Trip it: 6 failures >= 5 min_requests, 100% error rate.
        for _ in 0..6 {
            b.record(false);
        }
        assert_eq!(b.state(), State::Open);
        // Manually force half-open by simulating cool-off expiry.
        *b.opened_at.lock() = Some(Instant::now() - COOL_OFF - Duration::from_millis(10));
        assert!(b.allows()); // promotes to half-open
        assert_eq!(b.state(), State::HalfOpen);
        // A successful probe closes the circuit.
        b.record(true);
        assert_eq!(b.state(), State::Closed);
    }

    #[test]
    fn half_open_probe_failure_reopens() {
        let b = Breaker::new(cfg(5, 0.5));
        for _ in 0..6 {
            b.record(false);
        }
        *b.opened_at.lock() = Some(Instant::now() - COOL_OFF - Duration::from_millis(10));
        let _ = b.allows();
        assert_eq!(b.state(), State::HalfOpen);
        b.record(false);
        assert_eq!(b.state(), State::Open);
    }

    #[test]
    fn half_open_admits_only_one_probe() {
        // Regression: once a caller promotes Open→HalfOpen and claims the probe,
        // every other concurrent caller must be blocked until the probe's
        // outcome is recorded. Without CAS-gating, all callers received `true`
        // and a burst hit the recovering backend.
        let b = Breaker::new(cfg(5, 0.5));
        for _ in 0..6 {
            b.record(false);
        }
        // Cool off so the next allows() promotes to half-open.
        *b.opened_at.lock() = Some(Instant::now() - COOL_OFF - Duration::from_millis(10));
        assert!(b.allows(), "first caller claims the single probe");
        assert_eq!(b.state(), State::HalfOpen);
        // Subsequent callers while the probe is in flight must be blocked.
        for _ in 0..50 {
            assert!(!b.allows(), "half-open must not admit a second probe");
        }
        // The probe resolves; the slot is released.
        b.record(true);
        assert_eq!(b.state(), State::Closed);
        assert!(b.allows(), "closed breaker admits traffic again");
    }

    #[test]
    fn can_route_is_non_mutating_on_cooled_open() {
        // Regression (P1, half-open probe consumed before selection):
        // `can_route` on a cooled-open breaker must report eligibility WITHOUT
        // promoting to HalfOpen or claiming the single probe. The old
        // side-effectful `allows`/`breaker_allows` would promote and claim on
        // every candidate during filtering, leaking probes onto backends that
        // were never selected.
        let b = Breaker::new(cfg(5, 0.5));
        for _ in 0..6 {
            b.record(false);
        }
        assert_eq!(b.state(), State::Open);
        b.force_cooled();
        assert!(!b.probe_claimed(), "no probe claimed yet");

        // Eligibility check must be side-effect free.
        assert!(b.can_route(), "cooled-open breaker is a routing candidate");
        assert_eq!(
            b.state(),
            State::Open,
            "can_route must not promote to HalfOpen"
        );
        assert!(!b.probe_claimed(), "can_route must not claim a probe");

        // Repeated checks must stay non-mutating.
        for _ in 0..10 {
            assert!(b.can_route());
        }
        assert_eq!(b.state(), State::Open);
        assert!(!b.probe_claimed());

        // By contrast, try_admit DOES claim the probe and promote.
        assert!(b.try_admit(), "admit claims the probe");
        assert_eq!(b.state(), State::HalfOpen);
        assert!(b.probe_claimed());
        // A half-open breaker is not selectable until the probe resolves.
        assert!(!b.can_route());
        b.record(true);
        assert_eq!(b.state(), State::Closed);
    }
}
