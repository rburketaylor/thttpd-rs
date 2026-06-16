# Gap Audit and Interview Readiness

**Updated:** 2026-06-16

## Verified Strengths

- 105 C-vs-Rust differential scenarios exercise status, headers, body, and connection behavior.
- 80 C-only scenarios validate the reference fixtures.
- 193 Rust workspace unit tests cover server and proxy internals.
- 63 comparator tests verify that the parity oracle catches drift.
- 31 proxy integration tests cover routing, shadow mode, health, circuit breaker, rollback, metrics, and drain behavior.
- `cargo fmt`, workspace clippy with `-D warnings`, tests, dependency policy, and integration checks are represented by `make verify` and CI.
- Legacy configuration parsing, bind-before-setuid startup, pidfile writing, and unreadable-password-file behavior are implemented.
- `thttpd-migrate` is implemented as the side-by-side migration path with active-active/canary routing, shadow diffing, health checks, circuit breaker, request IDs, `/metrics`, control socket rollback, and drain.

## Highest Remaining Risks

1. Runtime bandwidth throttling is not wired into response scheduling.
2. Daemonization and request logging are incomplete; SIGHUP cannot reopen a persistent log yet.
3. CGI execution lacks timeout, output bounds, resource limits, and `cgilimit` enforcement.
4. IPv6 and complete legacy argv/config compatibility remain incomplete.

The authoritative list is `docs/KNOWN_DEVIATIONS.md`.

## Claim Boundary

The supported claim is **behavior-preserving request handling under 105
characterized scenarios**, plus a shipped proxy migration path covered by 31
integration tests. It is not yet a full operational drop-in replacement.
The normalized comparison profile still compares body hashes after applying only
documented normalizers.

## Interview Path

1. Start with the structural-vs-behavioral failure story in `JOURNEY.md`.
2. Show the C reference, Rust crates, and differential harness.
3. Run `make demo`.
4. Open the strangler-fig proxy docs, refactor playbook, and known-deviation register.
5. Connect the method to side-by-side migration, shadow verification, controlled cutover, and rollback.
