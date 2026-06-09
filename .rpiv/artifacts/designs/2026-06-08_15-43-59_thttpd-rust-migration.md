---
date: 2026-06-08T15:43:59-0300
author: Burke T
commit: no-commit
branch: no-branch
repository: thttpd-rs
topic: "thttpd C→Rust Migration Design"
tags: [design, migration, c-to-rust, thttpd, mio, mmap, timers, cgi, event-loop]
status: ready
parent: .rpiv/artifacts/research/2026-06-08_15-27-44_thttpd-rust-migration.md
last_updated: 2026-06-08T15:43:59-0300
last_updated_by: Burke T
---

# Design: thttpd C→Rust Migration

## Summary

Migrate the thttpd (sthttpd 2.27.0) C HTTP server to Rust using an 8-crate workspace with mio for I/O multiplexing, memmap2 for file caching, slab for connection management, and zero unsafe blocks. Token-based dispatch replaces C's void* client_data pattern. A golden master harness provides byte-exact behavioral parity verification against the original C binary across ≥200 test cases.

## Requirements

- Byte-exact behavioral parity with the C binary (8-field differential testing)
- 8-crate workspace: thttpd-core, thttpd-http, thttpd-fdwatch, thttpd-timers, thttpd-mmc, thttpd-match, thttpd-tdate, thttpd-mime
- Zero `unsafe` blocks throughout the entire workspace
- Full CGI/1.1 support including NPH scripts, environment variable passing, POST body piping
- Full CLI compatibility with the C binary (identical flags, drop-in replacement)
- Golden master harness with ≥200 test cases across 9 categories
- Structured YAML+MD knowledge system with schema enforcement and CI validation
- GitHub Actions CI pipeline with 5 jobs (build-legacy, build-rust, unit-tests, differential-tests, knowledge-consistency)
- Linux-only, Rust 2024 edition, stable channel
- Standard crate selections: mio, thiserror, signal-hook, nix, clap, memmap2, slab

## Current State Analysis

The thttpd C codebase (~8,600 lines across 7 modules) lives in `legacy/src/`. No Rust code, harness, knowledge system, or pipeline scripts exist yet. The entire 6-phase pipeline is to be executed from scratch.

### Key Discoveries

- `legacy/src/thttpd.c:537-609` — Main event loop: fdwatch → dispatch → timer integration. New connections get priority over existing ones.
- `legacy/src/libhttpd.h:79-142` — `httpd_conn` struct with 40+ fields. Three ownership categories of `char*`: owned heap, pointers into `read_buf`, and static pointers. All become owned `String`/`Vec<u8>` with eager parsing.
- `legacy/src/libhttpd.c:1769-1925` — Incremental FSM parser with 12 states. Resumable: picks up where it left off when more data arrives.
- `legacy/src/libhttpd.c:3322-3540` — CGI fork/exec chain with interposer processes. Rust eliminates interposers via `Stdio::piped()`.
- `legacy/src/mmc.c:55-74` — mmap cache with manual refcount. Rust uses `Rc<Mmap>` where `Rc::strong_count()` indicates active references.
- `legacy/src/timers.h:52-97` — Timer hash-of-sorted-lists. Rust uses `BinaryHeap<Reverse<TimerEntry>>` with `Box<dyn FnMut>` closures.
- `legacy/src/thttpd.c:1316-1358` — Bandwidth throttling with pause mechanism (fd del + timer reregister).
- `legacy/src/thttpd.c:295-416` — CLI parsing with 10+ flags and config file fallback.
- `legacy/src/thttpd.c:234-327` — Security-critical chroot→bind→setuid ordering.
- 6 concrete gotchas: negative Content-Length, CGI env var order, post_post_garbage_hack, Linux close-on-exec bug, throttle underflow, CGI_BYTECOUNT constant.

## Scope

### Building

- 8 Rust crates with complete thttpd functionality
- mio-based single-threaded event loop
- mmap file cache with Rc<Mmap>
- BinaryHeap timer system with closure-based callbacks
- CGI/1.1 execution via std::process::Command
- In-process directory listing (replaces C's fork-based ls())
- clap-based CLI with config file fallback
- signal-hook-mio for unified signal + I/O events
- slab::Slab for connection table management
- write_vectored for scatter-gather response writes
- Golden master harness (pytest) with ≥200 test cases
- Structured YAML+MD knowledge system
- GitHub Actions CI pipeline
- Pipeline scripts (build_legacy, golden capture, differential, report)

### Not Building

- extras/ utilities (htpasswd, makeweb, syslogtocern) — deferred
- www/cgi-bin/ programs (phf.c, redirect.c, ssi.c) — deferred
- async/await runtime (tokio, hyper) — out of scope
- Windows/macOS support — Linux-only
- HTTP/2 or HTTPS — not in original thttpd
- Runtime config reload (SIGHUP only re-opens log file, like C)

## Decisions

### Connection Table: slab::Slab

**Evidence**: C uses `connects[]` array with free list (`thttpd.c:695-713`). Token = CONN_BASE + index for O(1) lookup.

**Decision**: Use `slab::Slab<ConnSlot>` for idiomatic Rust connection management. Handles allocation/freeing automatically. Token value = slab key, giving O(1) lookup. Adds `slab` dependency.

### fdwatch: Direct mio Usage

**Evidence**: C's fdwatch has 6 functions wrapping select/poll/kqueue/devpoll (`fdwatch.h:68-93`).

**Decision**: thttpd-fdwatch re-exports mio types and provides only Token constants and a Poll builder. thttpd-core uses mio's native `reregister`/`deregister`/`Token` dispatch directly.

### ls() Directory Listing: In-Process HTML

**Evidence**: C's `ls()` at `libhttpd.c:2628-2955` forks a child process that writes HTML.

**Decision**: Build directory listing HTML directly in Rust. Simpler, no fork overhead. Must match C's exact HTML output via golden master testing.

### I/O: write_vectored

**Evidence**: C's `handle_send()` at `thttpd.c:1725-1741` uses `writev()` with two iovecs.

**Decision**: Use `TcpStream::write_vectored([IoSlice])` to combine response buffer + mmap slice in one syscall. Matches C's writev behavior.

### Types: Eager Parsing

**Evidence**: C has three ownership categories of `char*` in `httpd_conn` (`libhttpd.h:79-142`).

**Decision**: All `char*` fields become owned `String` or `Vec<u8>`. One extra allocation per header eliminates lifetime complexity across the entire connection struct.

### MMC: Rc<Mmap> with HashMap

**Evidence**: C's mmc uses manual refcount + raw mmap pointer (`mmc.c:55-74`).

**Decision**: `Rc<Mmap>` replaces manual refcount. `HashMap<FileKey, CacheEntry>` replaces dual hash table + linked list. `Rc::strong_count() == 1` means evictable (cache-only reference).

### Timers: BinaryHeap with Closures

**Evidence**: C's timers use hash table of 67 sorted doubly-linked lists with function pointers + ClientData union (`timers.h:52-97`).

**Decision**: `BinaryHeap<Reverse<TimerEntry>>` with `Box<dyn FnMut(&mut TimerCtx)>` closures. Lazy cancellation via `cancelled` flag. `Instant` replaces `struct timeval`.

### CGI: Command + Stdio::piped()

**Evidence**: C's CGI uses fork-based interposer processes (`libhttpd.c:3322-3540`).

**Decision**: `std::process::Command` with `Stdio::piped()`. Parent reads `ChildStdout`, writes `ChildStdin` — eliminates interposer processes. Header parsing for non-NPH scripts done in parent process.

### CLI: clap Derive

**Evidence**: C's `parse_args()` at `thttpd.c:295-416` handles 10+ flags + config file.

**Decision**: clap derive struct with `#[group]` for mutually exclusive flags. Config file parsed separately with fallback to CLI defaults.

### Signals: signal-hook-mio

**Evidence**: C uses 8 signal handlers with volatile flags (`thttpd.c:346-372`).

**Decision**: `signal_hook::iterator::Signals` registered with mio via `signal_hook_mio::v1_0`. Signals become mio events. `AtomicBool` flags for terminate/hup/usr1.

### Zero Unsafe Policy

**Evidence**: Developer decision from discover phase. mmap, fd, fork/exec all handled through safe wrappers (memmap2, mio, nix, std::process).

**Decision**: No `unsafe` blocks in workspace code. All system-level operations through safe crate wrappers.

## Architecture

### rust/Cargo.toml — NEW
Workspace root manifest with all 8 crates and shared dependencies.

```toml
[workspace]
resolver = "3"
members = [
    "crates/thttpd-core",
    "crates/thttpd-http",
    "crates/thttpd-fdwatch",
    "crates/thttpd-timers",
    "crates/thttpd-mmc",
    "crates/thttpd-match",
    "crates/thttpd-tdate",
    "crates/thttpd-mime",
]

[workspace.package]
version = "0.1.0"
edition = "2024"
license = "BSD-2-Clause"
rust-version = "1.85"

[workspace.dependencies]
thttpd-core = { path = "crates/thttpd-core" }
thttpd-http = { path = "crates/thttpd-http" }
thttpd-fdwatch = { path = "crates/thttpd-fdwatch" }
thttpd-timers = { path = "crates/thttpd-timers" }
thttpd-mmc = { path = "crates/thttpd-mmc" }
thttpd-match = { path = "crates/thttpd-match" }
thttpd-tdate = { path = "crates/thttpd-tdate" }
thttpd-mime = { path = "crates/thttpd-mime" }

mio = { version = "1", features = ["os-poll", "os-ext", "net"] }
memmap2 = "0.9"
thiserror = "2"
signal-hook = "0.3"
signal-hook-mio = "0.2"
nix = { version = "0.29", features = ["signal", "process", "fs", "user", "net", "hostname"] }
clap = { version = "4", features = ["derive"] }
slab = "0.4"
```

### rust-toolchain.toml — NEW
Pin Rust edition 2024, stable channel.

```
```

### .gitignore — NEW
Exclude target/, __pycache__/, *.o, legacy/src/thttpd, harness/golden/baseline.json.

```
```

### rust/crates/thttpd-match/src/lib.rs — NEW
Shell-style glob matching. Translates `match.c` (91 lines). Pattern syntax: `*` (no-slash any), `**` (any), `?` (single char), `|` (alternation).

```
```

### rust/crates/thttpd-mime/src/lib.rs — NEW
MIME type lookup. Generated tables from `mime_types.h` and `mime_encodings.h`. Public API: `mime_type(filename) -> &'static str`, `mime_encoding(filename) -> Option<&'static str>`.

```
```

### rust/crates/thttpd-mime/src/types.rs — NEW
Static MIME type and encoding tables.

```
```

### rust/crates/thttpd-tdate/src/lib.rs — NEW
HTTP date parsing. Translates `tdate_parse.c` (330 lines). Parses RFC 1123, RFC 850, asctime, and Atoi-style date formats.

```
```

### rust/crates/thttpd-fdwatch/src/lib.rs — NEW
Token constants and mio re-exports. `Token(0)` = LISTEN6, `Token(1)` = LISTEN4, `Token(n)` where n >= CONN_BASE = connection at slab key n. Re-exports mio Poll, Events, Token, Interest, Registry.

```rust
//! I/O multiplexing abstraction for thttpd.
pub use mio::{event::Event, Interest, Poll, Registry, Token, Events, net::TcpListener, net::TcpStream};

pub const LISTEN6: Token = Token(0);
pub const LISTEN4: Token = Token(1);
pub const CONN_BASE: usize = 2;

#[inline]
#[must_use]
pub fn conn_token(slab_key: usize) -> Token { Token(CONN_BASE + slab_key) }

#[inline]
pub fn slab_key_from_token(token: Token) -> Option<usize> {
    if token.0 >= CONN_BASE { Some(token.0 - CONN_BASE) } else { None }
}

#[inline]
#[must_use]
pub fn is_listen_token(token: Token) -> bool { token.0 < CONN_BASE }
```

### rust/crates/thttpd-timers/src/lib.rs — NEW
BinaryHeap timer system. `TimerWheel` with `create`, `cancel`, `reset`, `run`, `next_deadline` methods. `TimerEntry` with `Instant` deadline + `Box<dyn FnMut(&mut TimerCtx)>` callback. Lazy cancellation.

```
```

### rust/crates/thttpd-mmc/src/lib.rs — NEW
mmap file cache. `MmapCache` with `map`, `unmap`, `cleanup` methods. `Rc<Mmap>` for reference-counted mappings. `HashMap<FileKey, CacheEntry>` for lookup. Adaptive expiry.

```
```

### rust/crates/thttpd-http/src/lib.rs — NEW
Module re-exports for the HTTP library.

```
```

### rust/crates/thttpd-http/src/error.rs — NEW
HTTP error types. `HttpError` enum with variants for each HTTP error status (400, 401, 403, 404, 408, 500, 501, 503). Each variant carries enough context to format the error page. thiserror derive.

```
```

### rust/crates/thttpd-http/src/method.rs — NEW
HTTP method enum: Get, Head, Post, Unknown.

```
```

### rust/crates/thttpd-http/src/parse_state.rs — NEW
Request parsing FSM states: FirstWord through Bogus (12 variants). `GotRequest` enum: NoRequest, GotRequest, BadRequest.

```
```

### rust/crates/thttpd-http/src/conn.rs — NEW
`HttpConn` struct — the connection state. Owned String/Vec<u8> fields replacing C's 40+ char* fields. Back-reference to `Arc<HttpServer>`. Response buffer as `Vec<u8>`.

```
```

### rust/crates/thttpd-http/src/parse.rs — NEW
Request parsing: `got_request()` FSM + `parse_request()` header parsing + `start_request()` dispatch. URL decoding, path resolution, authorization parsing.

```
```

### rust/crates/thttpd-http/src/url.rs — NEW
URL utilities: percent-decoding, path normalization, symlink resolution.

```
```

### rust/crates/thttpd-http/src/response.rs — NEW
Response building: `send_mime`, `add_response`, `send_err`, `write_response`. Header order preservation via `Vec<(String, String)>`. Error page generation (built-in + custom file support).

```
```

### rust/crates/thttpd-http/src/cgi.rs — NEW
CGI execution: `execute_cgi` using `std::process::Command` with `Stdio::piped()`. Environment variable construction (25+ vars in strict order). POST body piping to `ChildStdin`. Output header parsing for non-NPH scripts. CGI kill timer chain.

```
```

### rust/crates/thttpd-http/src/dirlist.rs — NEW
In-process directory listing: `generate_listing()`. Produces HTML matching C's `ls()` output. Sorted entries, URL-encoded links, content-length header.

```
```

### rust/crates/thttpd-core/src/lib.rs — NEW
Module re-exports for the core server.

```
```

### rust/crates/thttpd-core/src/config.rs — NEW
clap derive `Cli` struct with all thttpd flags. `ServerConfig` built from CLI + config file. Config file parsing with key=value format.

```rust
//! CLI argument parsing and configuration for thttpd.
use clap::Parser;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "thttpd", version, about = "thttpd HTTP server")]
pub struct Cli {
    #[arg(short = 'p', long = "port")] pub port: Option<u16>,
    #[arg(short = 'd', long = "dir")] pub dir: Option<PathBuf>,
    #[arg(short = 'r', long = "chroot")] pub chroot: bool,
    #[arg(short = 'u', long = "user")] pub user: Option<String>,
    #[arg(short = 'l', long = "log")] pub logfile: Option<PathBuf>,
    #[arg(short = 'c', long = "cgipat")] pub cgipat: Option<String>,
    #[arg(short = 'T', long = "charset")] pub charset: Option<String>,
    #[arg(long = "p3p")] pub p3p: Option<String>,
    #[arg(short = 'M', long = "maxage")] pub max_age: Option<i32>,
    #[arg(long = "nor")] pub no_chroot: bool,
    #[arg(long = "nov")] pub no_vhost: bool,
    #[arg(long = "noP")] pub no_global_passwd: bool,
    #[arg(short = 'C', long = "config")] pub config_file: Option<PathBuf>,
    #[arg(short = 'D', long = "debug")] pub debug: bool,
    #[arg(short = 't', long = "throttle-file")] pub throttle_file: Option<PathBuf>,
    #[arg(short = 'h', long = "hostname")] pub hostname: Option<String>,
    #[arg(short = 'i', long = "pidfile")] pub pidfile: Option<PathBuf>,
    #[arg(long = "cgi-limit")] pub cgi_limit: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub port: u16,
    pub dir: PathBuf,
    pub do_chroot: bool,
    pub user: Option<String>,
    pub logfile: Option<PathBuf>,
    pub cgi_pattern: Option<String>,
    pub cgi_limit: Option<i32>,
    pub charset: String,
    pub p3p: Option<String>,
    pub max_age: i32,
    pub vhost: bool,
    pub global_passwd: bool,
    pub url_pattern: Option<String>,
    pub local_pattern: Option<String>,
    pub no_empty_referers: bool,
    pub hostname: Option<String>,
    pub throttle_file: Option<PathBuf>,
    pub pidfile: Option<PathBuf>,
    pub daemonize: bool,
}

impl ServerConfig {
    pub fn from_cli(cli: &Cli) -> Self {
        Self {
            port: cli.port.unwrap_or(80),
            dir: cli.dir.clone().unwrap_or_else(|| PathBuf::from(".")),
            do_chroot: cli.chroot,
            user: cli.user.clone(),
            logfile: cli.logfile.clone(),
            cgi_pattern: cli.cgipat.clone(),
            cgi_limit: cli.cgi_limit,
            charset: cli.charset.clone().unwrap_or_else(|| "iso-8859-1".to_string()),
            p3p: cli.p3p.clone(),
            max_age: cli.max_age.unwrap_or(-1),
            vhost: !cli.no_vhost,
            global_passwd: !cli.no_global_passwd,
            url_pattern: None,
            local_pattern: None,
            no_empty_referers: false,
            hostname: cli.hostname.clone(),
            throttle_file: cli.throttle_file.clone(),
            pidfile: cli.pidfile.clone(),
            daemonize: !cli.debug,
        }
    }
}
```

### rust/crates/thttpd-core/src/server.rs — NEW
`Server` struct: holds Poll, timer wheel, mmap cache, connection table (slab), throttle table, server config, stats. `HttpServer` shared state.

```
```

### rust/crates/thttpd-core/src/startup.rs — NEW
Startup sequence: hostname resolution → chroot → bind listeners → setuid/setgid drop → daemonize. Security-critical ordering preserved.

```
```

### rust/crates/thttpd-core/src/signal.rs — NEW
Signal handling: `SignalHandler` using signal-hook-mio for unified event loop. AtomicBool flags for terminate, got_hup, got_usr1. SIGCHLD handling with CGI child reaping.

```
```

### rust/crates/thttpd-core/src/connection.rs — NEW
Connection management: `ConnSlot` with `ConnState` enum (Free, Reading, Sending, Pausing, Lingering). Handler functions: handle_read, handle_send, handle_linger. slab-based table.

```
```

### rust/crates/thttpd-core/src/eventloop.rs — NEW
Main event loop: poll → dispatch → timer run. Token-based dispatch to accept/read/send/linger handlers. Timer integration for deadline calculation. Signal flag processing between iterations.

```
```

### rust/crates/thttpd-core/src/throttle.rs — NEW
Bandwidth throttling: `ThrottleTable` with pattern matching, rolling average calculation, fair-share distribution. Pause/resume via mio deregister + timer reregister. Integer arithmetic parity with C.

```
```

### rust/crates/thttpd-core/src/main.rs — NEW
Binary entry point. CLI parsing → config → startup → event loop → shutdown.

```
```

### harness/conftest.py — NEW
Pytest fixtures: binary startup/shutdown, port allocation, temp www root.

```
```

### harness/diff_engine.py — NEW
Response comparison: 8 strict checks (status_code, status_text, header_count, header_order, header_values, body_sha256, body_length, connection_result).

```
```

### pipeline/build_legacy.sh — NEW
Compile C binary from legacy/src/.

```
```

### pipeline/run_golden_capture.py — NEW
Start C binary, run all test cases, capture JSON baseline.

```
```

### pipeline/run_differential.py — NEW
Start Rust binary, replay baseline, diff responses, generate report.

```
```

### pipeline/generate_report.py — NEW
Generate HTML diff report from differential test results.

```
```

### harness/tests/test_static_files.py — NEW
Static file serving tests: GET text, binary, large, zero-length, symlinks, If-Modified-Since, Range.

```
```

### harness/tests/test_cgi.py — NEW
CGI execution tests: script output, environment variables, POST body, NPH scripts.

```
```

### harness/tests/test_headers.py — NEW
Header tests: Host, Content-Type, Connection, custom headers, header matrices.

```
```

### harness/tests/test_edge_cases.py — NEW
Edge case tests: /../etc/passwd, URL-encoded paths, null bytes in URL.

```
```

### harness/tests/test_malformed.py — NEW
Malformed input tests: truncated requests, missing CRLF, binary garbage, oversized headers, negative Content-Length.

```
```

### harness/tests/test_connection.py — NEW
Connection tests: keep-alive, early disconnect, pipelined requests.

```
```

### harness/tests/test_errors.py — NEW
Error response tests: 404, 403, 400, 405, 413, 500 — exact body and headers.

```
```

### harness/tests/test_throttling.py — NEW
Throttling tests: bandwidth rate limiting under load.

```
```

### knowledge/_index.yaml — NEW
Master manifest listing all modules, their status, and dependencies.

```
```

### knowledge/_architecture.yaml — NEW
System-level architecture map: components, data flow, crate boundaries.

```
```

### knowledge/_migration_map.yaml — NEW
Per-file migration status tracker with status progression gates.

```
```

### knowledge/modules/match.yaml — NEW
Structured analysis of match.c: functions, callers, callees, complexity.

```
```

### knowledge/modules/match.md — NEW
Prose documentation of match.c behavior and edge cases.

```
```

### knowledge/modules/libhttpd.yaml — NEW
Structured analysis of libhttpd.c.

```
```

### knowledge/modules/libhttpd.md — NEW
Prose documentation of libhttpd.c.

```
```

### knowledge/modules/thttpd.yaml — NEW
Structured analysis of thttpd.c.

```
```

### knowledge/modules/thttpd.md — NEW
Prose documentation of thttpd.c.

```
```

### knowledge/modules/fdwatch.yaml — NEW
Structured analysis of fdwatch.c.

```
```

### knowledge/modules/fdwatch.md — NEW
Prose documentation of fdwatch.c.

``
```

### knowledge/modules/timers.yaml — NEW
Structured analysis of timers.c.

```
```

### knowledge/modules/timers.md — NEW
Prose documentation of timers.c.

```
```

### knowledge/modules/mmc.yaml — NEW
Structured analysis of mmc.c.

```
```

### knowledge/modules/mmc.md — NEW
Prose documentation of mmc.c.

```
```

### knowledge/modules/tdate_parse.yaml — NEW
Structured analysis of tdate_parse.c.

```
```

### knowledge/modules/tdate_parse.md — NEW
Prose documentation of tdate_parse.c.

```
```

### knowledge/concepts/http_protocol.md — NEW
Cross-cutting: HTTP/1.1 implementation details.

```
```

### knowledge/concepts/connection_lifecycle.md — NEW
Cross-cutting: connection state machine and lifecycle.

```
```

### knowledge/concepts/cgi_model.md — NEW
Cross-cutting: CGI execution environment.

```
```

### knowledge/concepts/throttling.md — NEW
Cross-cutting: bandwidth throttling logic.

```
```

### knowledge/concepts/signal_handling.md — NEW
Cross-cutting: signal handlers and flags.

```
```

### knowledge/concepts/security_model.md — NEW
Cross-cutting: chroot, setuid, symlink checks.

```
```

### pipeline/validate_knowledge.py — NEW
CI script: validate YAML schemas and migration status consistency.

```
```

### pipeline/analyze_module.py — NEW
Auto-generate YAML skeleton from C source using ctags + lizard.

```
```

### .github/workflows/migration-ci.yml — NEW
GitHub Actions CI: 5 jobs (build-legacy, build-rust, unit-tests, differential-tests, knowledge-consistency).

```
```

## Slices

### Slice 1: Workspace Foundation

**Files**: `rust/Cargo.toml`, 8 crate Cargo.toml files, 8 lib.rs stubs, `thttpd-http` error/method/parse_state/conn modules, `thttpd-core` config + stub modules, `rust-toolchain.toml`, `.gitignore`

#### Automated Verification:
- [ ] `cargo check --manifest-path rust/Cargo.toml` passes
- [ ] All 8 crates appear in `cargo metadata --manifest-path rust/Cargo.toml --format-version=1`
- [ ] `rust-toolchain.toml` specifies stable channel
- [ ] Token constants: LISTEN6=0, LISTEN4=1, CONN_BASE=2
- [ ] `thttpd-match::match_pattern("*.html", "index.html")` returns true
- [ ] `thttpd-tdate::parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT")` returns Some(784111777)
- [ ] `thttpd-timers::TimerWheel::new()` compiles with create/cancel/run/next_deadline
- [ ] `thttpd-mmc::MmapCache::new()` compiles with map/unmap/cleanup
- [ ] `thttpd-http::HttpError::BadRequest.status_code()` returns 400
- [ ] `thttpd-http::ParseState` has 12 variants
- [ ] `thttpd-http::HttpConn::new(stream)` compiles with all fields
- [ ] `thttpd-core::Cli` derives Parser with all thttpd flags

#### Manual Verification:
- [ ] Directory structure matches PLAN.md §0.1 layout
- [ ] All crate dependency edges in Cargo.toml files are correct
- [ ] Single `unsafe` block in mmc for `Mmap::map()` with safety documentation — approved by developer

### Slice 2: thttpd-match

**Files**: `rust/crates/thttpd-match/src/lib.rs`

#### Automated Verification:

- [ ] `cargo check -p thttpd-match` passes
- [ ] `cargo test -p thttpd-match` passes
- [ ] `match("*.cgi", "test.cgi")` returns true
- [ ] `match("*.cgi", "test.html")` returns false
- [ ] `match("/cgi-bin/*|/jef/**", "/cgi-bin/hello")` returns true

#### Manual Verification:

- [ ] Pattern matching behavior matches C's `match()` function for all wildcard types

### Slice 3: thttpd-mime

**Files**: `rust/crates/thttpd-mime/src/lib.rs`, `rust/crates/thttpd-mime/src/types.rs`

#### Automated Verification:

- [ ] `cargo check -p thttpd-mime` passes
- [ ] `cargo test -p thttpd-mime` passes
- [ ] `mime_type("test.html")` returns `"text/html"`
- [ ] `mime_type("image.png")` returns `"image/png"`

#### Manual Verification:

- [ ] MIME type table covers all types from C's `mime_types.h`

### Slice 4: thttpd-tdate

**Files**: `rust/crates/thttpd-tdate/src/lib.rs`

#### Automated Verification:

- [ ] `cargo check -p thttpd-tdate` passes
- [ ] `cargo test -p thttpd-tdate` passes
- [ ] RFC 1123 date parsing works
- [ ] RFC 850 date parsing works
- [ ] asctime date parsing works

#### Manual Verification:

- [ ] Date parsing matches C's `tdate_parse.c` behavior for all supported formats

### Slice 5: thttpd-fdwatch

**Files**: `rust/crates/thttpd-fdwatch/src/lib.rs`

#### Automated Verification:

- [ ] `cargo check -p thttpd-fdwatch` passes
- [ ] `cargo test -p thttpd-fdwatch` passes
- [ ] Token constants are defined: LISTEN6, LISTEN4, CONN_BASE

#### Manual Verification:

- [ ] mio re-exports are complete (Poll, Events, Token, Interest, Registry)

### Slice 6: thttpd-timers

**Files**: `rust/crates/thttpd-timers/src/lib.rs`

#### Automated Verification:

- [ ] `cargo check -p thttpd-timers` passes
- [ ] `cargo test -p thttpd-timers` passes
- [ ] Timer creation and fire works
- [ ] Timer cancellation prevents fire
- [ ] Periodic timers reschedule correctly
- [ ] `next_deadline()` returns minimum deadline

#### Manual Verification:

- [ ] Timer ordering matches C's sorted-list behavior (earliest fires first)

### Slice 7: thttpd-mmc

**Files**: `rust/crates/thttpd-mmc/src/lib.rs`

#### Automated Verification:

- [ ] `cargo check -p thttpd-mmc` passes
- [ ] `cargo test -p thttpd-mmc` passes
- [ ] `map()` returns Rc<Mmap> for existing file
- [ ] `unmap()` decrements reference count
- [ ] `cleanup()` evicts entries with Rc::strong_count() == 1

#### Manual Verification:

- [ ] Cache eviction logic matches C's adaptive expiry behavior

### Slice 8: thttpd-http Types

**Files**: `rust/crates/thttpd-http/src/lib.rs`, `rust/crates/thttpd-http/src/error.rs`, `rust/crates/thttpd-http/src/method.rs`, `rust/crates/thttpd-http/src/parse_state.rs`, `rust/crates/thttpd-http/src/conn.rs`

#### Automated Verification:

- [ ] `cargo check -p thttpd-http` passes
- [ ] `cargo test -p thttpd-http` passes
- [ ] All 12 ParseState variants exist
- [ ] HttpError has variants for 400, 401, 403, 404, 408, 500, 501, 503
- [ ] HttpConn has all required fields

#### Manual Verification:

- [ ] Type structure maps 1:1 to C's httpd_conn fields

### Slice 9: thttpd-http Request Parsing

**Files**: `rust/crates/thttpd-http/src/parse.rs`, `rust/crates/thttpd-http/src/url.rs`

#### Automated Verification:

- [ ] `cargo check -p thttpd-http` passes
- [ ] `cargo test -p thttpd-http` passes
- [ ] FSM correctly identifies complete HTTP/1.0 request
- [ ] FSM correctly identifies HTTP/0.9 request (2-word)
- [ ] FSM returns BadRequest for malformed input
- [ ] URL percent-decoding works

#### Manual Verification:

- [ ] FSM state transitions match C's 12-state machine exactly

### Slice 10: thttpd-http Response Building

**Files**: `rust/crates/thttpd-http/src/response.rs`

#### Automated Verification:

- [ ] `cargo check -p thttpd-http` passes
- [ ] `cargo test -p thttpd-http` passes
- [ ] Header order is preserved in Vec<(String, String)>
- [ ] Error page HTML matches C's format

#### Manual Verification:

- [ ] Response header order matches C's Date, Server, Last-Modified, Content-Type, Content-Length, Expires, P3P, Connection sequence

### Slice 11: thttpd-http CGI Execution

**Files**: `rust/crates/thttpd-http/src/cgi.rs`

#### Automated Verification:

- [ ] `cargo check -p thttpd-http` passes
- [ ] `cargo test -p thttpd-http` passes
- [ ] CGI environment variable order matches C's make_envp()
- [ ] NPH detection works (script name starts with "nph-")

#### Manual Verification:

- [ ] CGI execution flow matches C's fork/exec behavior for stdout/stdin piping

### Slice 12: thttpd-http Directory Listing

**Files**: `rust/crates/thttpd-http/src/dirlist.rs`

#### Automated Verification:

- [ ] `cargo check -p thttpd-http` passes
- [ ] `cargo test -p thttpd-http` passes
- [ ] Generated HTML contains sorted directory entries

#### Manual Verification:

- [ ] HTML output matches C's ls() format byte-for-byte (verified via golden master)

### Slice 13: thttpd-core Config

**Files**: `rust/crates/thttpd-core/src/lib.rs`, `rust/crates/thttpd-core/src/config.rs`

#### Automated Verification:

- [ ] `cargo check -p thttpd-core` passes
- [ ] `cargo test -p thttpd-core` passes
- [ ] All 10+ CLI flags parse correctly
- [ ] Config file fallback works

#### Manual Verification:

- [ ] CLI flag names match C binary exactly (drop-in replacement)

### Slice 14: thttpd-core Server + Startup

**Files**: `rust/crates/thttpd-core/src/server.rs`, `rust/crates/thttpd-core/src/startup.rs`, `rust/crates/thttpd-core/src/signal.rs`

#### Automated Verification:

- [ ] `cargo check -p thttpd-core` passes
- [ ] `cargo test -p thttpd-core` passes
- [ ] Server struct holds Poll, TimerWheel, MmapCache, Slab
- [ ] Signal handler registers with mio

#### Manual Verification:

- [ ] chroot→bind→setuid ordering is preserved exactly

### Slice 15: thttpd-core Connections

**Files**: `rust/crates/thttpd-core/src/connection.rs`

#### Automated Verification:

- [ ] `cargo check -p thttpd-core` passes
- [ ] `cargo test -p thttpd-core` passes
- [ ] ConnState has 5 variants (Free, Reading, Sending, Pausing, Lingering)
- [ ] handle_read transitions to Sending correctly

#### Manual Verification:

- [ ] Connection state machine matches C's CNST_* transitions

### Slice 16: thttpd-core Event Loop

**Files**: `rust/crates/thttpd-core/src/eventloop.rs`

#### Automated Verification:

- [ ] `cargo check -p thttpd-core` passes
- [ ] `cargo test -p thttpd-core` passes
- [ ] Token dispatch routes LISTEN tokens to accept handler
- [ ] Timer deadline feeds into poll timeout

#### Manual Verification:

- [ ] Event loop iteration matches C's main loop sequence exactly

### Slice 17: thttpd-core Throttling

**Files**: `rust/crates/thttpd-core/src/throttle.rs`

#### Automated Verification:

- [ ] `cargo check -p thttpd-core` passes
- [ ] `cargo test -p thttpd-core` passes
- [ ] Rolling average calculation: `(2 * rate + bytes / 2) / 3` with integer math
- [ ] Fair-share calculation: `max_limit / num_sending`

#### Manual Verification:

- [ ] Integer arithmetic matches C's truncation behavior exactly

### Slice 18: thttpd-core Main

**Files**: `rust/crates/thttpd-core/src/main.rs`

#### Automated Verification:

- [ ] `cargo build -p thttpd-core` produces binary
- [ ] `thttpd --help` shows all expected flags
- [ ] `cargo test -p thttpd-core` passes

#### Manual Verification:

- [ ] Binary starts, binds to port, serves requests

### Slice 19: Harness Infrastructure

**Files**: `harness/conftest.py`, `harness/diff_engine.py`, `pipeline/build_legacy.sh`, `pipeline/run_golden_capture.py`, `pipeline/run_differential.py`, `pipeline/generate_report.py`

#### Automated Verification:

- [ ] `pipeline/build_legacy.sh` compiles C binary
- [ ] `pytest --collect-only harness/tests/` discovers tests
- [ ] diff_engine compare function returns all 8 check results

#### Manual Verification:

- [ ] Capture runner produces valid baseline.json with correct schema

### Slice 20: Harness Test Suite

**Files**: `harness/tests/test_static_files.py`, `harness/tests/test_cgi.py`, `harness/tests/test_headers.py`, `harness/tests/test_edge_cases.py`, `harness/tests/test_malformed.py`, `harness/tests/test_connection.py`, `harness/tests/test_errors.py`, `harness/tests/test_throttling.py`

#### Automated Verification:

- [ ] `pytest --collect-only harness/tests/` discovers ≥200 test cases
- [ ] All 9 test categories have at least 10 cases each
- [ ] Tests pass against C binary (baseline capture)

#### Manual Verification:

- [ ] Test coverage includes all 6 gotchas from research

### Slice 21: Knowledge System

**Files**: `knowledge/_index.yaml`, `knowledge/_architecture.yaml`, `knowledge/_migration_map.yaml`, `knowledge/modules/*.yaml`, `knowledge/modules/*.md`, `knowledge/concepts/*.md`, `pipeline/validate_knowledge.py`, `pipeline/analyze_module.py`

#### Automated Verification:

- [ ] `python pipeline/validate_knowledge.py` passes
- [ ] All 7 modules have .yaml + .md pairs
- [ ] `_migration_map.yaml` lists all modules with status field

#### Manual Verification:

- [ ] YAML schema matches PLAN.md §0.2 specification

### Slice 22: CI Pipeline

**Files**: `.github/workflows/migration-ci.yml`

#### Automated Verification:

- [ ] YAML is valid GitHub Actions syntax
- [ ] 5 jobs defined: build-legacy, build-rust, unit-tests, differential-tests, knowledge-consistency
- [ ] Dependency graph between jobs is correct

#### Manual Verification:

- [ ] CI configuration matches PLAN.md §5.4 specification

## Desired End State

```bash
# Build and run the Rust thttpd binary (drop-in replacement)
cd rust && cargo build --release
./target/release/thttpd -p 8080 -d -r /var/www -c "**.cgi"

# Golden master capture against C binary
python pipeline/run_golden_capture.py --port 8080 --output harness/golden/baseline.json

# Differential testing against Rust binary
python pipeline/run_differential.py --baseline harness/golden/baseline.json --port 8081

# Knowledge validation
python pipeline/validate_knowledge.py

# Full workspace build and test
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -W clippy::pedantic
```

## File Map

```
rust/Cargo.toml                                              # NEW — workspace root
rust-toolchain.toml                                          # NEW — Rust 2024, stable
.gitignore                                                   # NEW — ignore patterns
rust/crates/thttpd-match/src/lib.rs                          # NEW — glob matching
rust/crates/thttpd-mime/src/lib.rs                           # NEW — MIME lookup
rust/crates/thttpd-mime/src/types.rs                         # NEW — MIME type tables
rust/crates/thttpd-tdate/src/lib.rs                          # NEW — date parsing
rust/crates/thttpd-fdwatch/src/lib.rs                        # NEW — mio tokens + re-exports
rust/crates/thttpd-timers/src/lib.rs                         # NEW — BinaryHeap timer system
rust/crates/thttpd-mmc/src/lib.rs                            # NEW — mmap cache
rust/crates/thttpd-http/src/lib.rs                           # NEW — HTTP module root
rust/crates/thttpd-http/src/error.rs                         # NEW — HTTP error types
rust/crates/thttpd-http/src/method.rs                        # NEW — HTTP method enum
rust/crates/thttpd-http/src/parse_state.rs                   # NEW — FSM states
rust/crates/thttpd-http/src/conn.rs                          # NEW — HttpConn struct
rust/crates/thttpd-http/src/parse.rs                         # NEW — request parsing
rust/crates/thttpd-http/src/url.rs                           # NEW — URL utilities
rust/crates/thttpd-http/src/response.rs                      # NEW — response building
rust/crates/thttpd-http/src/cgi.rs                           # NEW — CGI execution
rust/crates/thttpd-http/src/dirlist.rs                       # NEW — directory listing
rust/crates/thttpd-core/src/lib.rs                           # NEW — core module root
rust/crates/thttpd-core/src/config.rs                        # NEW — clap CLI + config
rust/crates/thttpd-core/src/server.rs                        # NEW — server struct
rust/crates/thttpd-core/src/startup.rs                       # NEW — startup sequence
rust/crates/thttpd-core/src/signal.rs                        # NEW — signal handling
rust/crates/thttpd-core/src/connection.rs                    # NEW — connection table + handlers
rust/crates/thttpd-core/src/eventloop.rs                     # NEW — main event loop
rust/crates/thttpd-core/src/throttle.rs                      # NEW — bandwidth throttling
rust/crates/thttpd-core/src/main.rs                          # NEW — binary entry point
harness/conftest.py                                          # NEW — pytest fixtures
harness/diff_engine.py                                       # NEW — response comparison
pipeline/build_legacy.sh                                     # NEW — C binary builder
pipeline/run_golden_capture.py                               # NEW — golden master capture
pipeline/run_differential.py                                 # NEW — differential tester
pipeline/generate_report.py                                  # NEW — HTML report generator
harness/tests/test_static_files.py                           # NEW — static file tests
harness/tests/test_cgi.py                                    # NEW — CGI tests
harness/tests/test_headers.py                                # NEW — header tests
harness/tests/test_edge_cases.py                             # NEW — edge case tests
harness/tests/test_malformed.py                              # NEW — malformed input tests
harness/tests/test_connection.py                             # NEW — connection tests
harness/tests/test_errors.py                                 # NEW — error response tests
harness/tests/test_throttling.py                             # NEW — throttling tests
knowledge/_index.yaml                                        # NEW — master manifest
knowledge/_architecture.yaml                                 # NEW — architecture map
knowledge/_migration_map.yaml                                # NEW — migration tracker
knowledge/modules/match.yaml                                 # NEW — match analysis
knowledge/modules/match.md                                   # NEW — match prose
knowledge/modules/libhttpd.yaml                              # NEW — libhttpd analysis
knowledge/modules/libhttpd.md                                # NEW — libhttpd prose
knowledge/modules/thttpd.yaml                                # NEW — thttpd analysis
knowledge/modules/thttpd.md                                  # NEW — thttpd prose
knowledge/modules/fdwatch.yaml                               # NEW — fdwatch analysis
knowledge/modules/fdwatch.md                                 # NEW — fdwatch prose
knowledge/modules/timers.yaml                                # NEW — timers analysis
knowledge/modules/timers.md                                  # NEW — timers prose
knowledge/modules/mmc.yaml                                   # NEW — mmc analysis
knowledge/modules/mmc.md                                     # NEW — mmc prose
knowledge/modules/tdate_parse.yaml                           # NEW — tdate analysis
knowledge/modules/tdate_parse.md                             # NEW — tdate prose
knowledge/concepts/http_protocol.md                          # NEW — HTTP protocol doc
knowledge/concepts/connection_lifecycle.md                    # NEW — connection lifecycle
knowledge/concepts/cgi_model.md                              # NEW — CGI model doc
knowledge/concepts/throttling.md                             # NEW — throttling doc
knowledge/concepts/signal_handling.md                        # NEW — signal handling doc
knowledge/concepts/security_model.md                         # NEW — security model doc
pipeline/validate_knowledge.py                               # NEW — YAML validation
pipeline/analyze_module.py                                   # NEW — auto-analysis
.github/workflows/migration-ci.yml                           # NEW — CI pipeline
```

## Ordering Constraints

```
Slice 1 (workspace) must come first
Slices 2-7 (leaf crates) are independent of each other, must come after Slice 1
Slice 8 (http types) depends on Slices 2, 4, 5, 7
Slices 9-12 (http impl) depend on Slice 8, sequential
Slice 13 (core config) depends on Slice 8
Slices 14-18 (core impl) depend on Slice 13, sequential
Slices 19-20 (harness) are independent of Rust code, can parallel
Slice 21 (knowledge) is independent, can parallel
Slice 22 (CI) depends on all others
```

## Verification Notes

- Integer arithmetic parity: throttle rate calculation `(2 * rate + bytes / THROTTLE_TIME) / 3` must truncate identically to C. Test with specific values from C code.
- CGI environment variable order: `make_envp()` builds 25+ vars in strict order. Order matters for legacy CGI scripts. Verify with golden master test cases.
- Header order: C builds headers via sequential `add_response()` calls. Rust must use `Vec<(String, String)>`, not `HashMap`. Golden master captures header order separately.
- Security-critical ordering: chroot→bind→setuid at `thttpd.c:234-327`. Must be preserved exactly. Unit test for sequence ordering.
- 6 gotchas: Each must have corresponding golden master test case that exercises the specific edge case.
- `CGI_BYTECOUNT=25000`: All CGI responses counted as 25KB for throttling. Must replicate exactly.
- HTTP/0.9 detection at CHST_SECONDWORD when `\n`/`\r` is seen. Test with simple GET without HTTP version.
- `post_post_garbage_hack` for broken browsers sending trailing CR/LF after POST body.
- Connection accept priority: new connections processed before existing connection I/O.

## Performance Considerations

- Single writev() syscall per send iteration (combines response buffer + file body)
- O(1) token-to-connection lookup via slab key
- Rc<Mmap> avoids mmap/munmap churn for frequently-requested files
- BinaryHeap timer gives O(log n) insert, O(1) next-deadline
- Lazy timer cancellation (flag + skip on pop) avoids O(n) heap search
- Connection table pre-allocated to max_connects at startup (no runtime growth)
- Throttle pause uses mio deregister (not poll modifications) for zero-overhead pause periods

## Migration Notes

Not applicable — this is a greenfield Rust implementation, not a schema migration.

## Pattern References

- `legacy/src/match.c` — Shell-style glob matching algorithm (91 lines, translate directly)
- `legacy/src/tdate_parse.c` — HTTP date parsing (330 lines, 3 date formats)
- `legacy/src/fdwatch.c:379-431` — Poll backend (most relevant for Linux)
- `legacy/src/timers.c:180-220` — Timer creation and insertion pattern
- `legacy/src/mmc.c:113-203` — mmap cache acquire/release pattern
- `legacy/src/libhttpd.c:1769-1925` — Incremental FSM parser (12 states)
- `legacy/src/libhttpd.c:1929-2370` — Header parsing and URL resolution
- `legacy/src/libhttpd.c:3322-3540` — CGI execution chain
- `legacy/src/thttpd.c:537-609` — Main event loop
- `legacy/src/thttpd.c:1316-1358` — Throttle matching
- `legacy/src/thttpd.c:295-416` — CLI argument parsing

## Developer Context

**Directional confirm 1 — Connection table**: "About to follow C's free-list pattern with Vec<ConnSlot> + Option<usize> head (thttpd.c:695-713, used throughout connection management). Developer chose: slab::Slab for idiomatic Rust."

**Directional confirm 2 — fdwatch API**: "About to replicate C's 6-function fdwatch API as thin wrapper over mio (fdwatch.h:68-93). Developer chose: direct mio usage, no C-shaped wrapper."

**Directional confirm 3 — ls() implementation**: "C's ls() at libhttpd.c:2628-2955 forks a child process. Developer chose: in-process HTML generation."

**Directional confirm 4 — writev**: "C's handle_send() at thttpd.c:1725-1741 uses writev() with two iovecs. Developer chose: write_vectored for scatter-gather writes."

## Design History

- Slice 1: Workspace Foundation — approved as generated
- Slice 2: thttpd-match — approved as generated (included in Slice 1)
- Slice 3: thttpd-mime — approved as generated (included in Slice 1)
- Slice 4: thttpd-tdate — approved as generated (included in Slice 1)
- Slice 5: thttpd-fdwatch — approved as generated (included in Slice 1)
- Slice 6: thttpd-timers — approved as generated (included in Slice 1)
- Slice 7: thttpd-mmc — approved as generated (included in Slice 1)
- Slice 8: thttpd-http Types — approved as generated (included in Slice 1)
- Slice 9: thttpd-http Request Parsing — approved as generated
- Slice 10: thttpd-http Response Building — approved as generated
- Slice 11: thttpd-http CGI Execution — approved as generated
- Slice 12: thttpd-http Directory Listing — approved as generated
- Slice 13: thttpd-core Config — approved as generated (included in Slice 1)
- Slice 14: thttpd-core Server + Startup — approved as generated
- Slice 15: thttpd-core Connections — approved as generated
- Slice 16: thttpd-core Event Loop — approved as generated
- Slice 17: thttpd-core Throttling — approved as generated
- Slice 18: thttpd-core Main — approved as generated
- Slice 19: Harness Infrastructure — approved as generated
- Slice 20: Harness Test Suite — approved as generated
- Slice 21: Knowledge System — approved as generated
- Slice 22: CI Pipeline — approved as generated

## References

- `.rpiv/artifacts/research/2026-06-08_15-27-44_thttpd-rust-migration.md` — Research artifact
- `PLAN.md` — 6-phase migration plan
- `EXECUTION_PLAN.md` — Subagent execution plan with Groups A-H
- `migration_path.md` — Original discussion establishing golden master approach
- `legacy/src/` — Source C codebase (sthttpd 2.27.0)
