//! Timer system for thttpd using a BinaryHeap.
//! Replaces C's hash-of-sorted-lists with `BinaryHeap<Reverse<TimerEntry>>`.
//! Lazy cancellation via `cancelled` flag.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};
use std::time::{Duration, Instant};

/// Unique timer identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimerId(usize);

/// Context passed to timer callbacks.
pub struct TimerCtx;

/// A scheduled timer entry.
struct TimerEntry {
    id: TimerId,
    deadline: Instant,
    period: Option<Duration>,
    callback: Box<dyn FnMut(&TimerCtx)>,
}

impl PartialEq for TimerEntry {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for TimerEntry {}

impl PartialOrd for TimerEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TimerEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Composite key: deadline first, then id for consistency with PartialEq
        self.deadline
            .cmp(&other.deadline)
            .then_with(|| self.id.0.cmp(&other.id.0))
    }
}

/// BinaryHeap-based timer wheel.
pub struct TimerWheel {
    heap: BinaryHeap<Reverse<TimerEntry>>,
    next_id: usize,
    cancelled: HashSet<TimerId>,
}

impl TimerWheel {
    /// Create a new timer wheel.
    pub fn new() -> Self {
        Self {
            heap: BinaryHeap::new(),
            next_id: 0,
            cancelled: HashSet::new(),
        }
    }

    /// Create a one-shot timer.
    pub fn create(&mut self, delay: Duration, callback: Box<dyn FnMut(&TimerCtx)>) -> TimerId {
        let id = TimerId(self.next_id);
        self.next_id += 1;
        self.heap.push(Reverse(TimerEntry {
            id,
            deadline: Instant::now() + delay,
            period: None,
            callback,
        }));
        id
    }

    /// Create a periodic timer.
    pub fn create_periodic(
        &mut self,
        period: Duration,
        callback: Box<dyn FnMut(&TimerCtx)>,
    ) -> TimerId {
        let id = TimerId(self.next_id);
        self.next_id += 1;
        self.heap.push(Reverse(TimerEntry {
            id,
            deadline: Instant::now() + period,
            period: Some(period),
            callback,
        }));
        id
    }

    /// Cancel a timer (lazy — tracked in cancelled set, cleaned up on next run).
    pub fn cancel(&mut self, id: TimerId) {
        self.cancelled.insert(id);
    }

    /// Check if a timer is cancelled.
    fn is_cancelled(&self, id: TimerId) -> bool {
        self.cancelled.contains(&id)
    }

    /// Reset a timer to fire after `delay` from now.
    /// This cancels the old timer and creates a new one.
    pub fn reset(&mut self, id: TimerId, delay: Duration) -> Option<TimerId> {
        // Find the old timer to get its period, then cancel and re-create
        let period = self
            .heap
            .iter()
            .find(|e| e.0.id == id)
            .and_then(|e| e.0.period);
        self.cancel(id);
        let new_id = TimerId(self.next_id);
        self.next_id += 1;
        let entry = TimerEntry {
            id: new_id,
            deadline: Instant::now() + delay,
            period,
            callback: Box::new(|_| {}),
        };
        self.heap.push(Reverse(entry));
        Some(new_id)
    }

    /// Run all expired timers, returning the number fired.
    pub fn run(&mut self, ctx: &mut TimerCtx) -> usize {
        let now = Instant::now();
        let mut fired = 0;

        loop {
            let should_fire = match self.heap.peek() {
                Some(entry) if self.is_cancelled(entry.0.id) => {
                    let id = entry.0.id;
                    self.heap.pop();
                    self.cancelled.remove(&id);
                    continue;
                }
                Some(entry) if entry.0.deadline <= now => true,
                _ => false,
            };

            if !should_fire {
                break;
            }

            let mut entry = self.heap.pop().unwrap().0;
            if self.is_cancelled(entry.id) {
                self.cancelled.remove(&entry.id);
                continue;
            }

            // Fire the callback
            (entry.callback)(ctx);
            fired += 1;

            // Reschedule periodic timers relative to now (matching C's tmr_run)
            if let Some(period) = entry.period {
                entry.deadline = Instant::now() + period;
                if !self.is_cancelled(entry.id) {
                    self.heap.push(Reverse(entry));
                }
            }
        }

        // Clean up cancelled entries at the top
        loop {
            let is_cancelled = self.heap.peek().is_some_and(|e| self.is_cancelled(e.0.id));
            if !is_cancelled {
                break;
            }
            let entry = self.heap.pop().unwrap();
            self.cancelled.remove(&entry.0.id);
        }

        fired
    }

    /// Returns the duration until the next timer fires, or None if no timers.
    pub fn next_deadline(&self) -> Option<Duration> {
        let now = Instant::now();
        let mut earliest: Option<Instant> = None;
        for entry in &self.heap {
            if !self.is_cancelled(entry.0.id) {
                match earliest {
                    None => earliest = Some(entry.0.deadline),
                    Some(e) if entry.0.deadline < e => earliest = Some(entry.0.deadline),
                    _ => {}
                }
            }
        }
        earliest.map(|d| d.saturating_duration_since(now))
    }
}

impl Default for TimerWheel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn test_create_and_fire() {
        let mut wheel = TimerWheel::new();
        let fired = Arc::new(Mutex::new(false));
        let fired_clone = fired.clone();
        wheel.create(
            Duration::from_millis(1),
            Box::new(move |_| {
                *fired_clone.lock().unwrap() = true;
            }),
        );
        std::thread::sleep(Duration::from_millis(10));
        let mut ctx = TimerCtx;
        wheel.run(&mut ctx);
        assert!(*fired.lock().unwrap());
    }

    #[test]
    fn test_cancel_prevents_fire() {
        let mut wheel = TimerWheel::new();
        let fired = Arc::new(Mutex::new(false));
        let fired_clone = fired.clone();
        let id = wheel.create(
            Duration::from_millis(1),
            Box::new(move |_| {
                *fired_clone.lock().unwrap() = true;
            }),
        );
        wheel.cancel(id);
        std::thread::sleep(Duration::from_millis(10));
        let mut ctx = TimerCtx;
        wheel.run(&mut ctx);
        assert!(!*fired.lock().unwrap());
    }

    #[test]
    fn test_next_deadline() {
        let mut wheel = TimerWheel::new();
        assert!(wheel.next_deadline().is_none());
        wheel.create(Duration::from_secs(5), Box::new(|_| {}));
        assert!(wheel.next_deadline().unwrap() <= Duration::from_secs(5));
    }

    #[test]
    fn test_periodic_reschedule() {
        let mut wheel = TimerWheel::new();
        let count = Arc::new(Mutex::new(0));
        let count_clone = count.clone();
        wheel.create_periodic(
            Duration::from_millis(1),
            Box::new(move |_| {
                *count_clone.lock().unwrap() += 1;
            }),
        );
        std::thread::sleep(Duration::from_millis(10));
        let mut ctx = TimerCtx;
        wheel.run(&mut ctx);
        assert!(*count.lock().unwrap() >= 1);
    }
}
