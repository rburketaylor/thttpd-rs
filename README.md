# thttpd-rs

**A legacy-system modernization case study: `sthttpd 2.27.0` ported from C to
Rust and verified against the original binary with differential tests.**

The goal is not to invent a new web server. The goal is to demonstrate a safe
way to replace an old system whose real specification lives in its observable
behavior:

1. Keep the legacy implementation available as an executable specification.
2. Capture representative behavior before changing it.
3. Port behind clear module boundaries.
4. Run old and new implementations side by side.
5. Track normalization and known deviations explicitly.
6. Modernize only after the parity gates are trustworthy.

## Verification

The repository currently contains **472 automated tests**:

| Layer | Tests | Purpose |
|---|---:|---|
| C-vs-Rust differential scenarios | 105 | Compare externally observable request behavior |
| Legacy C harness scenarios | 80 | Prove fixtures and scenarios against the reference server |
| Rust workspace unit tests | 193 | Verify server and proxy internals, including parser, protocol, cache, timer, auth, routing, shadow diffing, health, and control-plane behavior |
| Comparator unit tests | 63 | Prove that the differential oracle detects meaningful drift |
| Proxy integration tests | 31 | Exercise `thttpd-migrate` routing, shadowing, health, circuit breaker, rollback, metrics, and drain behavior |

Run the complete gate with:

```bash
python3 -m pip install -r requirements-dev.txt
cargo install cargo-audit cargo-deny --locked
make verify
```

`make check` runs the fast formatting, lint, unit, comparator, and knowledge
checks. `make integration` builds both implementations and runs `harness`,
`differential`, and `proxy` integration suites.

## Comparison Strictness

The differential engine exposes two profiles:

- `exact` compares every captured field without normalization.
- `normalized` compares status, header presence/order, and connection outcome
  exactly, then explicitly normalizes documented nondeterministic values before
  comparing header values and body SHA-256 hashes.

The 105 live differential scenarios use the normalized profile because the two
processes necessarily produce different timestamps, temporary paths, allocated
ports, and process working directories. Normalized mode does **not** skip body
comparison: it hashes the normalized body and fails on any remaining mutation.

Current normalizers are limited to:

- RFC 1123 `Date` and `Last-Modified` values
- test temporary-directory paths
- dynamically allocated CGI ports and host values
- CGI `PWD`, where the legacy process changes directory and the Rust process does not

See [Known Deviations](docs/KNOWN_DEVIATIONS.md) for the operational surfaces
that are not yet parity-complete.

## Architecture

The Rust port preserves the original single-threaded, event-driven design and
uses `mio` directly rather than introducing an async runtime.

```text
thttpd-core
├── thttpd-http       request parsing, auth, CGI, responses, directory listing
├── thttpd-fdwatch    mio-based readiness polling
├── thttpd-timers     timer wheel
├── thttpd-mmc        memory-mapped file cache
├── thttpd-match      shell-style glob matching
├── thttpd-tdate      HTTP date parsing
└── thttpd-mime       MIME and content-encoding lookup
```

The `legacy/` directory is intentionally retained. It is the reference
implementation used by the characterization and differential suites, not dead
source waiting to be deleted.

## Repository Map

```text
rust/                  Rust workspace and server binary
legacy/                upstream C reference implementation
harness/               pytest fixtures, scenarios, and comparison engine
pipeline/              legacy build, capture, report, and validation scripts
knowledge/             structured C-to-Rust migration records
docs/                  playbook, risks, security notes, and demo guide
scripts/demo.sh         short interview demonstration
JOURNEY.md              migration case study and lessons
Makefile                one-command quality and verification gates
```

## Build

Prerequisites:

- Rust 1.85, pinned by `rust-toolchain.toml`
- a C compiler and autotools for the legacy reference binary
- Python 3.10 or newer

```bash
make build
make legacy
```

## What This Demonstrates

- Characterization tests are often more reliable than legacy documentation.
- Structural completion is not behavioral completion.
- Differential testing turns undocumented edge behavior into an executable contract.
- Explicit deviations are more useful than an unqualified compatibility claim.
- Bind-before-setuid ordering, legacy config compatibility, and comparator
  correctness belong in the migration, not in a later polish phase.

## Security

The Rust port is structurally immune to all 10 historical CVE classes filed
against the C thttpd family (acme thttpd, Debian `src:thttpd`, Gentoo
`sthttpd`). The evidence and methodology live in:

- [Security Migration Report](docs/security/MIGRATION_REPORT.md) — historical CVE coverage, Rust mitigations, and the CI matrix
- [Security Notes](docs/SECURITY_NOTES.md) — the three audited `unsafe` OS-boundary crates
- [Vulnerability reporting & policy](SECURITY.md) — supported versions and response SLA

CI jobs (in `.github/workflows/`): `security` (cargo audit + deny + geiger,
every PR), `miri` (nightly), `sanitizers` (every PR, ASan), `fuzz` (nightly),
`release` (SBOM on tag). Run them locally with `make security`; see
[docs/security/RUNNING_LOCALLY.md](docs/security/RUNNING_LOCALLY.md) for Miri,
ASan, and cargo-fuzz.

## Interview Path

Start with the presenter-first walkthrough in
[docs/INTERVIEW_DEMO.md](docs/INTERVIEW_DEMO.md). It provides a 5-7 minute talk
track, live demo command, transitions, recovery lines, and links to deeper
evidence so the discussion does not turn into reading the repository aloud.

Use [`JOURNEY.md`](JOURNEY.md) for the migration case study,
[`docs/KNOWN_DEVIATIONS.md`](docs/KNOWN_DEVIATIONS.md) for honest gap tracking,
[`docs/STRANGLER_FIG.md`](docs/STRANGLER_FIG.md) for cutover mechanics, and
[`docs/AI_ASSISTANCE.md`](docs/AI_ASSISTANCE.md) for how AI output was verified.

The reusable method is documented in
[docs/REFACTOR_PLAYBOOK.md](docs/REFACTOR_PLAYBOOK.md). The five-minute project
walkthrough is in [docs/INTERVIEW_DEMO.md](docs/INTERVIEW_DEMO.md).

## Migration Tools

`thttpd-migrate` is a strangler-fig migration proxy that shifts traffic from
the C `thttpd` to the Rust `thttpd-rs` incrementally. It ships active-active
and canary routing, shadow mirroring with response diffing, active health
checks, a circuit breaker, Prometheus `/metrics`, request-id propagation, a Unix
control socket, one-command rollback, and graceful drain without modifying
either server.

- User guide & architecture: [docs/STRANGLER_FIG.md](docs/STRANGLER_FIG.md)
- Rollback runbook: [docs/ROLLBACK.md](docs/ROLLBACK.md)
- Week-by-week plan: [docs/MIGRATION_PLAYBOOK.md](docs/MIGRATION_PLAYBOOK.md)
- Control-socket protocol: [docs/CONTROL_PROTOCOL.md](docs/CONTROL_PROTOCOL.md)
- Async-runtime decision: [docs/ADR-0002-async-runtime-split.md](docs/ADR-0002-async-runtime-split.md)

## License

The original thttpd is BSD 2-Clause licensed by Jef Poskanzer. The Rust port
follows the same license; see `legacy/README.md` for the upstream notice.
