# fdwatch — I/O Multiplexing

## Source
`legacy/src/fdwatch.c` (838 lines) → `rust/crates/thttpd-fdwatch/src/lib.rs` (72 lines)

## Status
Migrated. 80/80 differential tests pass.

## What It Does
Abstracts I/O multiplexing (poll/select/epoll). In C, this was a portable wrapper around multiple OS APIs. In Rust, `mio` provides the same capability natively, so this crate is a thin layer providing token constants and type aliases.

## Key Decisions
- 11.6× compression — `mio` replaces 838 lines of C portability shims with 72 lines.
- Token constants encode whether a poll event is for a listener or a connection, matching C's dispatch logic.
