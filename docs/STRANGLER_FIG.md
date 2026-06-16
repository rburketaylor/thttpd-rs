# Strangler-Fig Migration Proxy (`thttpd-migrate`)

## What is this?

`thttpd-migrate` is a migration proxy that sits in front of the legacy C
`thttpd` and the new Rust `thttpd-rs`, implementing the **strangler fig**
pattern (Martin Fowler, 2004). It lets you shift traffic from C to Rust
incrementally, verify correctness in shadow mode, and roll back in one command
— without touching either server.

## Architecture

```
            ┌────────────────────────────────────────────────────────────┐
            │                  thttpd-migrate (proxy)                   │
   client ──┤  listener ─▶ router (weighted) ┶ active backend (C or Rust)│
            │                                 ┶ shadow backend (Rust)    │
            │                    diff_engine / health / circuit breaker   │
            │                    /metrics (Prometheus)                   │
            └────────────────────────────────────────────────────────────┘
                            │                                │
                            ▼                                ▼
                   ┌──────────────┐                 ┌──────────────┐
                   │  thttpd (C)  │                 │ thttpd-rs    │
                   └──────────────┘                 └──────────────┘
```

The proxy is a **new component**: it does not modify `thttpd-rs` (which keeps
its `mio`-based single-threaded architecture). The proxy uses `tokio` + `hyper`
because proxying is inherently concurrent across many connections and backends.
See `docs/ADR-0002-async-runtime-split.md` for the rationale.

## Quick start

```bash
# Build
make build

# Create a config (config/thttpd-migrate.example.toml is checked in)
cp config/thttpd-migrate.example.toml /etc/thttpd-migrate.toml
$EDITOR /etc/thttpd-migrate.toml

# Start (95% C, 5% Rust)
thttpd-migrate start --config /etc/thttpd-migrate.toml

# Promote Rust to 100%
thttpd-migrate --control-socket /var/run/thttpd-migrate/control.sock \
    set-weight rust-thttpd=100 c-thttpd=0

# Emergency rollback to C
thttpd-migrate --control-socket /var/run/thttpd-migrate/control.sock \
    rollback --to c-thttpd

# Inspect runtime state
thttpd-migrate status --state /var/run/thttpd-migrate/state.json
```

## Routing modes

| Mode | Behavior | When to use |
|---|---|---|
| `active-active` | Weighted random split across healthy backends | Day-to-day canary ramps |
| `canary` | Mechanically identical to active-active; operationally a gradual ramp | Phased rollouts (1% → 10% → 50% → 100%) |
| `shadow` | Primary serves every request; shadow (Rust) receives a mirror and responses are diffed; **the user is never affected** | Pre-rollout correctness verification |

In **shadow mode**, `routing.primary_backend` is always served and
`routing.shadow_backend` receives a mirrored copy only when that backend is
healthy/routable and admitted by its circuit breaker. Divergences are logged
and counted in `thttpd_migrate_shadow_divergences_total` but never reach the
client.

## Health & circuit breaker

- **Active health checks**: each backend's `health_path` is probed every
  `health.interval_ms`. `failure_threshold` consecutive failures mark a backend
  `Unhealthy`; `success_threshold` consecutive successes restore it. A health
  probe succeeds only on a 2xx response. 4xx, 5xx, timeouts, and connect/request
  errors are failures. Unhealthy backends are excluded from routing.
- **Circuit breaker**: a per-backend rolling window trips (opens) when the error
  rate exceeds `circuit_breaker.error_rate_threshold` *and* the request volume
  reaches `circuit_breaker.min_requests`. The cool-off is fixed at 5 seconds;
  after that it half-opens for a single probe. Success closes it, failure
  re-opens it.
- **Shadow rollback**: rollback sets the target backend's weight to 100 and all
  others to 0. In shadow mode it also updates the live `primary_backend` to the
  rollback target and moves the previous primary into `shadow_backend`.

## Observability — what to alert on

Prometheus metrics are served on the configured metrics listener
(`127.0.0.1:9100/metrics` by default), separate from the data plane. The
`metrics.path` config field is currently advisory; the Prometheus exporter
serves `/metrics`.

| Metric | Alert when |
|---|---|
| `thttpd_migrate_5xx_responses_total{backend=...}` | rate > 0 on Rust during canary |
| `thttpd_migrate_shadow_divergences_total{backend=...}` | rate > 0 in shadow mode |
| `thttpd_migrate_request_duration_seconds{backend="rust-thttpd"}` p99 | exceeds C baseline + 50% |

Every request carries an `X-Request-Id` (honored inbound, forwarded to backends,
echoed back). Structured logs go to stderr; set
`THTTPD_MIGRATE_LOG_FORMAT=json` for JSON output in production.

## Common failure modes

| Symptom | Action |
|---|---|
| Rust canary returns 5xx | `rollback --to c-thttpd`; new selections move to C while in-flight requests continue normally. See `ROLLBACK.md`. |
| Shadow divergences appear | Inspect logs for the `field` and `request_id`; fix Rust before ramping. |
| Proxy itself is unhealthy | Bypass it: point DNS/load balancer at C's port directly. |

## See also

- [Rollback runbook](ROLLBACK.md)
- [Control protocol spec](CONTROL_PROTOCOL.md)
- [Migration playbook](MIGRATION_PLAYBOOK.md)
- [ADR-0002: async runtime split](ADR-0002-async-runtime-split.md)
