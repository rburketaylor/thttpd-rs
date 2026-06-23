# Gap Audit and Interview Readiness

**Updated:** 2026-06-23

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

## Highest Remaining Risks

1. CGI execution lacks timeout, output bounds, resource limits, and `cgilimit` enforcement. The implementation uses `std::process::Command` with piped stdio and closes stdin correctly, but it does not supervise runtime resource use; CGI stdout is read into an unbounded buffer.
2. CGI working directory differs from legacy (the server's working directory is inherited rather than the CGI directory); this is normalized away in the differential suite via the `PWD` normalizer.
3. `VHOST_DIRLEVELS` compile-time directory splitting is intentionally omitted.

Throttling, daemonization, request logging, IPv6 dual-stack listeners, and full legacy argv/config compatibility have since reached parity — see the "Recently Closed" section of the authoritative list.

The authoritative list is `docs/KNOWN_DEVIATIONS.md`.

## Claim Boundary

The supported claim is **behavior-preserving request handling under 105
characterized scenarios**, plus a shipped proxy migration path covered by 31
integration tests. It is not yet a full operational drop-in replacement.
The normalized comparison profile still compares body hashes after applying only
documented normalizers.

## Interview Path

The canonical five-minute walkthrough is [INTERVIEW_DEMO.md](INTERVIEW_DEMO.md):
the risk, the shape and architecture decisions, the structural-vs-behavioral
failure story, the proof (`make verify`), and the strangler-fig migration
machinery alongside the known-deviation register. This audit records the
verified strengths and remaining risks; it does not maintain a second script.
