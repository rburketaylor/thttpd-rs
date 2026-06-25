# Migration Guide

## Overview

`thttpd-migrate` is a migration proxy that sits in front of the legacy C
`thttpd` and the new Rust `thttpd-rs`, implementing the **strangler fig**
pattern (Martin Fowler, 2004). It lets you shift traffic from C to Rust
incrementally, verify correctness in shadow mode, and roll back in one command
— without touching either server.

The proxy is a **new component**: it does not modify `thttpd-rs` (which keeps
its `mio`-based single-threaded architecture). The proxy uses `tokio` + `hyper`
because proxying is inherently concurrent across many connections and backends.
See `docs/ADR-0002-async-runtime-split.md` for the rationale.

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

## Quick Start

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

## Routing Modes

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

## Six-Week Migration Timeline

### Guiding principles

- **Never lose traffic.** Every step is reversible in one command
  (`rollback --to c-thttpd`), and in-flight requests continue normally.
- **Verify before you shift.** Shadow mode proves equivalence before any user
  sees a Rust response.
- **Ramp, don't flip.** Increase Rust weight gradually; watch metrics at each
  step.
- **You are never wrong to roll back.** If anything looks off, roll back and
  investigate.

### Week 0 — Setup

1. Build both servers: `make build` (Rust), `make legacy` (C).
2. Deploy `thttpd-migrate` alongside the existing C server. Point it at both
   backends with `mode = "active-active"`, `c-thttpd` weight 100,
   `rust-thttpd` weight 0.
3. Point the load balancer at the proxy instead of C directly. **No user traffic
   changes** (it's all still C).
4. Verify `thttpd-migrate status` shows both backends healthy.

### Week 1 — Shadow (zero traffic shift)

1. Set `routing.mode = "shadow"`, `primary_backend = "c-thttpd"`,
   `shadow_backend = "rust-thttpd"`.
2. Mirror 100% of traffic to Rust; users still hit C.
3. Watch `thttpd_migrate_shadow_divergences_total`.
   - **Expected: zero divergences** on identical binaries.
   - If divergences appear, fix Rust before proceeding. Do not advance.
4. Run the existing differential suite (`make differential`) as a sanity check.

### Week 2 — 5% canary

1. Switch to `mode = "active-active"`.
2. `set-weight rust-thttpd=5 c-thttpd=95`.
3. Watch for 5 minutes:
   - `thttpd_migrate_5xx_responses_total{backend="rust-thttpd"}` stays 0.
   - p99 latency on Rust is within 1ms of C.
4. If clean, hold for the rest of the week. If not, `rollback --to c-thttpd`.

### Week 3 — 50% canary

1. `set-weight rust-thttpd=50 c-thttpd=50`.
2. Monitor the same metrics for a full day under peak load.
3. Pay special attention to peak hours and any endpoints that surfaced shadow
   divergences in Week 1.

### Week 4 — 100% Rust

1. `set-weight rust-thttpd=100 c-thttpd=0`.
2. All user traffic is now on Rust. C is still running and taking zero traffic.
3. Hold for the week. The proxy is still in front; `rollback` is one command
   away.

### Week 5 — Decommission C

1. After a clean week at 100%, update the load balancer to point directly at the
   Rust backend, bypassing the proxy.
2. Confirm traffic flows correctly.
3. Shut down C and the proxy.
4. Remove the proxy from the deployment. `thttpd-rs` is now the sole server.

## Rollback Procedures

### TL;DR

```bash
thttpd-migrate --control-socket /var/run/thttpd-migrate/control.sock rollback --to c-thttpd
```

That's it. The live routing state is updated immediately for new backend
selections; in-flight requests continue normally on the backend that already
accepted them.

### When to rollback

Any of:
- Error rate on the Rust backend exceeds 1%
- p99 latency on the Rust backend exceeds baseline + 50%
- A specific endpoint returns 5xx
- An operator is uncertain about the current state

**You are never wrong to roll back.** Roll forward again once you understand the
issue.

### Step-by-step (1-minute procedure)

1. **Confirm the situation** (15s): `thttpd-migrate status --state /var/run/thttpd-migrate/state.json`. Read the `weight=` and `health=` fields.
2. **Roll back**: `thttpd-migrate --control-socket /var/run/thttpd-migrate/control.sock rollback --to c-thttpd`. Look for `rolled back to c-thttpd`.
3. **Verify** (15s): `curl -H 'X-Request-Id: rollback-test' http://proxy:8080/` — confirm the response comes from C (e.g. `Server: thttpd/...`).
4. **Capture evidence** (30s): save `state.json` and the proxy logs.
5. **Postmortem** (later): open a ticket. The state file and logs are the audit trail.

### What rollback does NOT do

- It does **not** stop the Rust backend — it stops sending it traffic. The Rust
  process keeps running so you can attach a debugger.
- It does **not** flush in-flight requests — they complete normally.
- It does **not** require a config edit — it's a one-command operator action.

### Failure mode recovery

| Symptom | Action |
|---|---|
| `thttpd-migrate` command not found | SSH to the proxy host; use the absolute path. |
| "unknown backend" | Check `/etc/thttpd-migrate.toml`; the `--to` value must match a `[backends.*]` name. |
| `control.sock` is missing / stale | The proxy may have restarted; reconnect. If the proxy is down, bypass it. |
| `state.json` is stale | Direct traffic at the TCP level: point DNS / load balancer at C's port directly, bypassing the proxy. |
| Proxy is hung | `kill -TERM $(pidof thttpd-migrate)`; if it doesn't exit in 10s, `kill -9`. Then point DNS at C. |

## Health & Circuit Breaker

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

## Observability — What to Alert On

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

## Common Failure Modes

| Symptom | Action |
|---|---|
| Rust canary returns 5xx | `rollback --to c-thttpd`; new selections move to C while in-flight requests continue normally. |
| Shadow divergences appear | Inspect logs for the `field` and `request_id`; fix Rust before ramping. |
| Proxy itself is unhealthy | Bypass it: point DNS/load balancer at C's port directly. |

## See also

- [RISKS.md](RISKS.md) — current gaps and status
- [CONTROL_PROTOCOL.md](CONTROL_PROTOCOL.md) — control socket reference
- [ROADMAP.md](../ROADMAP.md) — full migration roadmap
- [ADR-0002: async runtime split](ADR-0002-async-runtime-split.md)
