# Migration Playbook — thttpd → thttpd-rs

A six-week, low-risk migration from the C `thttpd` to the Rust `thttpd-rs`,
mediated by the `thttpd-migrate` strangler-fig proxy.

## Guiding principles

- **Never lose traffic.** Every step is reversible in one command
  (`rollback --to c-thttpd`).
- **Verify before you shift.** Shadow mode proves equivalence before any user
  sees a Rust response.
- **Ramp, don't flip.** Increase Rust weight gradually; watch metrics at each
  step.
- **You are never wrong to roll back.** If anything looks off, roll back and
  investigate.

## Week 0 — Setup

1. Build both servers: `make build` (Rust), `make legacy` (C).
2. Deploy `thttpd-migrate` alongside the existing C server. Point it at both
   backends with `mode = "active-active"`, `c-thttpd` weight 100,
   `rust-thttpd` weight 0.
3. Point the load balancer at the proxy instead of C directly. **No user traffic
   changes** (it's all still C).
4. Verify `thttpd-migrate status` shows both backends healthy.

## Week 1 — Shadow (zero traffic shift)

1. Set `routing.mode = "shadow"`, `primary_backend = "c-thttpd"`,
   `shadow_backend = "rust-thttpd"`.
2. Mirror 100% of traffic to Rust; users still hit C.
3. Watch `thttpd_migrate_shadow_divergences_total`.
   - **Expected: zero divergences** on identical binaries.
   - If divergences appear, fix Rust before proceeding. Do not advance.
4. Run the existing differential suite (`make differential`) as a sanity check.

## Week 2 — 5% canary

1. Switch to `mode = "active-active"`.
2. `set-weight rust-thttpd=5 c-thttpd=95`.
3. Watch for 5 minutes:
   - `thttpd_migrate_5xx_responses_total{backend="rust-thttpd"}` stays 0.
   - p99 latency on Rust is within 1ms of C.
4. If clean, hold for the rest of the week. If not, `rollback --to c-thttpd`.

## Week 3 — 50% canary

1. `set-weight rust-thttpd=50 c-thttpd=50`.
2. Monitor the same metrics for a full day under peak load.
3. Pay special attention to peak hours and any endpoints that surfaced shadow
   divergences in Week 1.

## Week 4 — 100% Rust

1. `set-weight rust-thttpd=100 c-thttpd=0`.
2. All user traffic is now on Rust. C is still running and taking zero traffic.
3. Hold for the week. The proxy is still in front; `rollback` is one command
   away.

## Week 5 — Decommission C

1. After a clean week at 100%, update the load balancer to point directly at the
   Rust backend, bypassing the proxy.
2. Confirm traffic flows correctly.
3. Shut down C and the proxy.
4. Remove the proxy from the deployment. `thttpd-rs` is now the sole server.

## Rollback at any point

No matter the week:

```bash
thttpd-migrate --control-socket /var/run/thttpd-migrate/control.sock rollback --to c-thttpd
```

Within 1 second all traffic returns to C. See `docs/ROLLBACK.md`.
