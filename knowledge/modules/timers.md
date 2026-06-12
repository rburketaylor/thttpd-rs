# timers — Timer Wheel

## Source
`legacy/src/timers.c` (403 lines) → `rust/crates/thttpd-timers/src/lib.rs` (253 lines)

## Status
Migrated. 81/81 differential tests pass.

## What It Does
Provides one-shot and periodic timers for idle connection cleanup, bandwidth throttling, and other timed events. C used a hash table of sorted linked lists; Rust uses a `BinaryHeap<Reverse<TimerEntry>>`.

## Key Decisions
- Lazy cancellation via a `HashSet<TimerId>` instead of eagerly removing entries. Expired/cancelled entries are cleaned up on `run()`.
- `next_deadline()` scans the heap to calculate poll timeout — same purpose as C's `tmr_mstimeout()`.
- Callbacks are boxed closures (`Box<dyn FnMut>`).
