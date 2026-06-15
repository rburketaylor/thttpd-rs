# Rollback Runbook — thttpd-migrate

## TL;DR

```bash
thttpd-migrate --control-socket /var/run/thttpd-migrate/control.sock rollback --to c-thttpd
```

That's it. All traffic shifts to the named backend within 1 second; in-flight
requests continue to completion; no requests are lost.

## When to use this

Any of:
- Error rate on the Rust backend exceeds 1%
- p99 latency on the Rust backend exceeds baseline + 50%
- A specific endpoint returns 5xx
- An operator is uncertain about the current state

**You are never wrong to roll back.** Roll forward again once you understand the
issue.

## Step-by-step (1-minute procedure)

1. **Confirm the situation** (15s): `thttpd-migrate status --state /var/run/thttpd-migrate/state.json`. Read the `weight=` and `health=` fields.
2. **Roll back** (1s): `thttpd-migrate --control-socket /var/run/thttpd-migrate/control.sock rollback --to c-thttpd`. Look for `rolled back to c-thttpd`.
3. **Verify** (15s): `curl -H 'X-Request-Id: rollback-test' http://proxy:8080/` — confirm the response comes from C (e.g. `Server: thttpd/...`).
4. **Capture evidence** (30s): save `state.json` and the proxy logs.
5. **Postmortem** (later): open a ticket. The state file and logs are the audit trail.

## What rollback does NOT do

- It does **not** stop the Rust backend — it stops sending it traffic. The Rust
  process keeps running so you can attach a debugger.
- It does **not** flush in-flight requests — they complete normally.
- It does **not** require a config edit — it's a one-command operator action.

## What to do if rollback itself fails

| Symptom | Action |
|---|---|
| `thttpd-migrate` command not found | SSH to the proxy host; use the absolute path. |
| "unknown backend" | Check `/etc/thttpd-migrate.toml`; the `--to` value must match a `[backends.*]` name. |
| `control.sock` is missing / stale | The proxy may have restarted; reconnect. If the proxy is down, bypass it (below). |
| `state.json` is stale | Direct traffic at the TCP level: point DNS / load balancer at C's port directly, bypassing the proxy. |
| Proxy is hung | `kill -TERM $(pidof thttpd-migrate)`; if it doesn't exit in 10s, `kill -9`. Then point DNS at C. |

## Verifying a rollback worked

```bash
# Status should show c-thttpd at weight 100, rust at weight 0
thttpd-migrate status --state /var/run/thttpd-migrate/state.json
```

Expected:
```
Backends:
  c-thttpd         127.0.0.1:8081   weight=100  health=Healthy
  rust-thttpd      127.0.0.1:8082   weight=0    health=Healthy
```
