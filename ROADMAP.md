# thttpd-rs Migration Roadmap

> Migration artifact status for thttpd-rs: what has shipped, what remains, and
> how the remaining additions round out the production story.
> This document is the answer to the question every interviewer hiring for a
> stack migration will eventually ask: *"How would you actually do this?"*

## What's already strong

Before listing what's missing, what's here is worth naming — this is the
foundation everything below builds on:

- **Behavior-preserving Rust port of thttpd**, split across 10 crates with a single-threaded
  `mio` event loop preserving the original C architecture
- **Differential testing** — 105 differential scenarios that compare the C and Rust
  binaries field-by-field (status, headers, body, lifecycle). All pass.
- **Knowledge graph** under `knowledge/` — every C file mapped to its Rust
  equivalent with `file:line` evidence
- **CI** — build + unit tests + harness + differential + knowledge validation
- **Strangler-fig proxy** — `thttpd-migrate` ships active-active/canary routing,
  shadow diffing, active health checks, a circuit breaker, request IDs,
  Prometheus `/metrics`, a control socket, rollback, and drain
- **Proxy integration gate** — 31 tests cover proxy routing, shadow mode,
  health, circuit breaker, rollback, metrics, and drain behavior
- **JOURNEY.md** — the migration case study (the doc that proves behavioral
  gates, not structural ones, were used)

This foundation is genuinely unusual. Most "Rust port" projects stop at "it
compiles and the unit tests pass." The differential suite compares request
behavior under an explicit normalization policy instead of settling for
"mostly correct." That is the move that separates a port from a rewrite.

## How the additions are prioritized

The additions below are grouped by the question they answer in an interview
context. The migration proxy, proxy observability, and rollback runbook are now
shipped; the remaining work starts from that baseline.

| Tier | Interview question it answers | Time to ship |
|------|-------------------------------|--------------|
| **S** | "How do you actually migrate a running system without downtime?" | 1-2 days each |
| **A** | "How do you know the new system is *better*, not just different?" | half-day to 2 days each |
| **B** | "What about polish — docs, tests, release discipline?" | afternoon each |

Build order: maintain the shipped Tier S controls, then add the remaining Tier
S production artifacts before Tier A. Tier B items are independent and can ship
whenever.

---

## Shipped Migration Controls

These pieces now answer the "how do you migrate a running system?" question
directly and are covered by the repository's automated gates.

### 1. Strangler-fig migration proxy — implemented

**What:** a new `thttpd-migrate` binary that sits in front of both servers
and lets you shift traffic C → Rust in any ratio, including 0% (shadow only).
Hot weight adjustment, circuit breaker on the Rust backend, graceful drain,
one-command rollback.

**Why it matters:** this is *the* canonical migration pattern. The first
follow-up question after "I ported it" is "how do you switch over?" The
honest answer is "incrementally, with the ability to revert quickly" —
and this proxy is the artifact that proves you can do it.

**Status:** implemented in `rust/crates/thttpd-migrate`, documented in
`docs/MIGRATION.md`, and covered by 31 proxy integration tests.

### 2. Observability — `tracing` + `/metrics` — implemented

**What:** structured logging, request-id propagation, and Prometheus metrics
for requests, durations, backend 5xx responses, and shadow divergences.

**Status:** implemented in the proxy. The exporter serves `/metrics`; the
configured `metrics.path` is currently advisory.

### 3. Rollback runbook — implemented

**What:** one-command rollback via the control socket plus a documented runbook.
Rollback updates routing immediately for new selections while in-flight
requests continue normally.

**Status:** implemented as `rollback --to ...` and documented in
`docs/MIGRATION.md` and `docs/CONTROL_PROTOCOL.md`.

## Tier S — remaining production migration work

The proxy/runbook/observability core is shipped. These are the highest-value
remaining productionization items.

### 4. Old `thttpd.conf` compatibility shim

**What:** complete the new legacy `thttpd.conf` parser. Supported directives
already load directly; unsupported directives fail with actionable errors.
Finish the remaining data-dir and symlink surfaces and add a migration report.

**Why it matters:** zero-effort adoption is the difference between a
migration that ships and one that dies in committee. Operators have years of
muscle memory on the old config syntax. Making them learn a new one is a
tax the migration doesn't need to levy.

**Effort:** ~half a day, plus configuration-option coverage analysis.

### 5. Criterion benchmarks — C vs Rust

**What:** `rust/benches/` with `criterion` benches for the hot paths (HTTP
parse, mmap cache lookup, glob match, timer tick, throttle calc), plus a
small C harness binary built from the same code paths. Output SVG charts
checked into `docs/bench/`.

**Why it matters:** Add "p99 latency, RSS, requests/sec, binary size" so
empirical results replace rhetorical size claims. This is the slide that
ends the "but is Rust actually faster?" debate in the room.

**Effort:** 1 day including the C harness.

### 6. Production deployment artifacts

**What:** multi-stage `Dockerfile` (cargo-chef for caching, distroless/scratch
final, non-root, read-only fs), `docker-compose.yml` for local dev, Helm
chart or k8s manifests with `livenessProbe` / `readinessProbe` / `startupProbe`
/ HPA, SBOM generation step in CI (syft or `cargo-auditable`).

**Why it matters:** hiring managers mentally check "could I deploy this
Monday?" — these are the artifacts that flip that to yes. Without them the
port is a curiosity, not a candidate.

**Effort:** 1-2 days.

---

## Tier A — strong differentiators

These prove the new system is *better* than the old one, not just *different*.

### 7. Security comparison report + CI — implemented

**What:** a public `docs/security/MIGRATION_REPORT.md` that maps every
historical CVE in `sthttpd` / `thttpd` to its CWE class and to the Rust
mechanism (with `file:line` evidence) that prevents it. Backed by four new
CI jobs: `cargo-audit`, `cargo-deny`, `cargo-geiger`, Miri, ASan, fuzz.

**Why it matters:** the "Rust is safer" claim is universal. The version with
receipts — "12 CVEs were filed against thttpd, 9 of those classes are
structurally impossible in our code, the remaining 3 are caught by Miri in
CI" — is not.

**Status:** implemented. The report lives in
`docs/security/MIGRATION_REPORT.md`; local checks are wired through
`make security`; CI jobs cover supply-chain policy, unsafe audit, Miri, ASan,
fuzz smoke tests, and release SBOM generation.

### 8. `cargo-fuzz` harness on the HTTP parser — implemented

**What:** `fuzz/fuzz_targets/parse_request.rs` (and `parse_url.rs`,
`parse_header.rs`). Differential tests prove C and Rust agree on *known*
inputs; fuzzing proves Rust is robust against inputs the C never saw. The
current CI job runs bounded nightly smoke fuzzing; longer fuzz campaigns remain
a local/manual activity.

**Why it matters:** fuzzing is one of the few ways to find security-relevant
bugs that the type system doesn't catch. Hiring managers who have been
through CVE postmortems recognize it.

**Status:** implemented for `parse_request` and `parse_url` under
`rust/fuzz/`. The GitHub Actions fuzz job runs both targets as bounded nightly
smoke tests and can be dispatched manually. A `parse_header` target remains a
possible incremental addition.

### 9. Architecture Decision Records

**What:** `knowledge/decisions/` with MADR-format ADRs: why `mio` not
`tokio`, why preserve the single-threaded event loop, why behavior-first parity
not idiomatic rewrite, why `Rc<Mmap>` not `Arc<Mmap>`, why we ported
`diff_engine.py` to Rust in the proxy but not in the server, etc.

**Why it matters:** ADRs demonstrate engineering judgment — the *why* of
decisions, not just the *what*. Senior engineers read these first when
evaluating a codebase. Pairing an ADR with the existing `knowledge/`
structure is a natural fit.

**Effort:** half a day to bootstrap the format; one hour per ADR.

### 10. Load tests + slow-loris chaos

**What:** `loadtest/` with k6 (or `wrk` + Lua) scripts for steady-state
throughput, spike, and slow-loris. Baseline numbers checked into git; a CI
step that fails PRs regressing throughput by >5%.

**Why it matters:** "diff-against-baseline" is the pattern that catches perf
regressions that escape code review. Combined with the chaos loop (kill C
and Rust alternately), it's also the artifact that proves the proxy's
circuit breaker works.

**Effort:** 1 day.

---

## Tier B — polish and signal

These round out the picture. None is load-bearing for the migration story;
each is a small win.

### 11. MkDocs migration site

A single URL public docs site: the journey, the ADR index, the benchmark
dashboard, the security report, the runbooks. Hand it to an interviewer;
hand it to a downstream user. Depends on items 6, 8, 9 being done first.

### 12. `cargo-mutants` CI step

`cargo-mutants` proves your tests would actually catch a regression. It
surfaces gaps the differential tests hide (asserts that always pass, dead
branches). Add to CI on a weekly schedule; failures get a triage ticket.

### 13. `proptest` property-based tests

Add `proptest` for the pure functions: `thttpd-match` glob matcher,
`thttpd-tdate` date parser, `thttpd-http` URL canonicalization. Low-effort,
high-signal modern-Rust pattern. Each one is ~2 hours.

### 14. Per-crate `CHANGELOG.md`

`Keep-a-Changelog` format, one per crate. Useful for downstream users;
demonstrates release discipline; pairs well with the ADRs (changelog
entries cite the ADR for non-obvious changes).

### 15. Migrated man pages / mdbook user guide

Convert thttpd's man page into an `mdbook` user guide with C→Rust command
mapping tables. Operator-facing documentation that closes the "I can't
tell what's different" gap.

---

## Recommended build order

The order below minimizes blocking dependencies while maximizing interview
signal. Each item is shippable independently — no phase gates the next
unless noted.

| # | Item | Depends on | Why this order |
|---|------|-----------|----------------|
| 1 | **ADRs** (#9) | — | Foundational. Informs every other plan. Half a day. |
| 2 | **Strangler-fig proxy** (#1) | ADR on `mio`/`tokio` split | Shipped. Keep it in the demo path and maintain the proxy integration gate. |
| 3 | **Security report + CI** (#7) | — | Shipped. Keep it current as security-relevant implementation changes land. |
| 4 | **Criterion benchmarks** (#5) | — | Empirical data backs every future claim. The "4× more compact" line becomes "p99 0.4ms vs 0.6ms at 1k req/s." |
| 5 | **Observability** (#2) | — | Shipped for the proxy; extend with any server-specific metrics as needed. |
| 6 | **Container + k8s** (#6) | #2 (observability) | Deployable Monday. |
| 7 | **Config shim** (#4) | — | Independent. |
| 8 | **Rollback runbook** (#3) | #2 (strangler proxy CLI) | Shipped with the proxy control plane. |
| 9 | **cargo-fuzz** (#8) | — | Shipped for parser and URL targets. Add narrower targets only when they expose a distinct risk. |
| 10 | **Load tests** (#10) | #1 (strangler proxy) | Proves the proxy works under load. |
| 11 | **proptest** (#13) | — | Quick wins on pure functions. |
| 12 | **cargo-mutants** (#12) | — | Independent. |
| 13 | **CHANGELOG** (#14) | #8 (ADRs) | Uses ADR language in entries. |
| 14 | **MkDocs site** (#11) | #6, #8, #9 | Assembles the existing artifacts into one URL. |
| 15 | **Man pages / mdbook** (#15) | #2, #1 (config shim, proxy CLI) | Documents the new CLIs. |

The first 5 items are the "minimum viable interview story." Each one is a
concrete story the interviewer will remember after the conversation.

---

## Status

| # | Item | Status | Plan |
|---|------|--------|------|
| 1 | Strangler-fig proxy | implemented | [docs](docs/MIGRATION.md) |
| 2 | Proxy observability | implemented | `/metrics`, tracing, request IDs |
| 3 | Rollback runbook | implemented | [docs](docs/MIGRATION.md) |
| 4 | Config shim | implemented | legacy argv matrix + `data_dir`/`symlink` directives |
| 5 | Criterion benchmarks | todo | — |
| 6 | Container + k8s | todo | — |
| 7 | Security report + CI | implemented | [docs](docs/security/MIGRATION_REPORT.md) |
| 8 | cargo-fuzz | implemented | [fuzz](rust/fuzz) |
| 9 | ADRs | accepted | [docs](docs/ADR-0002-async-runtime-split.md) |
| 10 | Load tests | todo | — |
| 11 | MkDocs site | todo | — |
| 12 | cargo-mutants | todo | — |
| 13 | proptest | todo | — |
| 14 | CHANGELOG | todo | — |
| 15 | Man pages | todo | — |
| — | Runtime parity | implemented | throttling, daemonization, request logging, IPv6 dual-stack |

Last updated: 2026-06-23.
