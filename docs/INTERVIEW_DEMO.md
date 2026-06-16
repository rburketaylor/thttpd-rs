# Five-Minute Interview Demo

## Minute 1: The Risk

Legacy rewrites fail when documentation is treated as the specification while
production behavior remains undocumented. This repository keeps the C binary as
an executable reference.

## Minute 2: The Shape

Show `legacy/`, the Rust workspace, `harness/`, `knowledge/`, and
`rust/crates/thttpd-migrate`. Explain that preserving module and event-loop
boundaries reduced migration risk, while the proxy gives operators a controlled
cutover path.

## Minute 3: The Failure Story

Open `JOURNEY.md`. The first implementation looked structurally complete but the
initial differential run passed only 2 of 45 captured cases. The repair loop
found missing headers, parser state loss, CGI deadlock, negative lengths, and
security differences.

## Minute 4: The Proof

Run:

```bash
make demo
```

The script inventories the verification layers, including the proxy integration
suite, and runs representative static, parser-hardening, and authentication
comparisons. `make verify` is the complete format, lint, unit, security,
legacy, differential, and proxy gate.

Explain that normalized comparison is explicit: timestamps, test paths, dynamic
ports, and CGI `PWD` are normalized; the normalized body is still hashed and
compared.

## Minute 5: The Business Connection

The transferable method is to capture current workflows, run old and new systems
side by side, use `thttpd-migrate` for shadow verification and canary traffic,
track deviations openly, and preserve rollback until production evidence
supports cutover. Open `docs/STRANGLER_FIG.md` and `docs/KNOWN_DEVIATIONS.md`
to show that migration control and request parity are managed separately.
