# thttpd — Server Core (Event Loop)

## Source
`legacy/src/thttpd.c` (2,189 lines) → `rust/crates/thttpd-core/src/` (2,715 lines across 9 modules)

## Status
Migrated. 105 differential scenarios pass.

## What It Does
The main event loop and server orchestration. Accepts connections, reads requests, dispatches to static file serving or CGI, writes responses, and manages connection lifecycle.

| Rust module | Responsibility |
|-------------|---------------|
| `lib.rs` | Library exports for the server crate |
| `main.rs` | CLI argument parsing, server initialization |
| `eventloop.rs` | mio poll loop, accept/read/send/linger dispatch |
| `startup.rs` | Listen socket binding, setuid/chroot |
| `server.rs` | Server struct, connection table, config |
| `connection.rs` | ConnSlot struct and state machine |
| `signal.rs` | SIGTERM/SIGINT/SIGHUP handling via signal-hook |
| `throttle.rs` | Bandwidth throttling and fair-share scheduling |
| `config.rs` | Server configuration struct |

## Key Decisions
- Single-threaded event loop using `mio` directly — no async runtime. Matches thttpd's architecture by design.
- Connection table uses `slab::Slab` for O(1) allocation/deallocation.
- Throttling parsing and calculations preserve C's byte-counting and rolling-average algorithm; runtime enforcement remains a known deviation.
- Signal handling uses `AtomicBool` flags checked at the top of the poll loop.
