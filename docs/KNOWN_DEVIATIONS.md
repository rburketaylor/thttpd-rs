# Known Deviations

This register separates verified request parity from incomplete operational
compatibility. It should be updated whenever a deviation is fixed or accepted.
The current entries below were rechecked against the Rust implementation on
2026-06-23.

| Area | Legacy behavior | Current Rust behavior | Impact | Disposition |
|---|---|---|---|---|
| CGI resource control | Process behavior follows legacy fork/exec model and enforces `cgilimit` at request/CGI execution time | Parses `cgilimit` from CLI/config, but uses `Command` without runtime `cgilimit` enforcement, timeout, or output bounds | A hung, noisy, or over-concurrent CGI can consume resources | Add `cgilimit` admission checks, timeout, output cap, termination, and tests |
| CGI working directory | Changes to the CGI directory | Inherits the server working directory | `PWD` differs and is normalized in tests | Decide whether to match legacy behavior |
| `VHOST_DIRLEVELS` | Optional compile-time directory splitting | Omitted | Relevant only when the legacy option is enabled | Document build assumption or implement |

## Implementation Notes

- CGI limits: `cgi.rs` launches scripts via `Command` and closes stdin, but
  does not enforce `cgilimit`, timeout, output cap, or child termination
  policy. CGI stdout is read into an unbounded buffer, so a CGI that produces
  runaway output is an unbounded-memory vector until the response completes.

## Recently Closed

| Area | Resolution |
|---|---|
| Bandwidth throttling | `handle_send` caps body bytes to the per-window allowance, pauses when it is exhausted, accounts bytes against the throttle table, and resumes on fair-share recalibration (`eventloop.rs`). Verified by 12 `TestDifferentialThrottling` scenarios plus the throttle unit tests. |
| Daemonization | `startup::daemonize` / `daemonize_with_handshake` perform the double-fork + `setsid` and reopen stdio onto `/dev/null`, toggled off by `-D` (`main.rs`). |
| Request logging | `logging::LogTarget` appends to the configured logfile and reopens it on `SIGHUP` (`eventloop.rs`); the access log follows the legacy CERN/Common format. |
| IPv6 listeners | `bind_listeners` binds `[::]:port` (v6only) and `0.0.0.0:port`, continuing if one family is unsupported, and binds every resolved address when `-h` is supplied (`startup.rs`). |
| CLI compatibility | All legacy short flags are handled: `-h/-g/-s/-r/-v` via the argv-normalization shim and `-p/-d/-u/-l/-c/-T/-P/-M/-C/-t/-H/-i/-D/-V` via clap, with last-wins semantics covered by the legacy-argv tests. |
| Symlink/data-dir config | `data_dir` and the `symlink`/`nosymlink` config directives are parsed and honored (`config.rs`). |
| Comparison body hashes | Normalized mode now hashes normalized bodies instead of forcing a match |
| Comparator coverage | Comparator unit tests are included in `make check` and CI |
| Config-file handling | `-C` now parses supported legacy directives and rejects unknown/unsupported options |
| Privileged bind ordering | Listeners bind after chroot and before setuid/setgid |
| Pidfile | The configured pidfile is written during successful startup |
| Unreadable `.htpasswd` | Returns the legacy 403 result instead of 401 |
