# Releasing thttpd-rs

This is the operational release runbook. A release is cut from `main` by
creating a signed annotated tag; the `release.yml` workflow builds the binary
with embedded dependency metadata, generates a CycloneDX SBOM, and attaches
both to the GitHub release.

## Security Sign-off (required before release)

Every release must pass these checks. The CI jobs run continuously; the links
below should point to the most recent green run on the release commit.

- [ ] `bash pipeline/run_security_scan.sh` exits 0 (audit + deny + geiger)
- [ ] `miri` job passed in the last 7 days: <link>
- [ ] `fuzz` job passed in the last 7 days: <link>
- [ ] `sanitizers` job passed in the last 7 days: <link>
- [ ] [`docs/security/MIGRATION_REPORT.md`](security/MIGRATION_REPORT.md)
      "Last verified" column updated to the release commit
- [ ] SBOM generated and attached to the release (automated by `release.yml`)
- [ ] GitHub release uses a **signed** tag (see below)

## Cutting a release

```bash
# 1. On main, at the release commit:
git tag -s -a v0.1.0 -m "Release v0.1.0"   # GPG-signed annotated tag
git push origin v0.1.0

# 2. release.yml runs automatically on the tag push:
#    - verifies the tag signature
#    - builds with cargo-auditable (embedded dep metadata)
#    - generates a CycloneDX SBOM
#    - generates SLSA build-provenance attestation
#    - attaches binary + SHA256SUMS + SBOM to the GitHub release
```

The release operator creates the tag locally (or through protected release
tooling) — the workflow never creates or pushes the tag it is triggered by.

## Verifying a release artifact

```bash
# Verify the GPG signature on the tag.
git tag -v v0.1.0

# Verify the SLSA build-provenance attestation on the binary.
gh attestation verify -o rburketaylor thttpd --artifact-path dist/thttpd

# Verify the embedded dependency SBOM.
cargo install cargo-auditable --locked
auditable inspect target/release/thttpd
```

See [`docs/security/MIGRATION_REPORT.md`](security/MIGRATION_REPORT.md) §2 for
the full list of verifiable security claims and the CI job backing each.
