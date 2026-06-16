# thttpd-migrate — Strangler-Fig Migration Proxy

## Source
New Rust migration tooling in `rust/crates/thttpd-migrate/src/` (6,003 lines across 15 modules).

## Status
Implemented. 31 proxy integration tests cover routing, shadow mode, health,
circuit breaker, rollback, metrics, and drain behavior.

## What It Does
`thttpd-migrate` sits in front of the legacy C `thttpd` and the Rust
`thttpd-rs` server so traffic can move gradually between them. It is not part
of the parity server's `mio` event loop; it is an async `tokio`/`hyper` proxy
chosen for concurrent client/backend forwarding.

| Rust module | Responsibility |
|-------------|----------------|
| `config.rs` | TOML config, backend validation, routing mode validation |
| `backend.rs` | Backend pool, live weights, health state |
| `router.rs` | Active-active/canary weighted routing and shadow primary selection |
| `forwarder.rs` | HTTP request rebuilding, hop-by-hop stripping, backend forwarding |
| `shadow.rs` | Shadow request mirroring, capped body capture, response diff dispatch |
| `diff.rs` | Normalized response comparison for shadow-mode divergence detection |
| `health.rs` | Active health probing; 2xx succeeds, non-2xx/timeouts/connect errors fail |
| `circuit.rs` | Per-backend rolling-window circuit breaker with fixed 5-second cool-off |
| `metrics.rs` | Prometheus metric registration/export; exporter serves `/metrics` |
| `state.rs` | Live config snapshots, state file writes, semantic rollback, drain state |
| `control.rs` | Unix control socket protocol for set-weight, rollback, drain, snapshot |
| `server.rs` | Data-plane listener, request handling, graceful drain enforcement |
| `tracing_setup.rs` | Pretty/JSON tracing setup |

## Key Decisions
- Active-active and canary modes use weighted routing over healthy, breaker-admitted backends.
- Shadow mode always serves `routing.primary_backend`; mirroring happens only when `routing.shadow_backend` is routable and admitted by the circuit breaker.
- Shadow diffs are logged and counted but never affect the user response.
- Rollback is semantic: the target backend gets weight 100 and others get 0. In shadow mode it also updates live `primary_backend` / `shadow_backend`.
- Drain stops accepting new connections, lets in-flight work finish for the configured grace period, then aborts remaining connection tasks.
- `metrics.path` is currently advisory because the Prometheus exporter serves `/metrics`.
