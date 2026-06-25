# ADR-0002: Async runtime split (proxy uses tokio; server stays mio)

Date: 2026-06-13
Status: Accepted

## Context

`thttpd-rs` deliberately reimplements thttpd's C architecture using `mio` with
a manual single-threaded event loop. This fidelity is a feature: it keeps the
Rust server structurally aligned with the C original for the golden-master
differential tests.

The migration proxy (`thttpd-migrate`) is a different kind of system. It manages
many concurrent client connections, multiplexes them across multiple backends,
runs shadow diffing, health probing, and a control plane — all concurrently.

## Decision

- The **server** (`thttpd-rs`) keeps its `mio`-based single-threaded event loop.
- The **proxy** (`thttpd-migrate`) uses `tokio` + `hyper`.

## Rationale

- Proxy logic is naturally concurrent (many connections × multiple backends).
  Re-implementing that concurrency on `mio` would be busywork that hides the
  actual proxy logic behind event-loop plumbing.
- The proxy is migration tooling, not part of the server's hot path. Its
  performance target (<1ms p99 overhead at 1k req/s) is easily met by a mature
  async stack.
- Production proxies (Envoy, nginx, HAProxy) are async; `tokio`/`hyper` is the
  idiomatic Rust realization of that model.

## Consequences

- The project gains a second runtime. The proxy does **not** share the server's
  `mio` event-loop code; they are independent binaries.
- The proxy brings additional transitive dependencies (tokio, hyper,
  metrics-exporter-prometheus, etc.). All are permissively licensed and pass
  `cargo deny` (see `deny.toml`).
- Once migration completes (see `docs/MIGRATION.md`), the proxy is
  decommissioned and the dependency surface shrinks again.

## References

- Implementation: `rust/crates/thttpd-migrate`
- Martin Fowler, *StranglerFigApplication* (2004)
- User guide: `docs/MIGRATION.md`
