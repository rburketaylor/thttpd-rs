# Known Deviations

This register separates verified request parity from incomplete operational
compatibility. It should be updated whenever a deviation is fixed or accepted.

| Area | Legacy behavior | Current Rust behavior | Impact | Disposition |
|---|---|---|---|---|
| Bandwidth throttling | Enforces per-pattern transfer rates and fair sharing | Parses and unit-tests rules, but the event loop does not enforce them | Signature server feature is incomplete | Implement with timing-based differential tests |
| Daemonization | Daemonizes unless `-D` is supplied | Remains in the foreground | Service-wrapper compatibility | Implement or require a process supervisor explicitly |
| Request logging | Logs requests and reopens log files on SIGHUP | No persistent request log; SIGHUP is informational | Log rotation and access auditing differ | Add a logging owner to `Server` and test reopen behavior |
| CGI resource control | Process behavior follows legacy fork/exec model | Uses `Command` without timeout or output bounds | A hung or noisy CGI can consume resources | Add timeout, output cap, termination, and tests |
| CGI working directory | Changes to the CGI directory | Inherits the server working directory | `PWD` differs and is normalized in tests | Decide whether to match legacy behavior |
| `VHOST_DIRLEVELS` | Optional compile-time directory splitting | Omitted | Relevant only when the legacy option is enabled | Document build assumption or implement |
| IPv6 listeners | Can bind IPv4 and IPv6 | Binds one resolved listener, normally IPv4 | IPv6-only clients cannot connect | Add dual-stack listener tests |
| CLI compatibility | Supports legacy short flags such as `-h`, `-g`, and `-s` | Some short flags differ or are unavailable | Existing scripts may require changes | Complete an argv compatibility matrix |
| Symlink/data-dir config | Supports `data_dir` and symlink directives | Recognizes and rejects them explicitly | Config migration stops with an actionable error | Implement before claiming full config parity |

## Recently Closed

| Area | Resolution |
|---|---|
| Comparison body hashes | Normalized mode now hashes normalized bodies instead of forcing a match |
| Comparator coverage | Comparator unit tests are included in `make check` and CI |
| Config-file handling | `-C` now parses supported legacy directives and rejects unknown/unsupported options |
| Privileged bind ordering | Listeners bind after chroot and before setuid/setgid |
| Pidfile | The configured pidfile is written during successful startup |
| Unreadable `.htpasswd` | Returns the legacy 403 result instead of 401 |
