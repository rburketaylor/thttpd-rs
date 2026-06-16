---
date: 2026-06-14T13:54:25-0300
author: Burke Taylor
commit: 71ad8af
branch: main
repository: thttpd-rs
topic: "Validation of Strangler-Fig Migration Proxy (thttpd-migrate)"
status: ready
verdict: fail
parent: ".rpiv/artifacts/plans/2026-06-12_16-30-00_strangler-fig-proxy.md"
tags: [validation, migration, proxy, strangler-fig, canary, shadow, rollout, rollback]
last_updated: 2026-06-14T13:54:25-0300
---

## Validation Report: Strangler-Fig Migration Proxy (thttpd-migrate)

### Implementation Status

- ✓ Phase 1: Workspace + CLI skeleton — Fully implemented
- ✓ Phase 2: Config schema + backend registry — Fully implemented
- ✓ Phase 3: Request routing + active/active — Fully implemented
- ✓ Phase 4: Shadow mode + response diffing — Fully implemented
- ✓ Phase 5: Health checks + circuit breaker — Fully implemented (minor call-site deviation, see Findings)
- ⚠️ Phase 6: Observability (tracing + Prometheus /metrics) — Partial — see Findings (one named test criterion absent)
- ✓ Phase 7: Graceful drain + rollback — Fully implemented
- ✓ Phase 8: Integration tests — Fully implemented
- ✓ Phase 9: Documentation + runbooks — Fully implemented (ADR numbering gap, see Findings)

### Automated Verification Results

- ✓ Workspace build: `cargo build --manifest-path rust/Cargo.toml --workspace` — compiles clean, thttpd-migrate + all crates
- ✓ Crate clippy: `cargo clippy --manifest-path rust/Cargo.toml -p thttpd-migrate --all-targets -- -D warnings` — no warnings
- ✓ Workspace clippy (regression surface): `cargo clippy --workspace --all-targets -- -D warnings` — clean
- ✓ Formatting: `cargo fmt --all --check` — clean
- ✓ Unit tests: `cargo test -p thttpd-migrate` — 52 passed, 0 failed (0.22s)
- ✓ CLI help: `cargo run -p thttpd-migrate -- --help` — lists `start`, `status`, `set-weight`, `drain`, `rollback`
- ✓ Bad config: `cargo run -p thttpd-migrate -- start --config /nonexistent.toml` — exits 1 (non-zero)
- ✓ Security gate: `make security` — `advisories ok, bans ok, licenses ok, sources ok` (cargo-deny license pass validates Phase 1 C8 concern)
- ✓ Integration tests: `pytest harness/tests/test_proxy.py` — 30/30 passed (31.92s)
- ✓ Doc link check: `scripts/check_doc_links.py` — all 12 relative links resolve
- ✓ Shell snippets: `bash -n` over fenced blocks in ROLLBACK/STRANGLER_FIG/MIGRATION_PLAYBOOK/CONTROL_PROTOCOL — all valid
- ✓ No regressions detected — additive workspace member; existing crates compile and lint clean

### Code Review Findings

#### Matches Plan:

- `rust/Cargo.toml:12` — `crates/thttpd-migrate` correctly added as workspace member
- `rust/crates/thttpd-migrate/Cargo.toml:3-6` — uses `version/edition/license/rust-version.workspace = true` identical to peer crates (thttpd-core, thttpd-http)
- `rust/crates/thttpd-migrate/src/main.rs` — clap CLI with all 5 subcommands and global `--control-socket`
- `router.rs:24-29,57-62` — shadow mode unconditionally resolves `decision.backend` to `routing.primary_backend`; the shadow backend only appears in `decision.shadow`, never served to users (confirmed by `shadow_mode_always_picks_primary` test)
- `forwarder.rs:44-46,55,124-133` — absolute backend URI built, path+query preserved, hop-by-hop headers stripped; request body converted to `ProxyBody` via `Full::new(body).map_err(...).boxed()` (no `todo!`/panic in any body path — resolves B4/C7)
- `shadow.rs:57` — `dispatch_shadow` takes `RoutingDecision` **by value** and moves the owned copy into `tokio::spawn` (resolves B1); `read_with_cap` (shadow.rs:22-42) genuinely loops frames with a cap (not a stub)
- `diff.rs:153-171,206-258` — ports the harness/diff_engine.py normalizers: RFC 1123 timestamp format-only matching (`is_timestamp`), temp-path substitution (`/tmp/thttpd_golden_*`, `/tmp/thttpd_diff_*`, `/tmp/pytest-*` → canonical placeholders), CGI port + PWD normalization, SHA-256 body hashing
- `circuit.rs:62-95` + `server.rs:157,167` + `router.rs:47,75` — breaker records outcomes on every forward (`pool.record_outcome`) AND is consulted by the router (`pool.breaker_allows`); HalfOpen probe supported; semantic rollback (target=100, others=0) confirmed by test
- `control.rs:70-103,153-183,197-227` — real working control plane: `UnixListener` server + `UnixStream::connect` clients, 4-byte BE length-prefixed JSON framing, `ControlRequest`/`ControlResponse` serde enums (resolves C9 — not pseudocode)
- `state.rs:99-108` — `write_state_atomic` writes `.tmp` then `std::fs::rename` (no partial reads); arc-swap live config; drain flag via `AtomicBool`/SeqCst
- `lib.rs:29,37` + `tracing_setup.rs:9-24` — `tracing_setup::init` is actually wired into `init_tracing` and called from `start()` (resolves C4 — tracing is not a no-op stub)
- `server.rs:46-62,64-71,104-106` — request counter + duration histogram recorded around every forward; request-ID honored/generated inbound, forwarded to backend, echoed on response
- `harness/tests/test_proxy.py` — 30 tests across 6 classes, reusing `conftest.py` fixtures (`proxy`, `dual_thttpd_backends`, `find_free_port`, new `wait_for_port` helper); no references to non-existent helpers
- `Makefile:37-39,49` — `proxy` target mirrors `harness`/`differential` style; `integration: harness differential proxy`

#### Deviations from Plan:

- **`rust/crates/thttpd-migrate/src/metrics.rs` — Phase 6 criterion gap (requires action).** The plan's Phase 6 Automated Verification line `metrics::tests::duration_histogram_records_observation` (one request → at least one bucket) is marked `[x]`, but no test by that name exists anywhere in the crate (`grep` over the crate returns nothing). The functionality it verifies **is** implemented — `record_metrics` (server.rs:37) records `thttpd_migrate_request_duration_seconds` and is called on both the success (server.rs:158) and 502 (server.rs:168) paths — so this is a missing *test*, not missing behavior. Fix: add a unit/integration test asserting the histogram records an observation after a forwarded request.
- **`rust/crates/thttpd-migrate/src/drain.rs:1` — dead stub module (housekeeping).** File is a single comment (`// Phase N+ stub — filled in by a later phase.`) and is declared in `lib.rs`. Drain logic is fully implemented across `state.rs` (flag), `control.rs` (client + `ControlRequest::Drain`), and `server.rs` (`tokio::select!` on drain signal + JoinSet), so this module is dead but harmless. Fix: either delete the module + its `pub mod drain;` declaration, or move drain-related helpers here.
- **Phase 1 bad-config message drift (minor, acceptable).** Plan's Phase 1 criterion expected the message `config file not found: /nonexistent.toml`; the final impl (Phase 2's `config::load` → `std::fs::read_to_string`) emits the OS error `No such file or directory (os error 2)`. Exit code is correct (non-zero). This is the intended Phase-2 evolution (config became authoritative), so only the literal string differs — no action required, but the Phase 1 `[x]` should be read as "exit non-zero," which holds.
- **`server.rs` vs `forwarder.rs` breaker call site (minor, functionally equivalent).** Plan text said "after each forward, call `breaker.record(success)`" inside `forwarder.rs::forward()`. The implementation calls it from the server handler (`pool.record_outcome` at server.rs:157/167) instead. Outcome is identical — every forwarded request records exactly one outcome — so this is an acceptable placement choice, not a behavioral gap.

#### Pattern Conformance:

- ✓ Cargo.toml workspace inheritance matches peer crates (`version/edition/license/rust-version.workspace = true`)
- ✓ Inline `#[cfg(test)] mod tests` in every source file matches existing crate convention; `tempfile = { workspace = true }` dev-dep used
- ✓ Python harness fully conforms: `subprocess.Popen`, `find_free_port()`, `proc.terminate()/wait()/kill()` teardown identical to `server_process`; new `wait_for_port()` correctly added to `conftest.py`
- ✓ `tracing` used consistently across all migrate modules; `println!` reserved for CLI user output (`status`/`set_weight`) — intentional divergence from server crates (which use `eprintln!`), justified by ADR-0002
- ✓ Makefile `proxy`/`integration`/`verify` targets mirror existing target style
- Minor observation (acceptable variation): new runtime deps (`tokio`, `hyper`, `serde`, `anyhow`, etc.) use inline versions instead of being hoisted into `[workspace.dependencies]` in `rust/Cargo.toml`. Existing convention centralizes shared deps there. Non-blocking — the crate compiles and `cargo deny` passes — but hoisting would match the house style.

#### Potential Issues:

- `docs/ADR-0002-async-runtime-split.md` is the only ADR present; there is no `ADR-0001`, leaving a numbering gap. The plan explicitly specified "ADR-0002," so the implementer followed the plan faithfully — but a reader expects an ADR-0001 to precede it. Either rename to ADR-0001 or add a placeholder ADR-0001 (this is arguably a plan-level quirk; see follow-up footer).

### Manual Testing Required:

The plan carries extensive manual criteria (live dual-backend demos, load tests, chaos tests, operator tabletop exercises). All remain valid and unverified by this automated validation:

1. Active-active routing:
   - [ ] Start two `nc -l` backends on 8081/8082 with distinct bodies; proxy on 8080; 1000 curls land in the configured weight ratio
   - [ ] Kill the Rust backend mid-flight; subsequent requests all route to C
2. Shadow mode:
   - [ ] Configure shadow; curl 100×; `/var/log/thttpd-migrate` shows zero divergences on identical binaries
   - [ ] Introduce a known Rust divergence (e.g. wrong `Server` header); shadow log shows request_id + field + expected/actual; user response is the primary's
3. Health + circuit:
   - [ ] Start C (up) + Rust (down); within 5s logs report Rust `Unhealthy`; all traffic on C, zero proxy 5xx
   - [ ] Kill C under 50 req/s; circuit trips within window; traffic shifts to Rust; no client errors
4. Drain + rollback (operator):
   - [ ] `set-weight rust-thttpd=100 c-thttpd=0` → all traffic on Rust within 1s
   - [ ] `rollback --to c-thttpd` → all traffic on C within 1s
   - [ ] `drain --timeout 30` → in-flight finish, new connections fail, process exits ≤30s
5. Performance:
   - [ ] 1k req/s for 1h; `thttpd_migrate_request_duration_seconds` p99 (proxy) minus p99 (direct) < 1ms

### Recommendations:

- **Add the missing `metrics::tests::duration_histogram_records_observation` test** (Phase 6). The histogram is recorded in the hot path, so the test is a small addition asserting at least one observation lands after a forward. This is the one item that turns the verdict from fail → pass.
- **Remove or populate `src/drain.rs`** and its `pub mod drain;` declaration — it is a dead stub today.
- **Hoist shared runtime deps into `[workspace.dependencies]`** in `rust/Cargo.toml` to match the existing convention (low priority; `cargo deny` already passes).
- **Resolve the ADR-0001/0002 numbering gap** — either rename ADR-0002 → ADR-0001 or seed an ADR-0001 (consider `/skill:revise` if you want the plan's ADR numbering corrected).
- All manual criteria above remain to be exercised before production rollout; the automated surface (52 unit + 30 integration) is green.
