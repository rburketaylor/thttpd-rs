# thttpd-rs — Rust Workspace

A behavior-preserving Rust port of sthttpd 2.27.0 plus the `thttpd-migrate`
strangler-fig proxy. The server is exercised by 105 differential scenarios
against the original C binary; the proxy has its own integration suite for
routing, shadowing, health, rollback, metrics, and drain behavior.
Deterministic values are compared exactly; documented nondeterministic values
are normalized explicitly.

## Crate Map

```
thttpd-core          main(), event loop, signals, throttling, config
├── thttpd-http      HTTP parsing, CGI, response building, directory listing
│   ├── thttpd-match     shell-style glob matching
│   ├── thttpd-mmc       memory-mapped file cache (Rc<Mmap>)
│   │   └── thttpd-mime      MIME type/encoding tables
│   └── thttpd-tdate     HTTP date parsing (RFC 1123/850/asctime)
├── thttpd-fdwatch    I/O multiplexing (thin mio wrapper)
└── thttpd-timers     BinaryHeap timer wheel

thttpd-migrate       async migration proxy: routing, forwarding, shadow diffing,
                     health, circuit breaker, metrics, state, control socket
```

## Crates

| Crate | C Source | What It Does |
|-------|----------|-------------|
| `thttpd-core` | `thttpd.c` | Event loop, startup, signal handling, configuration, throttle model |
| `thttpd-http` | `libhttpd.c` | HTTP parsing FSM, CGI dispatch, response builder, directory listing |
| `thttpd-fdwatch` | `fdwatch.c` | Thin mio wrapper with token-based dispatch |
| `thttpd-timers` | `timers.c` | BinaryHeap timer wheel with lazy cancellation |
| `thttpd-mmc` | `mmc.c` | Memory-mapped file cache with reference counting |
| `thttpd-match` | `match.c` | Shell-style glob pattern matching |
| `thttpd-tdate` | `tdate_parse.c` | HTTP date parsing (3 formats) |
| `thttpd-mime` | `mime_types.h` | MIME type and encoding lookup tables |
| `thttpd-migrate` | new migration tooling | Strangler-fig proxy for active-active/canary routing, shadow diffing, health, circuit breaker, metrics, rollback, and drain |

## Building

```bash
# Debug build
cargo build --workspace

# Release build (for testing against C binary)
cargo build --release

# Run all Rust unit tests
cargo test --workspace
```

## Server Architecture

The `thttpd-core` server is single-threaded, event-driven, and has no async
runtime. It uses `mio` directly with a manual poll loop:

```
poll() → accept connections
      → read request into buffer
      → run incremental FSM parser (handles byte-by-byte delivery)
      → dispatch: static file (mmap cache) or CGI (std::process::Command)
      → write response, linger-close
```

This deliberately matches thttpd's original architecture. Operational gaps are
tracked in `../docs/KNOWN_DEVIATIONS.md` rather than hidden behind a drop-in
replacement claim.

`thttpd-migrate` is intentionally separate migration tooling. It uses
`tokio`/`hyper` because proxying many client and backend connections is
naturally concurrent; see `../docs/ADR-0002-async-runtime-split.md`.

## Key Dependencies

| Crate | Purpose |
|-------|---------|
| `mio` | epoll/kqueue I/O multiplexing (replaces `select()`/`poll()`) |
| `memmap2` | Memory-mapped file I/O (replaces `mmap()`) |
| `nix` | Unix syscalls: `setuid`, `chroot`, `gethostname` |
| `clap` | CLI argument parsing |
| `signal-hook` | SIGTERM/SIGINT/SIGHUP handling |
| `slab` | O(1) connection table allocator |
| `thiserror` | Typed error enums |
| `tokio` / `hyper` | Async proxy runtime for `thttpd-migrate` |
