# Control Protocol — `thttpd-migrate`

The CLI subcommands `set-weight`, `rollback`, and `drain` communicate with a
running proxy over a Unix domain socket at `config.control_socket` (default
`/var/run/thttpd-migrate/control.sock`). The global `--control-socket` flag
overrides the path (useful in tests and non-root demos).

## Wire format

Length-prefixed JSON over a stream Unix socket:

```
[4-byte big-endian length][UTF-8 JSON payload]
```

The client opens a connection, sends one `ControlRequest`, reads one
`ControlResponse`, and closes the connection. Frames are capped at 16 MiB.

Protocol version: **1** (reported in every `ControlResponse.version`).

## Requests

All requests carry a `"command"` tag:

### `set_weight`

```json
{"command":"set_weight","weights":{"rust-thttpd":100,"c-thttpd":0}}
```

Applies the named weight overrides. Unknown backend names return an error.
Weights are also mirrored into the live config snapshot and `state.json`.

### `rollback`

```json
{"command":"rollback","to":"c-thttpd"}
```

Semantic rollback: the target backend's weight becomes 100 and every other
backend's weight becomes 0. It does **not** use sentinel `u32::MAX` weights.

### `drain`

```json
{"command":"drain","timeout_secs":30}
```

Sets the drain flag; the proxy stops accepting new connections and lets in-flight
requests finish. `timeout_secs` sets the enforced grace period for that drain:
after it expires, any remaining connection tasks are aborted.

### `snapshot`

```json
{"command":"snapshot"}
```

Returns the current runtime state (backends, weights, health, uptime, draining)
without mutating anything. `thttpd-migrate status` reads `state.json` directly
rather than using this command, but external tooling may use it.

## Responses

```json
{"ok":true,"message":"rolled back to c-thttpd","version":1,
 "snapshot":{"uptime_secs":120,"draining":false,"backends":[...]}}
```

On error:

```json
{"ok":false,"message":"rollback failed: unknown backend in rollback target: nope",
 "version":1,"snapshot":null}
```

## Examples

```bash
# Promote Rust
thttpd-migrate --control-socket /tmp/control.sock set-weight rust-thttpd=100 c-thttpd=0

# Roll back to C
thttpd-migrate --control-socket /tmp/control.sock rollback --to c-thttpd

# Drain for a planned cutover
thttpd-migrate --control-socket /tmp/control.sock drain --timeout-secs 30
```

## Compatibility

The protocol is local-only (Unix socket) and versioned via the `version` field.
A future incompatible change will bump the version; the CLI and proxy are
released together so they always agree.
