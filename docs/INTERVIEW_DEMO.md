# Five-Minute Interview Demo

## Minute 1: The Risk

Legacy rewrites fail when documentation is treated as the specification while
production behavior stays undocumented. This repository keeps the C binary in
the tree as an executable reference: behavior was captured from it before any
Rust dispatch code was written, and the captured records became the contract.

## Minute 2: The Shape

Show the shape and the decisions behind it:

- `legacy/` — the C reference, retained deliberately (not dead source).
- `rust/` — 8 crates that mirror the original C modules one-to-one:
  `thttpd-core` (event loop), `thttpd-http` (parser/CGI/responses),
  `thttpd-fdwatch`, `-timers`, `-mmc` (mmap cache), `-match`, `-tdate`, `-mime`.
- `harness/` — the differential engine that compares the two binaries
  field-by-field.
- `knowledge/` — every C file mapped to its Rust equivalent with `file:line`
  evidence, not a guess.

Two architecture decisions worth naming:

- The **server** uses `mio` with a manual single-threaded event loop, matching
  the C architecture deliberately so structural alignment supports the
  differential tests.
- The **proxy** uses `tokio`/`hyper` because proxying many connections across
  backends is inherently concurrent. See
  `docs/ADR-0002-async-runtime-split.md`.

Current scale: 105 differential scenarios, 193 unit tests, 63 comparator tests
— 472 automated tests in total.

## Minute 3: The Failure Story

Open `JOURNEY.md`. The first port reported all implementation phases complete by
structural gates (a file existed, it compiled, a test was collected) — and did
not answer requests. The first differential run passed 2 of 45 captured cases.

The repair loop found, by category: missing response headers, missing features
(`If-Modified-Since`/`Range`/`HEAD`), security gaps (symlink escape, directory
traversal), CGI output handling, and nondeterminism. The notable bugs were edge
cases in how input is delivered or sized — a CGI stdin deadlock, a negative
`Content-Length` that wrapped to `MAX_USIZE`, and a parser that reset state on
every read.

The methodological payoff: once the harness existed, adding a test cost one raw
request. Coverage grew from 45 to 105 scenarios cheaply because the investment
was front-loaded into the harness, not the individual tests.

## Minute 4: The Proof

Run:

```bash
make demo
make verify
```

`make demo` inventories the verification layers and runs representative static,
parser-hardening, and authentication comparisons. `make verify` is the complete
gate: format, clippy, unit, comparator, security (`cargo-audit`, `cargo-deny`),
legacy, differential, and proxy integration.

The integrity claim: normalized comparison is explicit, and normalization never
hides a bug. Timestamps, test paths, dynamic ports, and CGI `PWD` are normalized
— but the normalized body is still SHA-256 hashed and compared, and the oracle
itself has 63 unit tests. That is what makes the 105 passing scenarios mean what
they appear to mean.

## Minute 5: The Migration Machinery

The project has two phases, both shipped: prove behavioral parity, then make the
cutover safe.

`thttpd-migrate` is a strangler-fig proxy (`docs/STRANGLER_FIG.md`) that shifts
traffic C → Rust incrementally without touching either server. It ships:

- **Shadow mode** — primary serves every request; Rust gets a mirror; responses
  are diffed; the user is never affected.
- **Canary routing** — weighted split (1% → 10% → 50% → 100%).
- **Active health checks, a circuit breaker**, Prometheus `/metrics`, and
  request-id propagation.
- **One-command rollback** over a Unix control socket; in-flight requests
  finish normally.
- **Graceful drain** for planned cutover.

This is covered by 31 integration tests and a six-week migration playbook
(`docs/MIGRATION_PLAYBOOK.md`).

What is *not* done is tracked openly. Open `docs/KNOWN_DEVIATIONS.md`: throttle
enforcement, daemonization, request logging, CGI resource controls, and IPv6 are
not yet at parity, each with its legacy behavior, current Rust behavior, impact,
and disposition. An explicit gap register is more useful than an unqualified
compatibility claim — it is what lets the 105 passing scenarios be trusted.
