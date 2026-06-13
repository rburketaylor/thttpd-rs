# Five-Minute Interview Demo

## Minute 1: The Risk

Legacy rewrites fail when documentation is treated as the specification while
production behavior remains undocumented. This repository keeps the C binary as
an executable reference.

## Minute 2: The Shape

Show `legacy/`, the eight-crate Rust workspace, `harness/`, and `knowledge/`.
Explain that preserving module and event-loop boundaries reduced migration risk.

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

The script inventories the verification layers and runs representative static,
parser-hardening, and authentication comparisons. `make verify` is the complete
format, lint, unit, security, legacy, and differential gate.

Explain that normalized comparison is explicit: timestamps, test paths, dynamic
ports, and CGI `PWD` are normalized; the normalized body is still hashed and
compared.

## Minute 5: The Business Connection

The transferable method is to capture current workflows, run old and new systems
side by side, track deviations openly, and preserve rollback until production
evidence supports cutover. Open `docs/KNOWN_DEVIATIONS.md` to show that request
parity and operational parity are managed separately.
