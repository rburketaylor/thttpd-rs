# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.1.x (current) | :white_check_mark: Active |
| < 0.1 | :x: End of life |

## Reporting a Vulnerability

Please report security issues via
[GitHub Security Advisories](https://github.com/rburketaylor/thttpd-rs/security/advisories/new)
(private disclosure to the maintainers).

We commit to:

- **Acknowledge** within 3 business days.
- **Provide an initial assessment** within 10 business days.
- **Coordinate disclosure timing** with the reporter.
- **Credit the reporter** in the release notes (unless anonymity is requested).

Do **not** open a public GitHub issue for a security vulnerability.

## Scope

**In scope:** the thttpd-rs server, the `thttpd-migrate` strangler-fig proxy,
and any other first-party code in this repository. For the audited `unsafe`
OS-boundary crates (`thttpd-auth`, `thttpd-core`, `thttpd-mmc`) and the runtime
mitigations that back the security claims, see
[docs/SECURITY_NOTES.md](docs/SECURITY_NOTES.md). For the full historical-CVE
analysis and Rust-side mitigations, see
[docs/security/MIGRATION_REPORT.md](docs/security/MIGRATION_REPORT.md).

**Out of scope:**

- The `legacy/` C source — that is upstream sthttpd; report it upstream
  (https://github.com/blueness/sthttpd). As of this report, all 10 historical
  CVEs against the family are still unfixed in Debian's `src:thttpd`.
- Third-party Rust dependencies — report to upstream / [RustSec](https://rustsec.org/).
  We track them with `cargo audit`; we do not vouch for them.

## Verification

The security claims in [`docs/security/MIGRATION_REPORT.md`](docs/security/MIGRATION_REPORT.md)
are each backed by a CI job that runs on every commit (`security`, `miri`,
`sanitizers`, `fuzz`) or on every release (`release` with SBOM + SLSA
provenance). To reproduce them locally:

```bash
make security                                    # audit + deny + geiger
bash pipeline/run_security_scan.sh               # same, explicit entry point
# Miri, ASan, cargo-fuzz: see docs/security/RUNNING_LOCALLY.md
```
