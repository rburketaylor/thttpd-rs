---
date: 2026-06-08T15:18:22-0300
author: Burke T
commit: no-commit
branch: no-branch
repository: thttpd-rs
topic: "thttpd Rust migration"
tags: [intent, frd, migration, c-to-rust, thttpd, portfolio]
status: complete
last_updated: 2026-06-08T15:18:22-0300
last_updated_by: Burke T
---

# FRD: sthttpd (thttpd) C → Rust Migration

## Summary

A portfolio-showcase migration of sthttpd (thttpd) 2.27.0 — an ~8,600-line C HTTP server — to safe Rust, using a 6-phase gated pipeline with golden-master characterization testing, byte-exact differential verification, a schema-enforced YAML+MD knowledge system, and a modernization pass. The deliverable is a drop-in replacement binary that accepts identical CLI flags, produces identical HTTP responses, and contains zero `unsafe` blocks.

## Problem & Intent

The developer is building a demonstrably rigorous migration artifact to showcase engineering capability. The motivation is portfolio/showcase — quality of evidence matters more than speed. The artifact must convince a technical reviewer that:

1. The methodology is sound (gated phases, golden master, differential testing)
2. The testing is airtight (byte-exact parity proof, ≥200 test cases, CI enforcement)
3. The Rust output is genuinely equivalent to the C original (not "looks similar" but provably identical in behavior)

The developer already has extensive planning documents (PLAN.md, EXECUTION_PLAN.md, migration_path.md) that define a 6-phase pipeline. This FRD captures the intent, scope decisions, and acceptance criteria that those plans operationalize.

## Goals

- Achieve byte-exact behavioral parity with the C binary: identical status codes, status text, header order, body SHA-256, and connection behavior for ≥200 test cases
- Produce zero `unsafe` Rust code — use well-maintained safe wrappers (mio, nix, signal-hook) for all system-level operations
- Full CLI flag compatibility — the Rust binary is a drop-in replacement for the C `thttpd` binary
- Build a structured YAML+MD knowledge system with schema enforcement and CI validation, documenting every module's analysis, cross-cutting concerns, and migration decisions
- Golden master harness (Python/pytest) that captures C binary behavior and proves Rust parity via differential testing
- Modernization pass producing idiomatic Rust with complete rustdoc, named constants, `thiserror` error types, and zero clippy pedantic warnings
- CI pipeline (GitHub Actions) that enforces the regression guard on every push/PR
- Demonstrate AI-assisted migration methodology with subagent-driven parallel execution

## Non-Goals

- Cross-platform support — Linux-only; no macOS (kqueue) or Windows backends needed
- Translation of `extras/` utilities (htpasswd, makeweb, syslogtocern) — out of scope for initial migration
- Translation of `www/cgi-bin/` programs (phf.c, redirect.c, ssi.c) — only the CGI *execution mechanism* is in scope, not the CGI programs themselves
- Async/await, tokio, or hyper — the port is a synchronous, `select()`/`poll()`-style server using mio
- Performance improvements over the C binary — parity is the goal, not optimization
- Re-architecting the server — maintain 1:1 structural mapping to the C original through translation and modernization

## Functional Requirements

1. The system SHALL compile the `legacy/` C source into a working `thttpd` binary via `./configure && make`
2. The system SHALL initialize a Rust workspace with 8 crates: `thttpd-core`, `thttpd-http`, `thttpd-fdwatch`, `thttpd-timers`, `thttpd-mmc`, `thttpd-match`, `thttpd-tdate`, `thttpd-mime`
3. The system SHALL populate `knowledge/modules/*.yaml` with function signatures, callers, callees, globals, gotchas, and complexity ratings for every C source module
4. The system SHALL populate `knowledge/modules/*.md` with human-readable module guides for every C source module
5. The system SHALL populate `knowledge/concepts/*.md` with cross-cutting concern docs (HTTP protocol, connection lifecycle, CGI model, throttling, signal handling, security model, memory-mapped cache)
6. The system SHALL capture ≥200 golden master test cases from the C binary covering: static file serving, HTTP methods, header parsing, CGI execution, malformed input, connection behavior, error responses, throttling, and edge cases
7. The system SHALL record golden master captures as JSON with: request details, response status code, status text, header order, header values, body SHA-256, body byte count, and connection result
8. The system SHALL translate all 7 C modules to Rust in dependency order: leaf modules (match, tdate_parse, fdwatch) → infrastructure (timers, mmc) → core libraries (libhttpd) → main executable (thttpd)
9. The Rust binary SHALL support the same CGI/1.1 features as the C binary: script execution, environment variable passing, POST body piping, and NPH (no-parse-header) scripts
10. The Rust binary SHALL accept the same command-line flags as the C `thttpd` binary: `-p`, `-d`, `-r`, `-u`, `-l`, `-T`, and all other flags
11. The Rust binary SHALL handle SIGTERM, SIGHUP, and SIGUSR1 identically to the C binary
12. The Rust binary SHALL implement bandwidth throttling matching the C binary's behavior
13. The system SHALL run differential testing comparing Rust binary responses against golden master baseline, with strict comparison of status code, status text, header order, header values, body SHA-256, and connection result
14. The system SHALL implement an automated repair loop (max 5 cycles per mismatch) that feeds diff failures back for correction
15. The system SHALL apply a modernization pass to all crates: replace magic numbers with named constants, derive `thiserror::Error` for all error types, add `#[must_use]` where appropriate, and ensure complete rustdoc with `# Examples` on every `pub fn`
16. The system SHALL pass `cargo clippy -- -W clippy::pedantic` with zero warnings after modernization
17. The system SHALL pass `cargo doc --no-deps` generating clean HTML documentation

## Non-Functional Requirements

- **Performance**: No specific latency/throughput requirement beyond matching C binary behavior under the same test conditions. Parity is the constraint, not improvement.
- **Security**: Preserve all security features from the C original: chroot, setuid/setgid, symlink checks, URL path sanitization. No `unsafe` blocks in the Rust codebase — all system-level operations go through safe wrappers.
- **UX / Accessibility**: Full CLI flag compatibility — a user can swap the C binary for the Rust binary with zero configuration changes.
- **Reliability**: Byte-exact parity proven by differential testing. CI regression guard ensures parity never drifts. Repair loop with 5-cycle limit and human escalation path.

## Constraints & Assumptions

- **Platform**: Linux-only. No cross-platform abstractions needed for kqueue/devpoll.
- **Rust edition**: 2024, stable channel, pinned via `rust-toolchain.toml`
- **No async runtime**: Synchronous server architecture using mio for I/O multiplexing. No tokio, async-std, or async/await.
- **Dependencies**: mio (I/O multiplexing), thiserror (error types), signal-hook (signal handling), nix (chroot/setuid), clap (CLI parsing). Standard, well-maintained crates only.
- **Source version**: sthttpd 2.27.0 from `blueness/sthttpd` (commit 2845bf5), stored as a plain git clone in `legacy/`
- **Test harness**: Python 3 with pytest. Golden master captures stored as JSON.
- **CI**: GitHub Actions with jobs for build-legacy, build-rust, unit-tests, differential-tests, and knowledge-consistency
- **Assumption**: The C binary's undocumented behaviors (captured in golden master) are the authoritative spec, not the man pages or source comments
- **Assumption**: A reviewer will evaluate the migration by examining the pipeline artifacts, not just the final Rust code

## Acceptance Criteria

- [ ] `legacy/` builds with `./configure && make`, producing a working `thttpd` binary
- [ ] `cargo build --workspace` succeeds with zero errors on Rust 2024 edition
- [ ] `cargo test --workspace` passes all unit tests
- [ ] `cargo clippy --workspace -- -W clippy::pedantic` produces zero warnings
- [ ] `cargo doc --no-deps` generates clean HTML documentation for all 8 crates
- [ ] `grep -r "unsafe" rust/crates/` returns zero matches — no `unsafe` blocks exist
- [ ] Running the Rust binary with `-p 8080 -d ./www` serves static files identically to the C binary
- [ ] The Rust binary accepts all CLI flags that the C binary accepts, producing identical behavior
- [ ] `harness/golden/baseline.json` contains ≥200 test cases covering all categories (static files, CGI, malformed input, errors, throttling, connection behavior)
- [ ] Running golden master capture twice produces identical JSON (reproducibility)
- [ ] `pipeline/run_differential.py --strict` exits 0 — 100% parity across all test cases
- [ ] Diff report shows body SHA-256 matches, header order matches, status text matches for every response
- [ ] CGI execution works: scripts receive correct environment variables, POST body is piped correctly, NPH scripts bypass response parsing
- [ ] `knowledge/_index.yaml` lists all 7 modules with correct status
- [ ] `knowledge/_migration_map.yaml` shows all modules with `status: modernized`
- [ ] `knowledge/modules/*.yaml` files exist for all 7 modules with complete function signatures and analysis
- [ ] `knowledge/modules/*.md` files exist for all 7 modules with prose documentation
- [ ] `knowledge/concepts/*.md` files exist for all 7 cross-cutting concerns
- [ ] `knowledge/decisions/*.md` ADR files exist for crate boundaries, mio choice, and error handling strategy
- [ ] `pipeline/validate_knowledge.py` runs clean on all YAML files
- [ ] GitHub Actions CI pipeline is green on all jobs: build-legacy, build-rust, unit-tests, differential-tests, knowledge-consistency

## Recommended Approach

A 6-phase gated pipeline executed by coordinated subagent groups: Phase 0 (repo scaffolding, knowledge system, build scripts) → Phase 1 (parallel deep analysis of all 7 C modules, populating YAML+MD knowledge artifacts) → Phase 2 (golden master harness capturing ≥200 C binary behavior snapshots) → Phase 3 (dependency-ordered C→Rust translation in 4 batches with compile gates) → Phase 4 (differential testing with automated repair loops proving byte-exact parity) → Phase 5 (idiomatic Rust modernization pass, documentation finalization, CI hardening). Each phase has explicit exit gates and blocks the next phase. The Rust workspace is structured as 8 crates mirroring the C module decomposition, using mio for I/O multiplexing and zero `unsafe` code throughout.

## Decisions

### Portfolio showcase intent
**Question**: What's the driving motivation behind migrating thttpd to Rust?
**Recommended**: n/a — `intent` question
**Chosen**: Portfolio / showcase — building a demonstrably rigorous, well-documented migration artifact to showcase engineering capability
**Rationale**: The developer's framing. Quality of evidence and rigor matter more than speed.

### Definition of done
**Question**: Your plan calls for byte-exact parity, structured knowledge system, golden master harness, differential testing, and full modernization. Which definition of "done" fits the showcase?
**Recommended**: Full pipeline as planned
**Chosen**: Full pipeline as planned — byte-exact parity, full knowledge system, golden master, differential testing, modernization pass
**Rationale**: Highest bar produces the most compelling evidence for a reviewer. Every phase contributes to the showcase.

### Platform target
**Question**: What platform(s) must the Rust binary support?
**Recommended**: Linux-only
**Chosen**: Linux-only
**Rationale**: Matches thttpd's primary deployment. Eliminates kqueue/devpoll complexity in fdwatch translation. mio on Linux (epoll) is sufficient.

### Dependency philosophy
**Question**: Which dependency philosophy fits the showcase — standard crate selections, minimal dependencies, or custom?
**Recommended**: Standard crate selections (mio, thiserror, signal-hook, nix, clap)
**Chosen**: Standard crate selections
**Rationale**: Well-maintained, widely-recognized crates that a reviewer would see as sound, idiomatic choices. Zero-unsafe goal requires safe wrappers for system calls.

### Knowledge system scope
**Question**: Does the YAML+MD knowledge system with schema enforcement earn enough showcase value to justify the setup cost?
**Recommended**: Full YAML+MD system as planned
**Chosen**: Full YAML+MD system with schema enforcement and CI validation
**Rationale**: Distinctive — most migration projects don't have structured institutional knowledge. Sets the showcase apart and demonstrates methodology rigor.

### Rust edition
**Question**: Which Rust edition should the workspace target?
**Recommended**: Rust 2024 edition
**Chosen**: Rust 2024 edition, stable channel
**Rationale**: Latest edition demonstrates currency. Pinned via `rust-toolchain.toml` for reproducibility.

### CI strategy
**Question**: Should the repo include a working CI pipeline, or is local harness sufficient?
**Recommended**: GitHub Actions
**Chosen**: GitHub Actions with full pipeline: build-legacy, build-rust, unit-tests, differential-tests, knowledge-consistency
**Rationale**: CI enforcement proves the regression guard works, not just locally but reproducibly. A reviewer can see the pipeline is real.

### CGI support level
**Question**: How much CGI support needs to work in the Rust port?
**Recommended**: Full CGI/1.1 support including NPH scripts
**Chosen**: Full CGI support — CGI/1.1 including NPH scripts, environment variable passing, POST body piping, exact fork/exec behavior
**Rationale**: CGI is a significant part of thttpd's functionality. Byte-exact parity goal requires it. Uses `std::process::Command` for safe fork/exec.

### CLI flag compatibility
**Question**: Should the Rust binary accept the same command-line flags as the C thttpd binary?
**Recommended**: Full CLI compatibility
**Chosen**: Full CLI compatibility — identical flags and behavior, drop-in replacement
**Rationale**: Drop-in replacement is a strong portfolio point. A reviewer can verify by swapping binaries.

### Unsafe code policy
**Question**: What's the policy on `unsafe` Rust code given mmap, raw fds, fork/exec, and signal handling?
**Recommended**: Zero unsafe blocks
**Chosen**: Zero unsafe — no `unsafe` blocks in the entire workspace
**Rationale**: Strong showcase differentiator. Safe wrappers (nix, signal-hook, mio) exist for all needed operations. Proves Rust's safety guarantees aren't compromised.

### C source layout
**Question**: The full sthttpd source lives in `legacy/` as a plain git clone. Keep this layout?
**Chosen**: Keep `legacy/` as a plain git clone
**Rationale**: evidence: `legacy/.git/config` confirms plain clone of blueness/sthttpd. Confirmed in pre-resolution step.

### Extras and CGI programs scope
**Question**: Should `extras/` utilities (htpasswd, makeweb, syslogtocern) and `www/cgi-bin/` programs (phf, redirect, ssi) be in scope?
**Chosen**: Out of scope for initial migration, deferred
**Rationale**: These are auxiliary utilities, not part of the core HTTP server. The CGI execution mechanism is in scope, but the CGI programs themselves are not. Can be added as follow-up work.

### No work started
**Question**: No Rust code, harness, knowledge system, or pipeline scripts exist yet?
**Chosen**: Confirmed — the entire 6-phase pipeline is to be executed
**Rationale**: evidence: `find` returns zero `.rs`, `.py`, `Cargo.toml` files anywhere in the repo. Confirmed in pre-resolution step.

## Open Questions

- **Extras translation scope**: Whether `extras/` utilities (htpasswd, makeweb, syslogtocern) should be translated in a future iteration. The developer deferred this decision. The CGI programs in `www/cgi-bin/` (phf.c, redirect.c, ssi.c) are likewise deferred.

## Suggested Follow-ups

- `legacy/extras/htpasswd.c` — password file utility (120 lines) that could be a natural follow-up translation target
- `legacy/extras/makeweb.c` — web directory creation tool (small, self-contained)
- `legacy/www/cgi-bin/redirect.c` — URL redirect CGI program, exercises the CGI mechanism from the server side
- `legacy/www/cgi-bin/ssi.c` — server-side include processing, more complex CGI usage
- The `legacy/` directory is a plain git clone, not a submodule — consider converting to a git submodule or subtree for cleaner tracking of upstream changes
- `Screenshot_20260608_144555.png` at repo root — appears to be a screenshot; consider moving to `docs/` or `.rpiv/` for repo cleanliness

## References

- `PLAN.md` — 6-phase migration plan with knowledge system design, verification checklist, and CI pipeline specification
- `EXECUTION_PLAN.md` — Subagent execution plan with group structure (A–H), dependency graph, and parallel execution timeline
- `migration_path.md` — Original discussion with another agent about the migration approach (golden master, differential testing, modernization pass)
- `legacy/` — Pristine clone of blueness/sthttpd (sthttpd 2.27.0, commit 2845bf5)
