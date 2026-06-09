# thttpd → Rust Migration Plan

## Context

**Goal:** Migrate the [sthttpd (thttpd)](https://github.com/blueness/sthttpd) codebase — a ~8,600-line C HTTP server (11,396 including headers) — to Rust, achieving byte-exact behavioral parity with the original compiled binary. Alongside the migration, build a structured documentation and testing framework that persists institutional knowledge inside the repo.

**Why thttpd:** Small enough for a single person + AI agents to complete, complex enough to exercise real migration challenges (raw sockets, CGI, file I/O, HTTP parsing, throttling, signal handling, chroot, memory-mapped caching, timer management). Widely deployed, BSD-licensed, and has a clean module structure.

**Source code map** (from `blueness/sthttpd`):

```
src/
├── thttpd.c          # Main server loop, connection management, throttling (2,189 lines)
├── thttpd.h          # Main header — server structs, globals, prototypes (398 lines)
├── libhttpd.c        # HTTP protocol library (parsing, request/response, CGI) (4,230 lines)
├── libhttpd.h        # HTTP library header — httpd_conn, httpd_server structs (284 lines)
├── fdwatch.c         # I/O multiplexing abstraction (select/poll/kqueue/devpoll) (838 lines)
├── fdwatch.h
├── timers.c          # Timer management — timeout callbacks, scheduled events (403 lines)
├── timers.h
├── mmc.c             # Memory-mapped file cache — mmap-based file serving (529 lines)
├── mmc.h
├── match.c           # Shell-style glob matching for URL patterns (91 lines)
├── match.h
├── tdate_parse.c     # HTTP date parsing (330 lines)
├── tdate_parse.h
├── mime_types.h      # MIME type table (generated)
├── mime_encodings.h  # MIME encoding table (generated)
├── make_mime.pl      # Perl script to generate MIME tables
├── version.h         # Version string
├── Makefile.am
scripts/              # Contrib scripts
extras/               # Extras (htpasswd, etc.)
docs/                 # Man pages
www/                  # Default web root
```

---

## Research Foundations

This plan draws from five key sources:

| Source | Key Takeaway |
|--------|-------------|
| **Noricum** (JuanMarchetto) | 9-stage gated pipeline with differential testing and repair loops |
| **VirtusLab** agent migration | 3-phase: Document → Code → Iteratively diff-test and refine |
| **Google LLM migration at scale** | File-group batching, configurable validation gates, ML-powered repair, sharded human review (general code migrations, not C→Rust specific) |
| **CodeGeeks 9-step safe workflow** | Behavior baseline (golden master), characterization tests, PR size rules |
| **RustAssure / Syzygy** | Differential symbolic testing for C→Rust equivalence verification |

---

## Approach

The migration uses a **6-phase gated pipeline**. Each phase has an explicit entry condition, exit gate, and set of deliverables. No phase begins until the previous phase's exit gate passes.

```
Phase 0: Foundation ─── repo setup, knowledge scaffolding, build baseline
Phase 1: Analysis ────── automated code comprehension, dependency mapping
Phase 2: Golden Master ── black-box characterization testing of C binary
Phase 3: Translation ──── chunk-by-chunk C→Rust with compile gates
Phase 4: Verification ─── differential testing, repair loops, parity proof
Phase 5: Modernization ── idiomatic Rust pass, documentation, CI hardening
```

---

## Phase 0: Foundation — Repo Setup & Knowledge Scaffolding

### 0.1 Clone & Establish Monorepo Structure

```
thttpd-migration/
├── legacy/                    # Pristine copy of sthttpd (git subtree)
├── rust/                      # New Rust workspace
│   ├── Cargo.toml
│   ├── crates/
│   │   ├── thttpd-core/       # Server main loop, connection management
│   │   ├── thttpd-http/       # HTTP parsing, CGI, response building
│   │   ├── thttpd-fdwatch/    # I/O multiplexing abstraction
│   │   ├── thttpd-timers/     # Timer management, scheduled callbacks
│   │   ├── thttpd-mmc/        # Memory-mapped file cache
│   │   ├── thttpd-match/      # Glob matching
│   │   ├── thttpd-tdate/      # Date parsing
│   │   └── thttpd-mime/       # MIME type tables
│   └── tests/
│       └── integration/
├── harness/                   # Golden Master test suite (Python)
│   ├── pytest.ini
│   ├── conftest.py
│   ├── golden/
│   │   ├── baseline.json      # Captured C binary responses
│   │   └── fixtures/          # Static files, malformed payloads
│   ├── tests/
│   │   ├── test_static_files.py
│   │   ├── test_cgi.py
│   │   ├── test_headers.py
│   │   ├── test_edge_cases.py
│   │   └── test_malformed.py
│   └── diff_engine.py         # Response comparison logic
├── knowledge/                 # Structured project knowledge (see §0.2)
├── pipeline/                  # Automation scripts
│   ├── build_legacy.sh
│   ├── run_golden_capture.py
│   ├── run_differential.py
│   └── generate_report.py
├── adr/                       # Architecture Decision Records
└── PLAN.md                    # This file
```

**Exit gate:** `legacy/` builds with `./configure && make`. `rust/` workspace initializes with `cargo check` passing on empty crates.

### 0.2 Structured Knowledge System — The `knowledge/` Directory

This is not a pile of markdown files. It is a **version-controlled, schema-enforced knowledge graph** that agents and humans both read/write. Structure:

```
knowledge/
├── _index.yaml                 # Master manifest: all modules, their status, dependencies
├── _architecture.yaml          # System-level architecture map (components, data flow)
├── _migration_map.yaml         # Per-file migration status tracker
│
├── modules/                    # One YAML+MD pair per C source module
│   ├── thttpd.yaml             # Structured: functions, globals, callers, callees, complexity
│   ├── thttpd.md               # Prose: what this module does, gotchas, undocumented behavior
│   ├── libhttpd.yaml
│   ├── libhttpd.md
│   ├── fdwatch.yaml
│   ├── fdwatch.md
│   ├── match.yaml
│   ├── match.md
│   ├── tdate_parse.yaml
│   ├── tdate_parse.md
│   ├── timers.yaml
│   ├── timers.md
│   ├── mmc.yaml
│   └── mmc.md
│
├── concepts/                   # Cross-cutting concerns
│   ├── http_protocol.md        # How thttpd implements HTTP/1.1
│   ├── connection_lifecycle.md # fd → parse → respond → close
│   ├── throttling.md           # Bandwidth throttling logic
│   ├── cgi_model.md            # CGI execution environment
│   ├── signal_handling.md      # SIGTERM, SIGHUP, SIGUSR1
│   └── security_model.md       # chroot, setuid, symlink checks
│
├── decisions/                  # ADR-style decision records
│   ├── 001-crate-boundaries.md
│   ├── 002-mio-vs-epoll.md
│   ├── 003-error-handling-strategy.md
│   └── template.md
│
├── learnings/                  # Migration discoveries (bugs found, quirks documented)
│   └── {timestamp}-{slug}.md
│
└── queries/                    # Agent Q&A log — past questions with answers for future sessions
    └── {topic}.md
```

#### Why YAML+MD, not just markdown:

| Problem | YAML+MD solution |
|---------|-----------------|
| "What functions does `libhttpd.c` export?" | `modules/libhttpd.yaml` → `functions:` list, machine-parseable |
| "Which modules call `httpd_parse_request`?" | `modules/libhttpd.yaml` → `callers:` field, auto-populated by analysis |
| "What's the migration status of `fdwatch`?" | `_migration_map.yaml` → single source of truth, CI-checkable |
| "Why did we choose mio over raw epoll?" | `decisions/002-mio-vs-epoll.md` → ADR format, linkable |
| "What undocumented quirks exist?" | `modules/*.md` → prose section, `learnings/` → dated discoveries |
| "How does a new AI agent get oriented?" | `_index.yaml` + `_architecture.yaml` → machine-readable entry point |

#### Schema for `modules/*.yaml`:

```yaml
module: libhttpd
file: src/libhttpd.c
header: src/libhttpd.h
lines: 4230
status: analyzed  # analyzed | translating | compiled | verified | modernized
complexity: high  # low | medium | high
description: HTTP protocol library — request parsing, response building, CGI execution

functions:
  - name: httpd_parse_request
    signature: "int httpd_parse_request(httpd_conn *hc)"
    callers: [thttpd.c:handle_connection]
    callees: [httpd_parse_headers, httpd_parse_query]
    complexity: high
    notes: "Contains undocumented behavior for malformed Transfer-Encoding headers"
    migration_target: thttpd-http::parse::parse_request

globals:
  - name: httpd_conn
    type: struct
    fields: [...]
    notes: "Opaque to callers, allocated per-connection"

dependencies:
  imports: [fdwatch, match, tdate_parse, mmc]
  imported_by: [thttpd]

gotchas:
  - "Line 1247: silently accepts negative Content-Length on some platforms"
  - "CGI env variable order matters for some legacy scripts"
```

**Automation:** A Python script (`pipeline/analyze_module.py`) auto-generates the YAML skeleton from C source using `ctags` + `callgraph` + line counting. The agent then enriches it during analysis.

**Exit gate:** `_index.yaml` lists all modules. `modules/*.yaml` files exist with at least `module`, `file`, `lines`, `status` fields. `decisions/template.md` is in place.

---

## Phase 1: Analysis — Automated Code Comprehension

### 1.1 Static Analysis of Legacy Code

For each C source file, run automated analysis to populate `knowledge/modules/*.yaml`:

```bash
# Dependency extraction
ctags -R --fields=+ne --extras=+q legacy/src/

# Call graph generation (using gcc/cscope)
cscope -Rbk -i legacy/src/

# Line counts, cyclomatic complexity
lizard legacy/src/   # or pmccabe
```

### 1.2 Agent-Driven Deep Analysis

For each module (in dependency order), an AI agent:

1. **Reads** the full C source file
2. **Produces** the enriched `knowledge/modules/{name}.yaml` with:
   - Complete function signatures and call graphs
   - Global state and shared mutable data
   - Implicit invariants and undocumented behavior
   - Error handling patterns (which errors are caught, which are silently ignored)
   - Platform-specific `#ifdef` branches
3. **Writes** `knowledge/modules/{name}.md` — a human-readable module guide
4. **Identifies** cross-cutting concerns → entries in `knowledge/concepts/`

### 1.3 Module Dependency Graph

Generate a visual dependency map:
```
thttpd.c ──→ libhttpd.c ──→ match.c
    │              │
    ├──→ fdwatch.c ├──→ tdate_parse.c
    ├──→ timers.c  └──→ mmc.c
    └──→ mmc.c ────→ timers.c
```

This drives the **translation order** in Phase 3: leaf modules first (`match`, `tdate_parse`, `fdwatch`), then infrastructure (`timers`, `mmc`), then `libhttpd`, then `thttpd`.

### 1.4 Identify Migration Risk Areas

Flag high-risk constructs per module:
- Raw pointer arithmetic → needs careful Rust ownership mapping
- `fork()`/`execve()` for CGI → Rust `std::process::Command` with careful fd inheritance
- `select()/poll()` with hand-rolled state machines → mio or epoll abstraction
- Global mutable state (`static` globals) → needs `OnceLock`/`RwLock` strategy
- String handling with manual null-termination → Rust `String`/`Vec<u8>` mapping

Record these in `knowledge/modules/*.yaml` under `gotchas:` and `complexity:`.

**Exit gate:** Every `.c` file has a corresponding `.yaml` + `.md` in `knowledge/modules/`. `_migration_map.yaml` shows all modules with `status: analyzed`. Dependency graph is generated and committed.

---

## Phase 2: Golden Master — Black-Box Characterization Testing

This is the **most critical phase**. Before writing a single line of Rust, capture exactly how the C binary behaves under every condition we can think of.

### 2.1 Build the C Binary for Capture

```bash
pipeline/build_legacy.sh
# Compiles legacy/ with debug symbols, fixed port (8080), temp www root
```

### 2.2 Test Categories & Fixture Generation

| Category | What it tests | Fixtures |
|----------|--------------|----------|
| **Static file serving** | GET for text, binary, large files, zero-length files, symlinks | `fixtures/www/{small.html, large.bin, empty.txt, symlink.html}` |
| **HTTP methods** | GET, HEAD, POST (CGI), OPTIONS, invalid methods | Raw socket requests |
| **Header parsing** | Host, If-Modified-Since, Range, Content-Type, Connection | Programmatic header matrices |
| **CGI execution** | Script output, environment variables, POST body passing, NPH scripts | `fixtures/cgi-bin/{hello.sh, env.sh, nph-script.sh}` |
| **Malformed input** | Truncated requests, missing CRLF, binary garbage, oversized headers, negative Content-Length | Fuzzer-generated payloads |
| **Connection behavior** | Keep-alive, early disconnect, pipelined requests, slow loris | Raw socket with timing |
| **Error responses** | 404, 403, 400, 405, 413, 500 — exact body and headers for each | Error-triggering requests |
| **Throttling** | Bandwidth rate limiting under load | `throttle_rate` config + large file |
| **Edge cases** | Requests for `/../etc/passwd`, URL-encoded paths, null bytes in URL | Security-oriented payloads |

### 2.3 Capture Protocol

For each test case, record a JSON snapshot:

```json
{
  "id": "static-small-html",
  "request": {
    "method": "GET",
    "path": "/small.html",
    "headers": {"Host": "localhost"},
    "body": null
  },
  "response": {
    "status_code": 200,
    "status_text": "OK",
    "headers": {
      "Content-Type": "text/html",
      "Content-Length": "142",
      "Last-Modified": "..."
    },
    "header_order": ["Date", "Server", "Last-Modified", "Content-Type", "Content-Length", "Connection"],
    "body_sha256": "abc123...",
    "body_bytes": 142
  },
  "timing_ms": 3,
  "connection_result": "closed"
}
```

Key: **capture header order** and **exact status text** — these are where silent behavioral differences hide.

### 2.4 Capture Execution

```bash
# Start C binary on port 8080
pipeline/build_legacy.sh && legacy/src/thttpd -p 8080 -d -r ./harness/golden/fixtures/www

# Run full capture suite
python harness/run_golden_capture.py --port 8080 --output harness/golden/baseline.json

# Generates: harness/golden/baseline.json (the golden master)
```

**Exit gate:** `harness/golden/baseline.json` exists with ≥200 test cases covering all categories above. All test cases are reproducible (run twice → identical JSON output). Tests committed to repo.

---

## Phase 3: Translation — Chunk-by-Chunk C→Rust

### 3.1 Translation Order (Dependency-Driven)

Following the dependency graph from Phase 1, translate bottom-up:

```
Batch 1 (leaf modules, no dependencies):
  ├── match.c      → thttpd-match/
  ├── tdate_parse  → thttpd-tdate/
  └── fdwatch.c    → thttpd-fdwatch/

Batch 2 (infrastructure modules):
  └── timers.c     → thttpd-timers/  (depends on fdwatch for event loop integration)

Batch 3 (core libraries):
  ├── mmc.c        → thttpd-mmc/     (depends on timers for cache expiry)
  └── libhttpd.c   → thttpd-http/    (depends on match, tdate_parse, mmc)

Batch 4 (main executable):
  └── thttpd.c     → thttpd-core/    (depends on libhttpd, fdwatch, timers)

Batch 5 (glue):
  └── Integration, main.rs, config parsing
```

### 3.2 Per-Module Translation Protocol

For each module, follow this gated sequence:

```
Step 1: Read knowledge/modules/{name}.yaml + .md
Step 2: Read the full C source file
Step 3: Write Rust translation in the target crate
Step 4: cargo check → if fail, feed error back to agent, retry (max 5 attempts)
Step 5: cargo clippy → fix all warnings
Step 6: cargo test (unit tests for this module)
Step 7: Update knowledge/modules/{name}.yaml status → compiled
```

### 3.3 Translation Constraints (Agent Prompt Rules)

Every agent translation prompt includes:

> 1. Translate this C module to safe Rust. Use `std` library only. Use `mio` for I/O multiplexing (replacing `fdwatch`).
> 2. Maintain 1:1 structural mapping to the C original. Do NOT introduce async/await, tokio, or hyper. This is a synchronous, `select()`/`poll()`-style server.
> 3. Map C error handling to Rust `Result<T, E>` with a module-specific error enum.
> 4. Every public function gets `///` rustdoc with: purpose, arguments, return value, and one usage example.
> 5. Preserve all undocumented behavior found in `knowledge/modules/{name}.yaml` under `gotchas:`.
> 6. Use `#[cfg(test)]` unit tests that mirror the C module's implicit invariants.
> 7. No `unsafe` blocks unless absolutely necessary and documented with a safety comment.

### 3.4 Compilation Gate

After each batch:

```bash
# Full workspace compile
cd rust && cargo build 2>&1

# If errors: feed compiler output + offending file back to agent
# Loop up to 5 times
# If still failing: escalate to human, log in knowledge/learnings/
```

**Exit gate:** `cargo build` succeeds on the full workspace. All crate unit tests pass. `knowledge/_migration_map.yaml` shows all modules with `status: compiled`.

---

## Phase 4: Verification — Differential Testing & Parity Proof

### 4.1 The Differential Test Runner

This is the core verification loop — it proves behavioral equivalence:

```
┌──────────────────────────────────────────────────┐
│  1. Start Rust binary on port 8081               │
│  2. Load harness/golden/baseline.json            │
│  3. For each test case:                          │
│     a. Send the same request to Rust             │
│     b. Capture response                          │
│     c. Diff against baseline entry               │
│  4. Generate harness/golden/diff_report.json     │
│  5. If any mismatches:                           │
│     a. For each mismatch:                        │
│        - Collect: request, expected, actual       │
│        - Identify the Rust code path              │
│        - Feed to repair agent                     │
│     b. Re-compile, re-run from step 1            │
│     c. Max 5 repair cycles per mismatch          │
│  6. If 100% match: PASS                          │
└──────────────────────────────────────────────────┘
```

### 4.2 Diff Engine — What Gets Compared

```python
# harness/diff_engine.py

def compare_responses(expected, actual):
    checks = {
        "status_code":  expected.status_code == actual.status_code,
        "status_text":  expected.status_text == actual.status_text,
        "header_count": len(expected.headers) == len(actual.headers),
        "header_order": list(expected.headers.keys()) == list(actual.headers.keys()),
        "header_values": all(expected.headers[k] == actual.headers[k]
                            for k in expected.headers),
        "body_sha256":   expected.body_sha256 == actual.body_sha256,
        "body_length":   expected.body_bytes == actual.body_bytes,
        "connection":    expected.connection_result == actual.connection_result,
    }
    return checks
```

Strict mode: ALL checks must pass. Header order matters. Body byte-for-byte match (SHA-256).

### 4.3 Repair Loop Protocol

When a diff fails:

1. **Classify** the failure: status mismatch, header mismatch, body mismatch, or connection mismatch
2. **Trace**: Use the test case's request path to identify which Rust function handled it
3. **Feed to agent**:
   ```
   The Rust binary produced:
     Status: 404 Not Found
     Body: "File not found\n"
   
   The C binary produced:
     Status: 404 Not Found  
     Body: "File not found"     ← note: no trailing newline
   
   Relevant Rust code: thttpd-http/src/response.rs:142
   
   Fix the Rust code to match the C binary's exact output.
   ```
4. Agent patches → recompile → retest → loop
5. Log every repair in `knowledge/learnings/` with timestamp and outcome

### 4.4 Regression Guard

After 100% parity, lock the baseline:

```bash
# CI step: run differential tests, fail if any drift
python harness/run_differential.py --baseline harness/golden/baseline.json --port 8081
```

**Exit gate:** `harness/golden/diff_report.json` shows 100% pass rate across all ≥200 test cases. Report is committed as proof artifact. `_migration_map.yaml` shows all modules `status: verified`.

---

## Phase 5: Modernization — Idiomatic Rust & Documentation

**Only after Phase 4 passes 100%** — no logic changes, only structural improvements.

### 5.1 Idiomatic Rust Pass

For each crate, an agent performs a refinement pass with these constraints:

> This code is functionally verified. Do NOT alter execution logic. Apply these changes only:
> 1. Replace magic numbers with named constants or enums
> 2. Ensure all `pub` functions have complete rustdoc with `# Examples`
> 3. Standardize error handling: derive `thiserror::Error` for all error types
> 4. Replace any remaining `unsafe` with safe alternatives where possible (document why if not)
> 5. Add `#[must_use]` where appropriate
> 6. Run `cargo clippy -- -W clippy::pedantic` and address all warnings

### 5.2 Documentation Finalization

For each crate, ensure:
- `lib.rs` has a module-level doc comment with architecture overview
- Every public item has rustdoc
- `cargo doc --no-deps` generates clean HTML documentation

### 5.3 Knowledge System Finalization

- Update `_migration_map.yaml` → all modules `status: modernized`
- Write `knowledge/concepts/migration_summary.md` — what was learned, what surprised us
- Close all open ADRs with outcomes
- Archive `knowledge/learnings/` entries into a summary

### 5.4 CI Pipeline

```yaml
# .github/workflows/migration-ci.yml
name: Migration Verification
on: [push, pull_request]
jobs:
  build-legacy:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: cd legacy && ./configure && make

  build-rust:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions-rs/cargo@v1
        with: { command: build, args: "--workspace" }

  unit-tests:
    needs: build-rust
    runs-on: ubuntu-latest
    steps:
      - uses: actions-rs/cargo@v1
        with: { command: test, args: "--workspace" }

  differential-tests:
    needs: [build-legacy, build-rust]
    runs-on: ubuntu-latest
    steps:
      - run: ./pipeline/build_legacy.sh
      - run: cargo build --release --manifest-path rust/Cargo.toml
      - run: python harness/run_differential.py --strict
      - run: python harness/generate_report.py --output reports/diff_report.html

  knowledge-consistency:
    runs-on: ubuntu-latest
    steps:
      - run: python pipeline/validate_knowledge.py  # Check YAML schemas
```

**Exit gate:** CI pipeline is green. `cargo doc` generates clean docs. All knowledge YAML validates against schema. `_migration_map.yaml` shows all modules `status: modernized`.

---

## Knowledge Management System — Detailed Design

### The Problem with "Just Markdown"

A flat pile of `.md` files has no schema, no queryability, no structure. Agents can't reliably find information. Humans can't see what's stale. There's no way to track migration status programmatically.

### Our Solution: Schema-Enforced YAML + Prose Markdown + CI Validation

#### 1. `_index.yaml` — The Entry Point

```yaml
version: 1
project: thttpd-migration
last_updated: "2025-01-15"
modules:
  - name: match
    status: modernized
    file: src/match.c
    rust_crate: thttpd-match
  - name: tdate_parse
    status: verified
    file: src/tdate_parse.c
    rust_crate: thttpd-tdate
  - name: timers
    status: compiled
    file: src/timers.c
    rust_crate: thttpd-timers
  - name: mmc
    status: compiled
    file: src/mmc.c
    rust_crate: thttpd-mmc
  # ... etc
concepts:
  - http_protocol
  - connection_lifecycle
  - cgi_model
  - throttling
  - security_model
  - signal_handling
  - memory_mapped_cache
```

#### 2. `_migration_map.yaml` — Migration Progress Tracker

```yaml
phases:
  phase0_foundation: done
  phase1_analysis: done
  phase2_golden_master: done
  phase3_translation: in_progress  # updated as work proceeds
  phase4_verification: pending
  phase5_modernization: pending

files:
  match.c:       { status: modernized, rust: thttpd-match,   parity: 100% }
  tdate_parse.c: { status: verified,   rust: thttpd-tdate,   parity: 100% }
  fdwatch.c:     { status: compiled,   rust: thttpd-fdwatch, parity: null }
  timers.c:      { status: compiled,   rust: thttpd-timers,  parity: null }
  mmc.c:         { status: compiled,   rust: thttpd-mmc,     parity: null }
  libhttpd.c:    { status: translating, rust: thttpd-http,   parity: null }
  thttpd.c:      { status: analyzed,   rust: thttpd-core,    parity: null }

golden_master:
  total_cases: 247
  last_capture: "2025-01-15T10:30:00Z"
  c_binary_sha256: "abc123..."
```

CI validates this file: `pipeline/validate_knowledge.py` checks that statuses are valid enum values and that `parity` percentages are numbers or null.

#### 3. ADR Format (`decisions/`)

Each decision record follows Michael Nygard's ADR format:

```markdown
# ADR 002: Use mio for I/O multiplexing

## Status: Accepted

## Context
fdwatch.c implements select()/poll() abstraction. We need an equivalent in Rust.

## Decision
Use the `mio` crate (v1.0) for I/O multiplexing rather than raw epoll.

## Consequences
- (+) Cross-platform (mirrors fdwatch's select/poll abstraction)
- (+) Well-maintained, used in production by tokio
- (-) Adds a dependency
- (-) Slightly different API than raw poll, may need adapter layer
```

#### 4. Agent Context Protocol

When a new AI agent session starts, it reads:

1. `_index.yaml` → what modules exist, their status
2. `_migration_map.yaml` → what's done, what's in progress
3. The relevant `modules/{name}.yaml` for its assigned task
4. Any open `decisions/` that affect its work

This replaces "cold starts" — the agent enters with structured knowledge, not guessing.

---

## Files to Create/Modify

| Path | Purpose |
|------|---------|
| `PLAN.md` | This document |
| `pipeline/build_legacy.sh` | Compile C binary for testing |
| `pipeline/run_golden_capture.py` | Run golden master capture against C binary |
| `pipeline/run_differential.py` | Differential test runner (C vs Rust) |
| `pipeline/generate_report.py` | Generate HTML diff report |
| `pipeline/analyze_module.py` | Auto-generate YAML skeleton from C source |
| `pipeline/validate_knowledge.py` | CI check: validate knowledge YAML schemas |
| `harness/conftest.py` | Pytest configuration, binary startup fixtures |
| `harness/diff_engine.py` | Response comparison logic |
| `harness/golden/baseline.json` | Captured C binary responses (generated) |
| `harness/tests/*.py` | Golden master test suite |
| `knowledge/_index.yaml` | Master manifest |
| `knowledge/_architecture.yaml` | Architecture map |
| `knowledge/_migration_map.yaml` | Migration progress |
| `knowledge/modules/*.yaml` | Per-module structured analysis |
| `knowledge/modules/*.md` | Per-module prose docs |
| `knowledge/concepts/*.md` | Cross-cutting concern docs |
| `knowledge/decisions/*.md` | ADRs |
| `rust/Cargo.toml` | Workspace root |
| `rust/crates/*/Cargo.toml` | Per-crate manifests |
| `.github/workflows/migration-ci.yml` | CI pipeline |

---

## Verification Checklist

### After Phase 2 (Golden Master)
- [ ] `baseline.json` has ≥200 test cases
- [ ] Running capture twice produces identical JSON
- [ ] All HTTP method variants covered
- [ ] Malformed input cases present
- [ ] CGI execution cases present
- [ ] Header order captured for every response

### After Phase 3 (Translation)
- [ ] `cargo build --workspace` succeeds with zero errors
- [ ] `cargo clippy --workspace` has zero warnings
- [ ] `cargo test --workspace` passes all unit tests
- [ ] No `unsafe` blocks without documented safety justification
- [ ] Every `pub fn` has rustdoc

### After Phase 4 (Verification)
- [ ] Differential test passes 100% of golden master cases
- [ ] Body SHA-256 matches for every static file response
- [ ] Header order matches for every response
- [ ] CGI output matches byte-for-byte
- [ ] Error responses match exact status text
- [ ] Connection behavior (keep-alive / close) matches

### After Phase 5 (Modernization)
- [ ] `cargo doc --no-deps` generates clean HTML
- [ ] All magic numbers replaced with named constants
- [ ] `thiserror` used consistently for error types
- [ ] CI pipeline green on all checks
- [ ] Knowledge YAML validates against schema
- [ ] `_migration_map.yaml` shows all modules `modernized`

---

## Summary: What This Pipeline Gives You

1. **Proof of equivalence** — not "looks similar" but byte-exact differential testing against the original binary
2. **Structured institutional knowledge** — schema-enforced YAML that agents and humans can query, not a wall of markdown
3. **Gated progression** — no phase starts until the previous one is proven complete
4. **Repair loops** — automated feedback cycles when behavior diverges
5. **CI-enforced regression guard** — every commit proves parity hasn't drifted
6. **Living documentation** — knowledge evolves with the code, validated by CI
