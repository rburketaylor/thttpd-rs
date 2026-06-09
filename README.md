# thttpd-rs

**A ground-up Rust port of [sthttpd](https://github.com/blueness/sthttpd) 2.27.0 — Jef Poskanzer's tiny/turbo/throttling HTTP server — backed by a multi-phase migration pipeline that proves byte-exact behavioral parity against the original C binary.**

---

## The Story

thttpd has served the internet faithfully since 1995. It's small (~9,800 lines of C), fast, secure, and does one thing extremely well: serve static files over HTTP. But it's written in C — manual memory management, raw pointer arithmetic, `fork()`/`exec()` CGI dispatch, and hand-rolled I/O multiplexing.

This project asks a question: **can you port a 30-year-old C server to Rust without changing a single observable behavior?** Not "looks similar." Not "mostly equivalent." **Byte-exact parity**, proven by automated differential testing against the original compiled binary.

The answer required building something more interesting than just a Rust HTTP server — it required a **migration pipeline**: a structured, gated, AI-assisted workflow that decomposes the problem, captures ground truth, translates module by module, and proves equivalence at every step.

---

## What's Here

```
thttpd-rs/
├── rust/                   # Rust workspace — 8 crates, 2,429 lines across 26 files
│   └── crates/
│       ├── thttpd-core/    # Event loop, startup, signals, throttling
│       ├── thttpd-http/    # HTTP parsing FSM, CGI, response building, directory listing
│       ├── thttpd-fdwatch/ # I/O multiplexing (thin mio wrapper)
│       ├── thttpd-timers/  # BinaryHeap timer wheel
│       ├── thttpd-mmc/     # Memory-mapped file cache (Rc<Mmap>)
│       ├── thttpd-match/   # Shell-style glob matching
│       ├── thttpd-tdate/   # HTTP date parsing (RFC 1123/850/asctime)
│       └── thttpd-mime/    # MIME type/encoding tables
│
├── legacy/                 # Upstream sthttpd source (see below)
├── harness/                # Golden Master test suite (Python/pytest)
│   ├── conftest.py         # Binary startup fixtures, port allocation
│   ├── diff_engine.py      # 8-field response comparator
│   └── tests/              # 80 test cases across 8 categories
│
├── pipeline/               # Migration automation scripts
│   ├── build_legacy.sh     # Compile the C binary for capture
│   ├── run_golden_capture.py
│   ├── run_differential.py
│   ├── generate_report.py
│   ├── analyze_module.py
│   └── validate_knowledge.py
│
├── knowledge/              # Structured knowledge graph (YAML + Markdown)
│   ├── _index.yaml         # Master manifest
│   ├── _architecture.yaml  # System architecture map
│   ├── _migration_map.yaml # Per-file migration status
│   └── modules/            # Per-module YAML + prose pairs
│
└── docs/                   # Migration plans and strategy
    ├── PLAN.md             # Master 6-phase migration plan
    ├── EXECUTION_PLAN.md   # Parallel subagent execution plan
    └── migration_path.md   # Pipeline philosophy
```

---

## The Migration Pipeline

The pipeline is a 6-phase gated workflow. No phase starts until the previous one is proven complete.

### Phase 0: Foundation

Repo setup, Cargo workspace initialization, and the **knowledge system** — a version-controlled, schema-enforced knowledge graph (`knowledge/`) that gives both humans and AI agents a structured understanding of every C module: its functions, callers, callees, complexity, gotchas, and undocumented behavior. Each module gets a machine-parseable YAML file and a prose Markdown companion.

### Phase 1: Analysis

Automated static analysis of the C source, producing dependency graphs, risk assessments (raw pointer arithmetic → ownership mapping, `fork()`/`execve()` → `std::process::Command`, `select()`/`poll()` → mio, global mutable state → `OnceLock`/`RwLock`), and per-module complexity scoring.

### Phase 2: Golden Master

The critical innovation. Instead of "translating and hoping," we **capture the C binary's exact behavior first**:

1. Compile the original C binary
2. Blast it with 80 black-box test cases across 8 categories
3. Capture every response (status code, headers, body, timing) into `baseline.json`

This becomes the **golden master** — the ground truth that the Rust binary must match.

**Test categories:** static files, CGI, headers, edge cases, malformed input, connection handling, errors, and bandwidth throttling.

### Phase 3: Translation

C → Rust, module by module, in dependency order (leaves first):

```
match.c → thttpd-match         (91 → 132 lines)
mime_types.h → thttpd-mime     (190 → 95 lines)
tdate_parse.c → thttpd-tdate   (330 → 228 lines)
fdwatch.c → thttpd-fdwatch     (838 → 72 lines)
timers.c → thttpd-timers       (403 → 253 lines)
mmc.c → thttpd-mmc             (529 → 200 lines)
libhttpd.c → thttpd-http       (4,230 → 995 lines)
thttpd.c → thttpd-core         (2,189 → 454 lines)
```

Each module follows a strict translation protocol: read the knowledge YAML, translate function by function, compile-gate (must pass `cargo check`), then unit-test-gate.

### Phase 4: Verification — The Differential Test Loop

The moment of truth:

1. Start the Rust binary
2. Replay the exact same golden master test suite
3. Diff every response across 8 dimensions: status code, status text, header count, header order, header values, body SHA-256, body length, and connection result
4. If anything diverges, feed the failure back to the AI with the exact C behavior, the Rust behavior, and the relevant source — then loop

This is not "looks about right." This is **byte-exact proof of equivalence**.

### Phase 5: Modernization

Once 100% of golden master tests pass, the logic is frozen. Now — and only now — we polish:

- Replace magic numbers with enums
- Add comprehensive `rustdoc` examples
- Standardize error handling with `thiserror`
- Finalize documentation and knowledge system

---

## Architecture Decisions

| C Pattern | Rust Equivalent | Why |
|-----------|----------------|-----|
| `select()`/`poll()` + hand-rolled state machine | `mio` (epoll/kqueue) | Same event-driven model, OS-native |
| `mmap()` + manual `refcount` | `Rc<Mmap>` + `Rc::strong_count()` | Rust's ownership tracks references automatically |
| Hash-of-sorted-lists timers | `BinaryHeap<Reverse<TimerEntry>>` | Lazy cancellation, same O(log n) performance |
| `fork()`/`execve()` + interposer | `std::process::Command` | Cleaner stdio pipelining, no interposer needed |
| Global `static` mutable state | `AtomicBool` flags + `OnceLock` | Thread-safe where needed, zero-cost where not |
| `httpd_conn` struct (40+ fields, raw pointers) | `HttpConn` (owned `String`/`Vec<u8>`) | No borrowed pointers into read buffer |
| C string handling (null-termination) | `String`/`Vec<u8>` | Automatic bounds checking |
| `setuid`/`chroot` via direct syscalls | `nix` crate | Typed wrappers around the same syscalls |

**No async runtime.** The server uses `mio` directly with a manual single-threaded event loop, deliberately matching thttpd's original architecture. This is not an accident — it's the design.

---

## The Harness

The test harness is a Python/pytest framework designed for **differential black-box testing**:

```
harness/
├── conftest.py              # Fixtures: server startup, port allocation, temp www root
├── diff_engine.py           # 8-field response comparator
└── tests/
    ├── test_static_files.py    # 10 tests: text, binary, large, zero-length, Range, etc.
    ├── test_cgi.py             # 10 tests: CGI execution, NPH, env vars, path-info
    ├── test_headers.py         # 10 tests: Content-Type, Date, gzip, virtual hosting
    ├── test_edge_cases.py      # 10 tests: HTTP/0.9, keep-alive, directory traversal
    ├── test_malformed.py       # 10 tests: invalid method, binary garbage, pipelining
    ├── test_connection.py      # 10 tests: TCP lifecycle, slow loris, max connections
    ├── test_errors.py          # 10 tests: 404, 403, 400, error page format
    └── test_throttling.py      # 10 tests: rate limiting, fair share, rolling average
```

The `diff_engine` compares responses across 8 fields — not just "did it return 200?" but "are the headers in the same order? Is the body byte-identical? Did the connection close the same way?"

---

## CI Pipeline

Every commit runs through a 5-job GitHub Actions workflow:

```
knowledge-consistency ──── validates YAML schemas & cross-references
build-legacy ───────────── compiles the C binary
build-rust ─────────────── compiles the Rust workspace
  └── unit-tests ────────── 48 unit tests across all crates
  └── differential-tests ── golden capture + differential testing
```

The `knowledge-consistency` job is the gatekeeper: it validates that every C module has a corresponding YAML+MD pair, that the migration map covers all modules, and that statuses are consistent. If the knowledge system is wrong, nothing else runs.

---

## Building

### Prerequisites

- Rust 1.85+ (pinned via `rust-toolchain.toml`)
- C compiler (gcc/clang) for the legacy binary
- Python 3.10+ with pytest and pyyaml

### Build the Rust workspace

```bash
cargo build --manifest-path rust/Cargo.toml --workspace
```

### Build the legacy C binary

```bash
bash pipeline/build_legacy.sh
```

### Run unit tests

```bash
cargo test --manifest-path rust/Cargo.toml --workspace
```

### Validate the knowledge system

```bash
python pipeline/validate_knowledge.py
```

---

## Numbers at a Glance

| Metric | C (sthttpd) | Rust (thttpd-rs) |
|--------|-------------|-------------------|
| Source files | 17 | 26 |
| Lines of code | 9,817 | 2,429 |
| Modules | 7 + headers | 8 crates |
| Unit tests | — | 48 |
| Golden master tests | — | 80 (8 categories) |
| External dependencies | libc | mio, nix, clap, memmap2, thiserror, signal-hook, slab |

The Rust implementation is ~4× more compact despite adding full type safety, comprehensive error handling, and 48 unit tests. The `libhttpd.c` → `thttpd-http` translation alone compresses 4,230 lines of C into 995 lines of Rust — a 4.3× reduction.

---

## Legacy Source

The `legacy/` directory contains the upstream [sthttpd](https://github.com/blueness/sthttpd) source by Anthony G. Basile, which is itself a maintained fork of Jef Poskanzer's original [thttpd](http://www.acme.com/software/thttpd/). It is included for differential testing only. All credit for the original implementation belongs to those authors.

---

## License

The original thttpd is released under a BSD 2-Clause license by Jef Poskanzer. This Rust port follows the same license. See the [legacy README](legacy/README.md) for the full text.
