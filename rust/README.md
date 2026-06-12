# thttpd-rs — Rust Workspace

A byte-exact Rust port of sthttpd 2.27.0, proven by 81 differential tests against the original C binary.

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
```

## Crates

| Crate | C Source | Lines | What It Does |
|-------|----------|-------|-------------|
| `thttpd-core` | `thttpd.c` | 454 | Event loop, startup, signal handling, bandwidth throttling |
| `thttpd-http` | `libhttpd.c` | 995 | HTTP parsing FSM, CGI dispatch, response builder, directory listing |
| `thttpd-fdwatch` | `fdwatch.c` | 72 | Thin mio wrapper with token-based dispatch |
| `thttpd-timers` | `timers.c` | 253 | BinaryHeap timer wheel with lazy cancellation |
| `thttpd-mmc` | `mmc.c` | 200 | Memory-mapped file cache with reference counting |
| `thttpd-match` | `match.c` | 132 | Shell-style glob pattern matching |
| `thttpd-tdate` | `tdate_parse.c` | 228 | HTTP date parsing (3 formats) |
| `thttpd-mime` | `mime_types.h` | 95 | MIME type and encoding lookup tables |

## Building

```bash
# Debug build
cargo build --workspace

# Release build (for testing against C binary)
cargo build --release

# Run all unit tests (91 tests)
cargo test --workspace
```

## Architecture

Single-threaded, event-driven, no async runtime. The server uses `mio` directly with a manual poll loop:

```
poll() → accept connections
      → read request into buffer
      → run incremental FSM parser (handles byte-by-byte delivery)
      → dispatch: static file (mmap cache) or CGI (std::process::Command)
      → write response, linger-close
```

This deliberately matches thttpd's original architecture — the port is a translation, not a redesign.

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
