# Known Deviations

This register separates verified request parity from incomplete operational
compatibility. It should be updated whenever a deviation is fixed or accepted.
The current entries below were rechecked against the Rust implementation on
2026-06-21.

| Area | Legacy behavior | Current Rust behavior | Impact | Disposition |
|---|---|---|---|---|
| Bandwidth throttling | Enforces per-pattern transfer rates and fair sharing | Parses and unit-tests rules, but the event loop does not enforce them | Signature server feature is incomplete | Implement with timing-based differential tests |
| Daemonization | Daemonizes unless `-D` is supplied | Remains in the foreground | Service-wrapper compatibility | Implement or require a process supervisor explicitly |
| Request logging | Logs requests and reopens log files on SIGHUP | No persistent request log; SIGHUP is informational | Log rotation and access auditing differ | Add a logging owner to `Server` and test reopen behavior |
| CGI resource control | Process behavior follows legacy fork/exec model and enforces `cgilimit` at request/CGI execution time | Parses `cgilimit` from CLI/config, but uses `Command` without runtime `cgilimit` enforcement, timeout, or output bounds | A hung, noisy, or over-concurrent CGI can consume resources | Add `cgilimit` admission checks, timeout, output cap, termination, and tests |
| CGI working directory | Changes to the CGI directory | Inherits the server working directory | `PWD` differs and is normalized in tests | Decide whether to match legacy behavior |
| `VHOST_DIRLEVELS` | Optional compile-time directory splitting | Omitted | Relevant only when the legacy option is enabled | Document build assumption or implement |
| IPv6 listeners | Can bind IPv4 and IPv6 | Binds one resolved listener, normally IPv4 | IPv6-only clients cannot connect | Add dual-stack listener tests |
| CLI compatibility | Supports legacy short flags such as `-h`, `-g`, and `-s` | Some short flags differ or are unavailable | Existing scripts may require changes | Complete an argv compatibility matrix |
| Symlink/data-dir config | Supports `data_dir` and symlink directives | Recognizes and rejects them explicitly | Config migration stops with an actionable error | Implement before claiming full config parity |

## Implementation Notes

- Throttling: `throttle.rs` parses and matches rules, but `eventloop.rs`
  sends from `response[bytes_sent..]` directly and tracks only byte counters.
- Daemon/logging: `ServerConfig` carries `daemonize` and `logfile`, but
  `main.rs` has no daemonization step and SIGHUP handling is a placeholder.
- CGI limits: `cgi.rs` launches scripts via `Command` and closes stdin, but
  does not enforce `cgilimit`, timeout, output cap, or child termination policy.
- IPv6: `startup.rs` builds one `host:port` address and returns a single
  listener. The fdwatch crate reserves IPv4/IPv6 token names, but startup does
  not yet bind a dual listener set.

## Recently Closed

| Area | Resolution |
|---|---|
| Comparison body hashes | Normalized mode now hashes normalized bodies instead of forcing a match |
| Comparator coverage | Comparator unit tests are included in `make check` and CI |
| Config-file handling | `-C` now parses supported legacy directives and rejects unknown/unsupported options |
| Privileged bind ordering | Listeners bind after chroot and before setuid/setgid |
| Pidfile | The configured pidfile is written during successful startup |
| Unreadable `.htpasswd` | Returns the legacy 403 result instead of 401 |
