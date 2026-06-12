# thttpd-rs

**A byte-exact Rust port of [sthttpd](https://github.com/blueness/sthttpd) 2.27.0 — Jef Poskanzer's tiny/turbo/throttling HTTP server — proven by automated differential testing against the original C binary.**

---

## The Story

thttpd has served the internet since 1995. It's small (~9,800 lines of C), fast, secure, and does one thing well: serve static files over HTTP with CGI support and bandwidth throttling. But it's written in C — manual memory management, raw pointer arithmetic, `fork()`/`exec()` CGI dispatch, and hand-rolled I/O multiplexing.

This project answers a question: **can you port a 30-year-old C server to Rust without changing a single observable behavior?** Not "looks similar." Not "mostly equivalent." **Byte-exact parity**, proven by running 81 differential tests that compare the C and Rust binaries side-by-side across status code, headers, body, and connection lifecycle.

The answer is **yes**. Every test passes:

| Suite | Result |
|---|---|
| Differential tests (C vs Rust) | **81 / 81** |
| C-only harness tests | **80 / 80** |
| Rust unit tests | **58 / 58** |
| **Total** | **219 / 219** |

---

## What's Here

```
thttpd-rs/
├── rust/                   # Rust workspace — 8 crates, 2,429 lines
│   ├── README.md           # Architecture, crate map, dependency graph
│   └── crates/
│       ├── thttpd-core/    # Event loop, startup, signals, throttling
│       ├── thttpd-http/    # HTTP parsing FSM, CGI, responses, directory listing
│       ├── thttpd-fdwatch/ # I/O multiplexing (thin mio wrapper)
│       ├── thttpd-timers/  # BinaryHeap timer wheel
│       ├── thttpd-mmc/     # Memory-mapped file cache (Rc<Mmap>)
│       ├── thttpd-match/   # Shell-style glob matching
│       ├── thttpd-tdate/   # HTTP date parsing (RFC 1123/850/asctime)
│       └── thttpd-mime/    # MIME type/encoding tables
│
├── legacy/                 # Upstream sthttpd source (for differential testing)
├── harness/                # Test suite (Python/pytest)
│   ├── conftest.py         # Server startup fixtures, port allocation
│   ├── diff_engine.py      # 8-field response comparator
│   └── tests/              # 80 test cases across 8 categories
│
├── pipeline/               # Build and validation scripts
│   ├── build_legacy.sh     # Compile the C binary
│   ├── validate_knowledge.py
│   ├── run_golden_capture.py
│   ├── run_differential.py
│   └── generate_report.py
│
├── knowledge/              # Structured migration records (YAML + Markdown)
│   ├── _index.yaml         # Master manifest
│   ├── _architecture.yaml  # System architecture map
│   ├── _migration_map.yaml # Per-file C→Rust mapping
│   └── modules/            # Per-module YAML + prose pairs
│
├── JOURNEY.md              # Development narrative — the story of how it got built
└── .github/workflows/      # CI pipeline
```

---

## Architecture

The Rust port preserves thttpd's original architecture — single-threaded, event-driven, no async runtime:

```
                              ┌─────────────┐
                              │  thttpd-core │  main(), event loop, signals
                              └──────┬───────┘
                                     │
              ┌──────────────────────┼──────────────────────┐
              │                      │                      │
     ┌────────┴────────┐   ┌────────┴────────┐   ┌────────┴────────┐
     │  thttpd-http    │   │  thttpd-fdwatch  │   │  thttpd-timers  │
     │  parse, CGI,    │   │  mio wrapper     │   │  timer wheel    │
     │  response, dir  │   └─────────────────┘   └─────────────────┘
     └───┬────┬────┬───┘
         │    │    │
    ┌────┘    │    └──────┐
    │         │           │
┌───┴───┐ ┌──┴───┐ ┌─────┴─────┐
│match  │ │ mmc  │ │  tdate    │
│globs  │ │cache │ │  dates    │
└───────┘ └──┬───┘ └───────────┘
             │
       ┌─────┴─────┐
       │   mime    │
       │  types    │
       └───────────┘
```

### C → Rust Design Decisions

| C Pattern | Rust Equivalent | Why |
|-----------|----------------|-----|
| `select()`/`poll()` + hand-rolled state machine | `mio` (epoll/kqueue) | Same event-driven model, OS-native |
| `mmap()` + manual `refcount` | `Rc<Mmap>` + `Rc::strong_count()` | Rust ownership tracks references automatically |
| Hash-of-sorted-lists timers | `BinaryHeap<Reverse<TimerEntry>>` | Lazy cancellation, same O(log n) |
| `fork()`/`execve()` + interposer | `std::process::Command` | Cleaner stdio pipelining |
| Global `static` mutable state | `AtomicBool` + `OnceLock` | Thread-safe where needed, zero-cost where not |
| `httpd_conn` struct (40+ raw pointer fields) | `HttpConn` (owned `String`/`Vec<u8>`) | No borrowed pointers into read buffer |
| C string handling (null-termination) | `String`/`Vec<u8>` | Automatic bounds checking |
| `setuid`/`chroot` | `nix` crate | Typed wrappers around same syscalls |

**No async runtime.** The server uses `mio` directly with a manual single-threaded event loop, matching thttpd's original architecture by design.

---

## The Test Harness

The harness tests both binaries side-by-side using **differential black-box testing**:

```python
# For each test:
c_response  = send_request(c_server, raw_bytes)
rust_response = send_request(rust_server, raw_bytes)
compare_responses(c_response, rust_response)  # 8-field diff
```

The `diff_engine` compares across 8 dimensions — not just "did it return 200?" but "are headers in the same order? Is the body byte-identical? Did the connection close the same way?"

### Test Categories

| File | Tests | What it covers |
|------|-------|---------------|
| `test_static_files.py` | 10 | Text, binary, large files, Range requests, If-Modified-Since |
| `test_cgi.py` | 10 | CGI execution, NPH scripts, env vars, path-info, POST body |
| `test_headers.py` | 10 | Content-Type, Date, gzip, virtual hosting, charset |
| `test_edge_cases.py` | 10 | HTTP/0.9, keep-alive, concurrent requests, directory traversal |
| `test_malformed.py` | 10 | Invalid methods, binary garbage, chunked encoding, pipelining |
| `test_connection.py` | 10 | TCP lifecycle, slow loris, idle cleanup, max connections |
| `test_errors.py` | 10 | 404, 403, 400, 501, error page format |
| `test_throttling.py` | 10 | Rate limiting, fair share, rolling average, byte counting |

---

## Building

### Prerequisites

- Rust 1.85+ (pinned via `rust-toolchain.toml`)
- C compiler (gcc/clang) for the legacy binary
- Python 3.10+ with pytest

### Build the Rust binary

```bash
cargo build --manifest-path rust/Cargo.toml --release
```

### Build the legacy C binary

```bash
bash pipeline/build_legacy.sh
```

### Run all tests

```bash
# Rust unit tests (58 tests)
cargo test --manifest-path rust/Cargo.toml --workspace

# C-only harness tests (80 tests)
python3 -m pytest harness/tests/ --ignore=harness/tests/test_differential.py -v

# Differential tests — C vs Rust side-by-side (81 tests)
python3 -m pytest harness/tests/test_differential.py -v --timeout=120 --timeout-method=thread

# Validate knowledge system consistency
python3 pipeline/validate_knowledge.py
```

---

## Numbers at a Glance

| Metric | C (sthttpd) | Rust (thttpd-rs) |
|--------|-------------|-------------------|
| Source files | 17 | 26 |
| Lines of code | 9,817 | 2,429 |
| Modules | 7 + headers | 8 crates |
| Differential tests | — | 81 (8 categories) |
| Harness tests | — | 80 (C-only, same 8 categories) |
| Unit tests | — | 58 |
| External dependencies | libc | mio, nix, clap, memmap2, thiserror, signal-hook, slab |

The Rust implementation is ~4× more compact despite adding full type safety, comprehensive error handling, and 58 unit tests. The `libhttpd.c` → `thttpd-http` translation compresses 4,230 lines of C into 995 lines of Rust — a 4.3× reduction.

---

## Migration Approach

The port was done in phases with automated gates:

1. **Foundation** — Repo setup, Cargo workspace, knowledge graph of every C module
2. **Analysis** — Static analysis of C source, dependency graphs, risk assessment
3. **Golden Master** — Capture the C binary's exact behavior into 80 test cases
4. **Translation** — C → Rust, module by module, in dependency order (leaves first)
5. **Verification** — Differential testing loop: run same tests against both binaries, diff every response, fix divergences
6. **Modernization** — Polish: enums, documentation, error handling

Each phase was gated — no phase started until the previous one was proven complete. The development narrative is in [`JOURNEY.md`](JOURNEY.md).

---

## Legacy Source

The `legacy/` directory contains the upstream [sthttpd](https://github.com/blueness/sthttpd) source by Anthony G. Basile, a maintained fork of Jef Poskanzer's original [thttpd](http://www.acme.com/software/thttpd/). It is included for differential testing. All credit for the original implementation belongs to those authors.

---

## License

The original thttpd is released under a BSD 2-Clause license by Jef Poskanzer. This Rust port follows the same license. See the [legacy README](legacy/README.md) for the full text.
