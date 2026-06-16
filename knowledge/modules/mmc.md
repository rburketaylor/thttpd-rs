# mmc — Memory-Mapped File Cache

## Source
`legacy/src/mmc.c` (529 lines) → `rust/crates/thttpd-mmc/src/lib.rs` (207 lines)

## Status
Migrated. 105 differential scenarios pass.

## What It Does
Caches file contents in memory using memory-mapped files. Provides reference-counted handles so files can be served concurrently without copying. C used manual `refcount` + `mmap()`; Rust uses `Rc<Mmap>` + `Rc::strong_count()`.

## Key Decisions
- `Rc<Mmap>` replaces manual reference counting — Rust's ownership system tracks when the last reference is dropped.
- HashMap keyed by canonical file path replaces C's linked-list cache.
- Periodic `cleanup()` evicts unreferenced entries, matching C's `mmc_cleanup()`.
