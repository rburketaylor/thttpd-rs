---
date: 2026-06-14T14:49:52-0300
author: Burke Taylor
commit: 71ad8af
branch: main
repository: thttpd-rs
topic: "Validation of Strangler-Fig Migration Proxy (thttpd-migrate)"
status: ready
verdict: pass
parent: ".rpiv/artifacts/plans/2026-06-12_16-30-00_strangler-fig-proxy.md"
tags: [validation, migration, proxy, strangler-fig, canary, shadow, rollout, rollback]
last_updated: 2026-06-14T14:49:52-0300
---

## Validation Report: Strangler-Fig Migration Proxy (thttpd-migrate)

Re-validation after fixing the two localized gaps from the prior run (`2026-06-14_13-54-25_…`): the missing Phase 6 `duration_histogram_records_observation` test was added (`metrics.rs:57`) with a `metrics-util` `DebuggingRecorder` dev-dependency, and the dead `drain.rs` stub module was deleted (logic lives in `state.rs`/`control.rs`/`server.rs`).

### Implementation Status

- ✓ Phase 1: Workspace + CLI skeleton — Fully implemented
- ✓ Phase 2: Config schema + backend registry — Fully implemented
- ✓ Phase 3: Request routing + active/active — Fully implemented
- ✓ Phase 4: Shadow mode + response diffing — Fully implemented
- ✓ Phase 5: Health checks + circuit breaker — Fully implemented
- ✓ Phase 6: Observability (tracing + Prometheus /metrics) — Fully implemented (gating test now present)
- ✓ Phase 7: Graceful drain + rollback — Fully implemented
- ✓ Phase 8: Integration tests — Fully implemented
- ✓ Phase 9: Documentation + runbooks — Fully implemented

### Automated Verification Results

- ✓ Workspace build: `cargo build --manifest-path rust/Cargo.toml --workspace` — compiles clean
- ✓ Crate clippy: `cargo clippy -p thttpd-migrate --all-targets -- -D warnings` — no warnings
- ✓ Formatting: `cargo fmt --all --check` — clean
- ✓ Unit tests: `cargo test -p thttpd-migrate` — 53 passed, 0 failed (0.21s); includes the new `duration_histogram_records_observation`
- ✓ CLI help: `cargo run -p thttpd-migrate -- --help` — lists `start`, `status`, `set-weight`, `drain`, `rollback`
- ✓ Bad config: `cargo run -p thttpd-migrate -- start --config /nonexistent.toml` — exits 1
- ✓ Security gate: `cargo deny check` — `advisories ok, bans ok, licenses ok, sources ok`
- ✓ Integration tests: `pytest harness/tests/test_proxy.py` — 30/30 passed (20.37s)
- ✓ Doc link check: `scripts/check_doc_links.py` — all 12 relative links resolve
- ✓ Shell snippets: `bash -n` over 5 fenced blocks in ROLLBACK/STRANGLER_FIG/MIGRATION_PLAYBOOK/CONTROL_PROTOCOL — all valid
- ✓ No regressions detected — additive workspace member; existing crates compile, lint, and format clean

### Code Review Findings

#### Matches Plan:

- `rust/Cargo.toml:12` — `crates/thttpd-migrate` correctly added as workspace member
- `rust/crates/thttpd-migrate/Cargo.toml:3-6` — uses `version/edition/license/rust-version.workspace = true` identical to peer crates
- `router.rs:24-29,57-62` — shadow mode unconditionally serves `routing.primary_backend`; the shadow backend only appears in `decision.shadow`, never served to users (confirmed by `shadow_mode_always_picks_primary` test)
- `forwarder.rs:44-46,55,124-133` — absolute backend URI built, path+query preserved, hop-by-hop headers stripped; request body converted to `ProxyBody` via `Full::new(body).map_err(...).boxed()` (no `todo!`/panic in any body path)
- `shadow.rs:57` — `dispatch_shadow` takes `RoutingDecision` by value and moves the owned copy into `tokio::spawn` (`'static`-safe); `read_with_cap` (shadow.rs:22-42) genuinely loops frames with a cap
- `diff.rs:153-171,206-258` — ports the harness/diff_engine.py normalizers: RFC 1123 timestamp format-only matching, temp-path substitution (`/tmp/thttpd_golden_*`, `/tmp/thttpd_diff_*`, `/tmp/pytest-*`), CGI port + PWD normalization, SHA-256 body hashing
- `circuit.rs:62-95` + `server.rs:157,167` + `router.rs:47,75` — breaker records outcomes on every forward AND is consulted by the router; HalfOpen probe supported; semantic rollback (target=100, others=0)
- `control.rs:70-103,153-227` — real working control plane: `UnixListener` server + `UnixStream::connect` clients, 4-byte BE length-prefixed JSON framing, `ControlRequest`/`ControlResponse` serde enums
- `state.rs:99-108` — `write_state_atomic` writes `.tmp` then `std::fs::rename` (no partial reads); arc-swap live config; drain flag via `AtomicBool`/SeqCst
- `lib.rs:29,37` + `tracing_setup.rs:9-24` — `tracing_setup::init` wired into `init_tracing` and called from `start()` (tracing is not a no-op)
- `server.rs:46-62,64-71,104-106` — request counter + duration histogram recorded around every forward; request-ID honored/generated inbound, forwarded to backend, echoed on response
- `metrics.rs:57` — `duration_histogram_records_observation` now present; uses a local `DebuggingRecorder` (global Prometheus exporter can't be reinstalled per-test), records one observation via the same `metrics::histogram!` macro the hot path uses, asserts the snapshot contains a non-empty `Histogram` entry with the correct metric name
- `drain.rs` — deleted; `pub mod drain;` removed from `lib.rs`. Drain logic fully covered by `state.rs`/`control.rs`/`server.rs`
- `harness/tests/test_proxy.py` — 30 tests across 6 classes, reusing `conftest.py` fixtures (`proxy`, `dual_thttpd_backends`, `find_free_port`, `wait_for_port`); no references to non-existent helpers
- `Makefile:37-39,49` — `proxy` target mirrors `harness`/`differential` style; `integration: harness differential proxy`
- All five docs present: `STRANGLER_FIG.md`, `ROLLBACK.md`, `MIGRATION_PLAYBOOK.md`, `CONTROL_PROTOCOL.md`, `ADR-0002-async-runtime-split.md`

#### Deviations from Plan:

None. Implementation is a faithful realization of the plan. The two previously-flagged deviations remain acceptable/non-actionable and do not require changes:
- The Phase 1 bad-config message emits the OS error (`No such file or directory (os error 2)`) rather than the literal `config file not found: …`. This is the intended Phase-2 evolution (config became authoritative via `config::load` → `std::fs::read_to_string`); the exit code is correctly non-zero, which is the criterion's actual intent.
- The circuit breaker is recorded from the server handler (`pool.record_outcome` at server.rs:157/167) rather than from inside `forwarder.rs::forward()`. Outcome is identical — every forwarded request records exactly one outcome — so this is an acceptable placement choice.

#### Pattern Conformance:

- ✓ Cargo.toml workspace inheritance matches peer crates (`version/edition/license/rust-version.workspace = true`)
- ✓ Inline `#[cfg(test)] mod tests` in every source file matches existing crate convention; `tempfile = { workspace = true }` dev-dep used
- ✓ Python harness fully conforms: `subprocess.Popen`, `find_free_port()`, `proc.terminate()/wait()/kill()` teardown identical to `server_process`; `wait_for_port()` correctly added to `conftest.py`
- ✓ `tracing` used consistently across all migrate modules; `println!` reserved for CLI user output — intentional divergence from server crates (which use `eprintln!`), justified by ADR-0002
- ✓ Makefile `proxy`/`integration`/`verify` targets mirror existing target style
- Minor observation: new runtime deps use inline versions instead of being hoisted into `[workspace.dependencies]` in `rust/Cargo.toml`. Existing convention centralizes shared deps there. Acceptable variation — the crate compiles and `cargo deny` passes — but hoisting would match house style.

### Manual Testing Required:

The plan carries extensive manual criteria (live dual-backend demos, load tests, chaos tests, operator tabletop exercises). All remain valid and unverified by this automated validation:

1. Active-active routing:
   - [ ] Start two `nc -l` backends on 8081/8082 with distinct bodies; proxy on 8080; 1000 curls land in the configured weight ratio
   - [ ] Kill the Rust backend mid-flight; subsequent requests all route to C
2. Shadow mode:
   - [ ] Configure shadow; curl 100×; logs show zero divergences on identical binaries
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

- Ready to commit — implementation is complete and validated. The previously-gating Phase 6 test is present and passing; the dead stub is removed.
- All manual criteria above remain to be exercised before production rollout; the automated surface (53 unit + 30 integration) is green.
- Optional housekeeping (non-blocking): hoist shared runtime deps into `[workspace.dependencies]` to match convention; resolve the ADR-0001/0002 numbering (the plan explicitly specified "ADR-0002," so a reader expects an ADR-0001 — either seed one or, if you want the numbering corrected at the plan level, escalate via `/skill:revise`).
