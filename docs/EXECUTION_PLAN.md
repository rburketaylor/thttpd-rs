# Subagent Execution Plan — thttpd Rust Migration

> **Purpose:** Split the 6-phase migration (PLAN.md) into parallel work streams that can be executed by independent subagent groups. Phase 0 is done by the main agent; Phases 1–5 are split across named groups with explicit dependencies.

---

## Overview: Group Structure

```
┌─────────────────────────────────────────────────────────────────┐
│  MAIN AGENT                                                     │
│  Phase 0: Foundation                                            │
│  (repo setup, workspace, knowledge scaffolding, build scripts)  │
└──────────────────────────┬──────────────────────────────────────┘
                           │
          ┌────────────────┼────────────────┐
          ▼                ▼                ▼
   ┌─────────────┐  ┌─────────────┐  ┌─────────────┐
   │  GROUP A    │  │  GROUP B    │  │  GROUP C    │
   │  Analysis:  │  │  Analysis:  │  │  Analysis:  │
   │  Leaf Mods  │  │  Infra Mods │  │  Core Mods  │
   │  (Phase 1)  │  │  (Phase 1)  │  │  (Phase 1)  │
   └──────┬──────┘  └──────┬──────┘  └──────┬──────┘
          │                │                │
          └────────────────┼────────────────┘
                           ▼
                    ┌─────────────┐
                    │  GROUP D    │
                    │  Harness    │
                    │  (Phase 2)  │
                    └──────┬──────┘
                           │
          ┌────────────────┼────────────────┐
          ▼                ▼                ▼
   ┌─────────────┐  ┌─────────────┐  ┌─────────────┐
   │  GROUP E    │  │  GROUP F    │  │  GROUP G    │
   │  Translate: │  │  Translate: │  │  Translate: │
   │  Leaf Mods  │  │  Infra Mods │  │  Core Mods  │
   │  (Phase 3)  │  │  (Phase 3)  │  │  (Phase 3)  │
   └──────┬──────┘  └──────┬──────┘  └──────┬──────┘
          │                │                │
          └────────────────┼────────────────┘
                           ▼
                    ┌─────────────┐
                    │  GROUP H    │
                    │  Verify &   │
                    │  Modernize  │
                    │  (Ph 4 + 5) │
                    └─────────────┘
```

---

## Phase 0: Foundation (Main Agent)

**Executor:** Main agent (you)
**Blocks:** All other groups

### Tasks

| # | Task | Deliverable |
|---|------|-------------|
| 0.1 | Create monorepo directory structure | `rust/`, `harness/`, `knowledge/`, `pipeline/`, `adr/` directories |
| 0.2 | Initialize Rust workspace with 8 empty crates | `rust/Cargo.toml` + 8 crate dirs with `lib.rs` stubs, `cargo check` passes |
| 0.3 | Create `rust-toolchain.toml` | Pin Rust edition 2024, stable channel |
| 0.4 | Build knowledge scaffolding | `_index.yaml`, `_architecture.yaml`, `_migration_map.yaml`, `modules/`, `concepts/`, `decisions/`, `learnings/`, `queries/` — all with stub content |
| 0.5 | Create ADR template | `knowledge/decisions/template.md` (Nygard format) |
| 0.6 | Write `pipeline/build_legacy.sh` | Script that compiles `legacy/src/` and produces a runnable binary |
| 0.7 | Build the legacy binary | Verify `legacy/src/thttpd` compiles and runs |
| 0.8 | Create `pipeline/analyze_module.py` | Script that runs ctags + lizard on a C file and outputs YAML skeleton |
| 0.9 | Create `pipeline/validate_knowledge.py` | Script that validates YAML schema (status enums, required fields) |
| 0.10 | Write `.gitignore` | Exclude `target/`, `__pycache__/`, `*.o`, `legacy/src/thttpd`, `harness/golden/baseline.json` |
| 0.11 | Initialize git repo, first commit | Clean commit with all scaffolding |

### Exit Gate

- [ ] `rust/` workspace exists with 8 crates, `cargo check` passes
- [ ] `knowledge/_index.yaml` lists all 7 modules
- [ ] `knowledge/_migration_map.yaml` has all 7 files with `status: pending`
- [ ] `pipeline/build_legacy.sh` produces a working binary
- [ ] `pipeline/analyze_module.py` generates valid YAML from a C source file
- [ ] `pipeline/validate_knowledge.py` runs clean on stub YAML
- [ ] First git commit is clean

---

## Phase 1: Analysis (3 Parallel Groups)

**Blocks:** Phase 3 (Translation) for respective modules
**Blocked by:** Phase 0

### GROUP A — Analysis: Leaf Modules

**Modules:** `match.c` (91 lines), `tdate_parse.c` (330 lines)
**Parallel with:** Groups B, C
**Estimated effort:** Low (small, self-contained modules)

| # | Task | Deliverable |
|---|------|-------------|
| A.1 | Run `pipeline/analyze_module.py` on `match.c` | `knowledge/modules/match.yaml` skeleton |
| A.2 | Deep-read `match.c` + `match.h`, enrich YAML | Complete function list, callers, callees, gotchas |
| A.3 | Write `knowledge/modules/match.md` | Prose: what it does, shell-glob algorithm, edge cases |
| A.4 | Run `pipeline/analyze_module.py` on `tdate_parse.c` | `knowledge/modules/tdate_parse.yaml` skeleton |
| A.5 | Deep-read `tdate_parse.c` + `tdate_parse.h`, enrich YAML | Complete function list, date format coverage, gotchas |
| A.6 | Write `knowledge/modules/tdate_parse.md` | Prose: HTTP date formats, parsing quirks |
| A.7 | Update `_migration_map.yaml` | `match.c` and `tdate_parse.c` → `status: analyzed` |

**Exit Gate:** Both `.yaml` files have `status: analyzed`, all functions documented, both `.md` files written.

---

### GROUP B — Analysis: Infrastructure Modules

**Modules:** `fdwatch.c` (838 lines), `timers.c` (403 lines), `mmc.c` (529 lines)
**Parallel with:** Groups A, C
**Estimated effort:** Medium (platform-specific code, multiple `#ifdef` backends)

| # | Task | Deliverable |
|---|------|-------------|
| B.1 | Run `pipeline/analyze_module.py` on `fdwatch.c` | `knowledge/modules/fdwatch.yaml` skeleton |
| B.2 | Deep-read `fdwatch.c` + `fdwatch.h`, enrich YAML | All 4 backends (kqueue/devpoll/poll/select), function table, global state, platform `#ifdef` map |
| B.3 | Write `knowledge/modules/fdwatch.md` | Prose: abstraction design, backend selection logic, FD_SETSIZE limits |
| B.4 | Run `pipeline/analyze_module.py` on `timers.c` | `knowledge/modules/timers.yaml` skeleton |
| B.5 | Deep-read `timers.c` + `timers.h`, enrich YAML | Timer data structures, scheduling algorithm, callback mechanism |
| B.6 | Write `knowledge/modules/timers.md` | Prose: timer model, resolution, interaction with fdwatch |
| B.7 | Run `pipeline/analyze_module.py` on `mmc.c` | `knowledge/modules/mmc.yaml` skeleton |
| B.8 | Deep-read `mmc.c` + `mmc.h`, enrich YAML | mmap strategy, cache eviction, ref counting |
| B.9 | Write `knowledge/modules/mmc.md` | Prose: caching model, memory pressure handling |
| B.10 | Write `knowledge/concepts/memory_mapped_cache.md` | Cross-cutting: how mmc interacts with timers and fdwatch |
| B.11 | Update `_migration_map.yaml` | `fdwatch.c`, `timers.c`, `mmc.c` → `status: analyzed` |

**Exit Gate:** All 3 `.yaml` files have `status: analyzed`, platform backends fully documented, `.md` files written.

---

### GROUP C — Analysis: Core Modules

**Modules:** `libhttpd.c` (4,230 lines), `thttpd.c` (2,189 lines)
**Parallel with:** Groups A, B
**Estimated effort:** High (largest files, most complex logic)

| # | Task | Deliverable |
|---|------|-------------|
| C.1 | Run `pipeline/analyze_module.py` on `libhttpd.c` | `knowledge/modules/libhttpd.yaml` skeleton |
| C.2 | Deep-read `libhttpd.c` + `libhttpd.h`, enrich YAML | All functions, CGI fork/exec paths, connection state machine, error handling patterns |
| C.3 | Write `knowledge/modules/libhttpd.md` | Prose: HTTP parsing model, CGI execution flow, undocumented behaviors |
| C.4 | Write `knowledge/concepts/http_protocol.md` | Cross-cutting: how thttpd implements HTTP/1.1 |
| C.5 | Write `knowledge/concepts/cgi_model.md` | Cross-cutting: fork/exec, env vars, stdin/stdout piping, NPH scripts |
| C.6 | Run `pipeline/analyze_module.py` on `thttpd.c` | `knowledge/modules/thttpd.yaml` skeleton |
| C.7 | Deep-read `thttpd.c` + `thttpd.h`, enrich YAML | Main loop, connection lifecycle, signal handlers, chroot/setuid, throttling |
| C.8 | Write `knowledge/modules/thttpd.md` | Prose: server architecture, configuration, startup sequence |
| C.9 | Write `knowledge/concepts/connection_lifecycle.md` | Cross-cutting: fd → parse → respond → close |
| C.10 | Write `knowledge/concepts/throttling.md` | Cross-cutting: bandwidth rate limiting logic |
| C.11 | Write `knowledge/concepts/signal_handling.md` | Cross-cutting: SIGTERM, SIGHUP, SIGUSR1 |
| C.12 | Write `knowledge/concepts/security_model.md` | Cross-cutting: chroot, setuid, symlink checks |
| C.13 | Generate full dependency graph | `_architecture.yaml` with complete component map |
| C.14 | Update `_migration_map.yaml` | `libhttpd.c`, `thttpd.c` → `status: analyzed` |

**Exit Gate:** Both `.yaml` files have `status: analyzed`, all 6 concept docs written, `_architecture.yaml` complete, dependency graph committed.

---

## Phase 2: Golden Master (1 Group + Parallel Test Writers)

**Blocks:** Phase 4 (Verification)
**Blocked by:** Phase 0 (binary must build)
**Can overlap with:** Phase 1 (analysis groups)

### GROUP D — Harness & Test Suite

**Sub-group D1: Infrastructure** (sequential, runs first)
**Sub-group D2: Test Writers** (parallel, after D1)

#### D1 — Harness Infrastructure

| # | Task | Deliverable |
|---|------|-------------|
| D1.1 | Create `harness/conftest.py` | Pytest fixtures: binary startup/shutdown, port allocation, temp www root |
| D1.2 | Create `harness/diff_engine.py` | Response comparison: status code, status text, header order, header values, body SHA-256, connection result |
| D1.3 | Create `harness/pytest.ini` | Test configuration, markers, timeouts |
| D1.4 | Create `pipeline/run_golden_capture.py` | Script that starts C binary, runs all tests, captures JSON baseline |
| D1.5 | Create `pipeline/run_differential.py` | Script that starts Rust binary, replays baseline, diffs responses |
| D1.6 | Create `pipeline/generate_report.py` | HTML diff report generator |
| D1.7 | Create fixture files | `harness/golden/fixtures/www/{small.html, large.bin, empty.txt, symlink.html}` |
| D1.8 | Create CGI fixtures | `harness/golden/fixtures/cgi-bin/{hello.sh, env.sh, nph-script.sh}` |

#### D2 — Test Suite Writers (parallel after D1)

| # | Task | Tests | Deliverable |
|---|------|-------|-------------|
| D2.1 | Static file tests | GET text, binary, large, zero-length, symlinks, If-Modified-Since, Range | `harness/tests/test_static_files.py` |
| D2.2 | CGI tests | Script output, env vars, POST body, NPH scripts | `harness/tests/test_cgi.py` |
| D2.3 | Header tests | Host, Content-Type, Connection, custom headers, header matrices | `harness/tests/test_headers.py` |
| D2.4 | Edge case tests | `/../etc/passwd`, URL-encoded paths, null bytes in URL | `harness/tests/test_edge_cases.py` |
| D2.5 | Malformed input tests | Truncated requests, missing CRLF, binary garbage, oversized headers, negative Content-Length | `harness/tests/test_malformed.py` |
| D2.6 | Connection tests | Keep-alive, early disconnect, pipelined requests, slow loris | `harness/tests/test_connection.py` |
| D2.7 | Error response tests | 404, 403, 400, 405, 413, 500 — exact body and headers | `harness/tests/test_errors.py` |
| D2.8 | Throttling tests | Bandwidth rate limiting under load | `harness/tests/test_throttling.py` |
| D2.9 | Capture baseline | Run full suite against C binary → `harness/golden/baseline.json` | ≥200 test cases, reproducible |

**Exit Gate:** `baseline.json` has ≥200 cases, running capture twice produces identical JSON, all test categories covered, tests committed.

---

## Phase 3: Translation (3 Groups, Dependency-Batched)

**Blocked by:** Phase 1 (respective module analysis complete)

### GROUP E — Translation: Leaf Modules

**Modules:** `match.c` → `thttpd-match`, `tdate_parse.c` → `thttpd-tdate`, `fdwatch.c` → `thttpd-fdwatch`
**Parallel with:** Groups F, G (once their analysis is done)
**Blocked by:** Group A (analysis complete)

All 3 crates are leaf modules with no inter-dependencies. They can be translated in parallel.

| # | Task | Crate | Deliverable |
|---|------|-------|-------------|
| E.1 | Translate `match.c` | `thttpd-match` | `lib.rs` + `#[cfg(test)]` unit tests, `cargo check` + `cargo clippy` + `cargo test` pass |
| E.2 | Translate `tdate_parse.c` | `thttpd-tdate` | `lib.rs` + unit tests, all checks pass |
| E.3 | Translate `fdwatch.c` → mio | `thttpd-fdwatch` | `lib.rs` wrapping mio, unit tests, all checks pass |
| E.4 | Write ADR: `002-mio-vs-epoll.md` | — | Decision record for I/O multiplexing choice |
| E.5 | Update `_migration_map.yaml` | — | 3 files → `status: compiled` |

**Translation prompt constraints** (from PLAN.md §3.3):
- Safe Rust only, `std` + `mio`
- 1:1 structural mapping, no async/await/tokio/hyper
- `Result<T, E>` with module-specific error enums
- Complete rustdoc on every `pub fn`
- Preserve undocumented behavior from YAML `gotchas:`
- No `unsafe` unless documented with safety comment

**Exit Gate:** All 3 crates pass `cargo check`, `cargo clippy`, `cargo test`. ADR written.

---

### GROUP F — Translation: Infrastructure Modules

**Modules:** `timers.c` → `thttpd-timers`, `mmc.c` → `thttpd-mmc`
**Blocked by:** Group B (analysis) + Group E (fdwatch must exist first, timers depends on it)

| # | Task | Crate | Deliverable |
|---|------|-------|-------------|
| F.1 | Translate `timers.c` | `thttpd-timers` | `lib.rs` + unit tests, depends on `thttpd-fdwatch` for event loop integration |
| F.2 | Translate `mmc.c` | `thttpd-mmc` | `lib.rs` + unit tests, depends on `thttpd-timers` for cache expiry |
| F.3 | Update `_migration_map.yaml` | — | 2 files → `status: compiled` |

**Exit Gate:** Both crates pass all checks. `thttpd-mmc` correctly depends on `thttpd-timers`.

---

### GROUP G — Translation: Core Modules

**Modules:** `libhttpd.c` → `thttpd-http`, `thttpd.c` → `thttpd-core`
**Blocked by:** Group C (analysis) + Groups E+F (all dependencies must exist)

This is the largest translation effort. `libhttpd.c` alone is 4,230 lines. Split into sub-tasks.

| # | Task | Crate | Deliverable |
|---|------|-------|-------------|
| G.1 | Write ADR: `001-crate-boundaries.md` | — | Decision record for module decomposition |
| G.2 | Write ADR: `003-error-handling-strategy.md` | — | Decision record for error type hierarchy |
| G.3 | Translate libhttpd: HTTP parsing | `thttpd-http` | `parse/` module — request line, headers, query string |
| G.4 | Translate libhttpd: Response building | `thttpd-http` | `response/` module — status codes, headers, body |
| G.5 | Translate libhttpd: CGI execution | `thttpd-http` | `cgi/` module — `Command` fork/exec, env vars, stdin/stdout |
| G.6 | Translate libhttpd: File serving | `thttpd-http` | `files/` module — integrates with `thttpd-mmc` |
| G.7 | `cargo check` + `cargo test` on `thttpd-http` | `thttpd-http` | Full crate compiles and passes tests |
| G.8 | Translate `thttpd.c`: Main loop | `thttpd-core` | `main.rs` — server startup, connection acceptance, event loop |
| G.9 | Translate `thttpd.c`: Connection management | `thttpd-core` | `connection/` module — per-connection state machine |
| G.10 | Translate `thttpd.c`: Signal handling | `thttpd-core` | `signals/` module — `signal-hook` crate for SIGTERM/SIGHUP/SIGUSR1 |
| G.11 | Translate `thttpd.c`: Security (chroot/setuid) | `thttpd-core` | `security/` module — `nix` crate for chroot/setuid/setgid |
| G.12 | Translate `thttpd.c`: Throttling | `thttpd-core` | `throttle/` module — bandwidth rate limiting |
| G.13 | Translate `thttpd.c`: Config/CLI parsing | `thttpd-core` | `config/` module — command-line argument parsing |
| G.14 | Integration: `main.rs` wiring | `thttpd-core` | Wire all modules together, `cargo build` produces working binary |
| G.15 | Update `_migration_map.yaml` | — | `libhttpd.c`, `thttpd.c` → `status: compiled` |

**Exit Gate:** `cargo build --workspace` succeeds. Binary starts and accepts connections. All unit tests pass.

---

## Phase 4 + 5: Verification & Modernization (Per-Crate Groups)

**Blocked by:** Phase 3 (translation complete) + Phase 2 (golden master captured)

### GROUP H — Verify & Modernize (per crate, parallelizable)

After the full workspace compiles, each crate gets a verify-then-modernize pass. These can run in parallel across crates since they touch different code.

#### H-verify (run first per crate)

For each crate, run the differential test suite:

| # | Task | Deliverable |
|---|------|-------------|
| H.v1 | Run `pipeline/run_differential.py` against Rust binary | `harness/golden/diff_report.json` |
| H.v2 | For each mismatch: classify, trace to Rust function, feed to repair agent | Patched Rust code |
| H.v3 | Re-run differential tests after repairs | Updated `diff_report.json` |
| H.v4 | Repeat repair loop (max 5 cycles per mismatch) | 100% pass rate or escalated issues |
| H.v5 | Log all repairs in `knowledge/learnings/` | Dated learning files |
| H.v6 | Update `_migration_map.yaml` | All files → `status: verified` |

#### H-modernize (run after H-verify passes 100%)

For each crate, in parallel:

| # | Task | Crate | Deliverable |
|---|------|-------|-------------|
| H.m1 | Modernize `thttpd-match` | `thttpd-match` | Named constants, `thiserror`, `#[must_use]`, complete rustdoc |
| H.m2 | Modernize `thttpd-tdate` | `thttpd-tdate` | Same pass |
| H.m3 | Modernize `thttpd-fdwatch` | `thttpd-fdwatch` | Same pass |
| H.m4 | Modernize `thttpd-timers` | `thttpd-timers` | Same pass |
| H.m5 | Modernize `thttpd-mmc` | `thttpd-mmc` | Same pass |
| H.m6 | Modernize `thttpd-http` | `thttpd-http` | Same pass |
| H.m7 | Modernize `thttpd-core` | `thttpd-core` | Same pass |
| H.m8 | Run `cargo doc --no-deps` | workspace | Clean HTML documentation |
| H.m9 | Run `cargo clippy -- -W clippy::pedantic` | workspace | Zero warnings |
| H.m10 | Write `knowledge/concepts/migration_summary.md` | — | Lessons learned |
| H.m11 | Update `_migration_map.yaml` | — | All files → `status: modernized` |

**Modernization constraints** (from PLAN.md §5.1):
- Do NOT alter execution logic
- Replace magic numbers with named constants/enums
- Derive `thiserror::Error` for all error types
- Replace `unsafe` with safe alternatives where possible
- Add `#[must_use]` where appropriate
- Complete rustdoc with `# Examples` on every `pub fn`

**Exit Gate:** Differential tests pass 100%. `cargo doc` clean. `cargo clippy --pedantic` zero warnings. All modules `status: modernized`.

---

## Dependency Graph & Execution Timeline

```
Time ──────────────────────────────────────────────────────────────►

Phase 0          Phase 1              Phase 2         Phase 3              Phase 4+5
(Main)           (Parallel)           (Overlap w/1)   (Dep-batched)        (Per-crate)

MAIN ──────┐
           ├──► GROUP A (match, tdate) ──────┐
           ├──► GROUP B (fdwatch, timers, ───┤
           │         mmc)                    │
           ├──► GROUP C (libhttpd, thttpd) ──┤
           │                                 │
           └──► GROUP D1 (harness infra) ────┤
                    │                        │
                    └──► GROUP D2 (tests, ───┤
                              parallel)      │
                                             │
                    ┌────────────────────────┘
                    ▼
               GROUP E (leaf translation) ──┐
                    │                       │
                    ▼                       │
               GROUP F (infra translation) ─┤
                    │                       │
                    ▼                       │
               GROUP G (core translation) ──┤
                                            │
                    ┌───────────────────────┘
                    ▼
               GROUP H (verify + modernize, per-crate parallel)
```

### Parallel Execution Summary

| When | Running in Parallel |
|------|-------------------|
| **Phase 1** | Group A ‖ Group B ‖ Group C |
| **Phase 1 + 2** | Groups A,B,C ‖ Group D1 → D2 |
| **Phase 3 Batch 1** | Group E (3 crates parallel) |
| **Phase 3 Batch 2** | Group F (2 crates sequential) |
| **Phase 3 Batch 3** | Group G (14 sub-tasks, some parallel) |
| **Phase 4+5** | Group H (7 crates parallel for modernization) |

---

## Subagent Execution Template

When you're ready to execute, each group can be launched as a subagent with this pattern:

```
# Example: Launch Group A (leaf module analysis)
subagent task:
  "You are Group A in the thttpd→Rust migration (see EXECUTION_PLAN.md).
   Your job is to analyze match.c and tdate_parse.c.
   
   1. Run pipeline/analyze_module.py on each file
   2. Read the full C source, enrich the YAML with complete analysis
   3. Write knowledge/modules/{name}.md for each
   4. Update _migration_map.yaml
   
   Exit gate: Both .yaml files have status: analyzed, all functions documented."
```

---

## Risk Mitigation

| Risk | Mitigation |
|------|-----------|
| Group C (libhttpd analysis) takes too long | Split into two sub-agents: C1 (libhttpd) and C2 (thttpd) |
| Golden Master capture reveals flaky tests | D2 agents fix tests, re-capture baseline |
| Translation repair loop hits 5-cycle limit | Escalate to human, log in `knowledge/learnings/`, continue with other crates |
| Groups finish at different speeds | Faster groups can assist slower ones (e.g., Group A helps Group C after finishing) |
| `libhttpd.c` translation is too large for one agent | Group G splits into 7 sub-tasks (G.3–G.9), each can be a separate subagent |

---

## CI Pipeline (Created in Phase 0, Updated Per Phase)

```yaml
# .github/workflows/migration-ci.yml — added incrementally
jobs:
  validate-knowledge:    # Phase 0
  build-legacy:          # Phase 0
  build-rust:            # Phase 3
  unit-tests:            # Phase 3
  golden-master:         # Phase 2 (re-run on every commit)
  differential-tests:    # Phase 4
  docs-check:            # Phase 5
```

---

## Summary

| Group | Phase | Modules | Parallel With | Blocked By |
|-------|-------|---------|---------------|------------|
| **Main** | 0 | All scaffolding | — | — |
| **A** | 1 | match, tdate_parse | B, C | Main |
| **B** | 1 | fdwatch, timers, mmc | A, C | Main |
| **C** | 1 | libhttpd, thttpd | A, B | Main |
| **D1** | 2 | Harness infra | A, B, C | Main |
| **D2** | 2 | Test suites (8 parallel) | — | D1 |
| **E** | 3 | match→Rust, tdate→Rust, fdwatch→Rust | — | A |
| **F** | 3 | timers→Rust, mmc→Rust | — | B, E |
| **G** | 3 | libhttpd→Rust, thttpd→Rust | — | C, E, F |
| **H** | 4+5 | Verify + modernize all crates | per-crate | D2, G |
