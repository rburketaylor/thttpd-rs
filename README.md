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

The repository currently contains **343 automated tests**:

| Layer | Tests | Purpose |
|---|---:|---|
| C-vs-Rust differential scenarios | 105 | Compare externally observable request behavior |
| Legacy C harness scenarios | 80 | Prove fixtures and scenarios against the reference server |
| Rust unit tests | 95 | Verify parser, protocol, cache, timer, auth, and configuration internals |
| Comparator unit tests | 63 | Prove that the differential oracle detects meaningful drift |

Run the complete gate with:

```bash
python3 -m pip install -r requirements-dev.txt
cargo install cargo-audit cargo-deny --locked
make verify
```

`make check` runs the fast formatting, lint, unit, comparator, and knowledge
checks. `make integration` builds both implementations and runs the C-only and
differential suites.

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
JOURNEY.md              development and repair-loop narrative
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

The reusable method is documented in
[docs/REFACTOR_PLAYBOOK.md](docs/REFACTOR_PLAYBOOK.md). The five-minute project
walkthrough is in [docs/INTERVIEW_DEMO.md](docs/INTERVIEW_DEMO.md).

## License

The original thttpd is BSD 2-Clause licensed by Jef Poskanzer. The Rust port
follows the same license; see `legacy/README.md` for the upstream notice.
