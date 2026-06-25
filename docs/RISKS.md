# Risks and Known Deviations

**Updated:** 2026-06-23

## Status at a Glance

- **105** C-vs-Rust differential scenarios passing
- **256** Rust workspace unit tests
- **63** comparator tests verifying the oracle
- **31** proxy integration tests
- **535** automated tests total

## Verified Strengths

- 105 C-vs-Rust differential scenarios exercise status, headers, body, and connection behavior.
- 80 C-only scenarios validate the reference fixtures.
- 256 Rust workspace unit tests cover server and proxy internals.
- 63 comparator tests verify that the parity oracle catches drift.
- 31 proxy integration tests cover routing, shadow mode, health, circuit breaker, rollback, metrics, and drain behavior.
- `cargo fmt`, workspace clippy with `-D warnings`, tests, dependency policy, and integration checks are represented by `make verify` and CI.
- Legacy configuration parsing, bind-before-setuid startup, pidfile writing, and unreadable-password-file behavior are implemented.
- `thttpd-migrate` is implemented as the side-by-side migration path with active-active/canary routing, shadow diffing, health checks, circuit breaker, request IDs, `/metrics`, control socket rollback, and drain.
- Security migration evidence is implemented: historical CVE mapping, unsafe-boundary audit, `cargo-audit` / `cargo-deny` / `cargo-geiger`, Miri, ASan, cargo-fuzz smoke targets, and release SBOM workflow artifacts are present in the tree.

## Known Deviations

| Area | Legacy behavior | Current Rust behavior | Impact | Disposition |
|---|---|---|---|---|
| CGI resource control | Process behavior follows legacy fork/exec model and enforces `cgilimit` at request/CGI execution time | Parses `cgilimit` from CLI/config, but uses `Command` without runtime `cgilimit` enforcement, timeout, or output bounds | A hung, noisy, or over-concurrent CGI can consume resources | Add `cgilimit` admission checks, timeout, output cap, termination, and tests |
| CGI working directory | Changes to the CGI directory | Inherits the server working directory | `PWD` differs and is normalized in tests | Decide whether to match legacy behavior |
| `VHOST_DIRLEVELS` | Optional compile-time directory splitting | Omitted | Relevant only when the legacy option is enabled | Document build assumption or implement |

## Implementation Notes

- CGI limits: `cgi.rs` launches scripts via `Command` and closes stdin, but does not enforce `cgilimit`, timeout, output cap, or child termination policy. CGI stdout is read into an unbounded buffer, so a CGI that produces runaway output is an unbounded-memory vector until the response completes.

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

## Claim Boundary

The supported claim is **behavior-preserving request handling under 105
characterized scenarios**, plus a shipped proxy migration path covered by 31
integration tests. It is not yet a full operational drop-in replacement.
The normalized comparison profile still compares body hashes after applying only
documented normalizers.

## See also

- [INTERVIEW.md](INTERVIEW.md) — interview presenter script
- [JOURNEY.md](../JOURNEY.md) — migration case study
- [MIGRATION.md](MIGRATION.md) — migration guide
