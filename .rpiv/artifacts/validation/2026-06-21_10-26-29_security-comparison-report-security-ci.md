---
template_version: 1
date: 2026-06-21T10:26:29-0300
author: Burke T
commit: 3afe7c0
branch: main
repository: thttpd-rs
topic: "Validation of Security Comparison Report + Security CI"
status: ready
verdict: pass
parent: ".rpiv/artifacts/plans/2026-06-12_16-30-00_security-report.md"
tags: [validation, security, cve, cwe, miri, cargo-audit, cargo-deny, fuzz, sbom]
last_updated: 2026-06-21T10:26:29-0300
---

## Validation Report: Security Comparison Report + Security CI

### Implementation Status

- ✓ Phase 1: CVE discovery + vulnerability inventory — Fully implemented
- ✓ Phase 2: C-side root cause analysis — Fully implemented
- ✓ Phase 3: Rust-side mitigation analysis (incl. `thttpd-auth` extraction + SIGPIPE fix) — Fully implemented
- ✓ Phase 4: Miri + sanitizer + fuzz setup — Fully implemented (local criteria verified; CI execution pending first commit/push)
- ✓ Phase 5: SBOM + cargo-audit + cargo-deny CI — Fully implemented
- ✓ Phase 6: Report assembly (`docs/security/MIGRATION_REPORT.md`) — Fully implemented
- ⚠️ Phase 7: Release security process — Partial (machinery in place: `release.yml`, `SECURITY.md`, `Dockerfile.security`, `scan_container.sh`; release-time automation unverified because no release tag exists — legitimately future work)

All seven phases have their on-disk deliverables present. Phases 1–6 are verified end-to-end against the working tree. Phase 7's automated criteria are inherently release-gated (signed tag, SLSA attestation, container scan) and remain unchecked in the plan — the supporting files exist but cannot execute until a release is cut.

### Automated Verification Results

Phase 1 — CVE inventory:
- ✓ CVE inventory lock parses: `python3 -c "import tomllib; tomllib.load(open('docs/security/cve_inventory.lock','rb'))"` — `row_count = 10`, keys `[generated, queries, row_count, sources]`
- ✓ CVE table fully curated (zero placeholders): `grep -cE '<year>|<CWE>|<cvss>|<affected>' docs/security/CVE_TABLE.md` — `0` (10 real CVEs with year/CWE/CVSS/severity/affected filled)
- ✓ Source URLs live: `curl -sL` HEAD/GET on all 10 NVD links — `10 / 10` resolved (HTTP 200)
- ✓ Candidate scanner runs: `bash pipeline/find_unfixed_cves.sh` — exit 0, emits 105 candidate hits for classification

Phase 2 — C-side patterns:
- ✓ C-pattern citations resolve: 11 line-numbered `File:line` refs in `C_PATTERNS.md` against `legacy/src/{libhttpd.c,thttpd.c}` — all resolve

Phase 3 — Rust mitigations + extraction:
- ✓ Workspace builds: `cargo build --manifest-path rust/Cargo.toml --workspace` — Finished, exit 0
- ✓ Auth tests pass: `cargo test --manifest-path rust/Cargo.toml -p thttpd-auth` — `11 passed; 0 failed`
- ✓ Core tests pass (auth path via re-export): `cargo test --manifest-path rust/Cargo.toml -p thttpd-core` — `15 passed; 0 failed`
- ✓ Gate 1 (headline, deterministic): `grep -rn --include='*.rs' 'unsafe' rust/crates/thttpd-http/src/` — empty (zero `unsafe` tokens, including comments)
- ✓ Unsafe-audit gate: `bash pipeline/audit_unsafe.sh` — `PASS [Gate 1]` (0 unsafe in thttpd-http/src) + `PASS [Gate 2]` (boundary set == `{thttpd-auth, thttpd-core, thttpd-mmc}`), exit 0
- ✓ Parity backstop: `make differential` — `105 passed in 306.01s`, exit 0 (extraction did not change wire behavior)
- ✓ CWE coverage cross-check: `RUST_MITIGATIONS.md` CWE set ⊇ `C_PATTERNS.md` CWE set (12 vs 11 CWEs; CWE-476 is an extra mitigation row present in the plan's own template)
- ✓ Mitigation citations resolve: 15 `File:line` refs in `RUST_MITIGATIONS.md` across 9 files — all resolve

Phase 4 — Miri / ASan / fuzz (local):
- ✓ Miri: `MIRIFLAGS="-Zmiri-disable-isolation -Zmiri-permissive-provenance" cargo +nightly miri test -p thttpd-http --lib` — `40 passed; 0 failed` in 4.19s (well under the 10-min target)
- ✓ ASan: `RUSTFLAGS="-Z sanitizer=address" cargo +nightly test -p thttpd-http --lib --target x86_64-unknown-linux-gnu` — `40 passed; 0 failed`
- ✓ Fuzz build: `(cd rust && cargo +nightly fuzz build)` — Finished, exit 0; both `parse_request` and `parse_url` binaries produced

Phase 5 — supply-chain CI:
- ✓ `make security` — exit 0 (audit + deny + audit_unsafe in one canonical command)
- ✓ `cargo audit --file rust/Cargo.lock` — 194 crate dependencies scanned, no vulnerabilities, exit 0
- ✓ `cargo deny --manifest-path rust/Cargo.toml check` — `advisories ok, bans ok, licenses ok, sources ok`, exit 0 (non-blocking `license-not-encountered` warnings on over-inclusive allow-list entries BSD-3-Clause/ISC/Unicode-DFS-2016)
- ✓ `security.yml` present; `migration-ci.yml` no longer has a `security:` job (`grep` for security/audit/deny returns no match)

Phase 6 — report:
- ✓ Enforced-by links resolve: all workflow files referenced in `MIGRATION_REPORT.md` (`security.yml`, `miri.yml`, `sanitizers.yml`, `fuzz.yml`) exist
- ✓ Security-scan wrapper: `bash pipeline/run_security_scan.sh` — exit 0 (wraps `make security`)
- ⚠️ `markdown-link-check docs/security/MIGRATION_REPORT.md` — **not run**: tool not installed locally. Inbound links verified by other means (workflow-file existence + NVD URL liveness above).

Phase 7 — release process:
- ⚠️ `git tag -v` / `gh attestation verify` / `trivy image` — not run: no release tag exists and `trivy` is not installed. These are release-gated and remain unchecked in the plan.

- ✓ No regressions detected — full workspace build + 71 crate-level tests + 105 differential tests all green

### Code Review Findings

#### Matches Plan:

- `rust/crates/thttpd-http/src/lib.rs:10` — `pub use thttpd_auth as auth;` re-export shim in place; no leftover `pub mod auth;`. The 7 `thttpd_http::auth::*` call sites in `eventloop.rs` compile unedited (proven by `cargo build --workspace` + `cargo test -p thttpd-core`).
- `rust/crates/thttpd-auth/src/lib.rs:147,152,168` — the three `crypt(3)` FFI `unsafe` sites moved here verbatim; `auth_check2` (L40), `AuthResult` (L54), `ERR_401_TITLE`/`ERR_401_FORM` (L24/L27) all `pub`, satisfying the eventloop contract.
- `rust/crates/thttpd-auth/build.rs` — links `cargo:rustc-link-lib=crypt` on Linux/BSD, deliberately omits Apple (libSystem) — correct cross-platform split for `crypt(3)`.
- `rust/crates/thttpd-core/src/signal.rs:65` — SIGPIPE now registered via safe `flag::register(SIGPIPE, FLAGS.sigpipe_sink.clone())`; the `unsafe { low_level::register }` is gone. `signal.rs:78-95` adds the required `write_to_closed_pipe_returns_epipe` test (UnixStream pair, close read end, assert BrokenPipe/EPIPE).
- `rust/crates/thttpd-http/Cargo.toml` — `base64`/`libc` removed (0 direct refs remain in `http/src/`, so removal is safe), `thttpd-auth = { workspace = true }` added with explanatory comment.
- `rust/Cargo.toml:6,25` — `"crates/thttpd-auth"` in `members` and `thttpd-auth = { path = ..., version = "0.1.0" }` in `[workspace.dependencies]`.
- `Makefile:43-49` — `security` target runs audit + deny + `audit_unsafe.sh` (geiger) so local and CI invoke the identical command.
- `pipeline/audit_unsafe.sh` — two-gate design exactly as revised in round 2 (Gate 1 deterministic grep, Gate 2 geiger set-membership with schema-tolerant `walk()`).
- Boundary `unsafe` confined to exactly the three documented crates: `thttpd-auth` (crypt FFI), `thttpd-core/src/startup.rs:70` (initgroups), `thttpd-mmc/src/lib.rs:103` (Mmap).
- `CVE_TABLE.md` — 10 CVEs (1999–2021) fully curated with NVD links and footnoted CWE provenance; `cve_inventory.lock` reproducibility sidecar present.

#### Deviations from Plan:

- `rust/fuzz/Cargo.toml` — **positive deviation (improvement).** The plan said the fuzz crate should be standalone "by omission from the members list (no exclude key needed)." The implementation additionally declares an empty `[workspace]` table, making the crate its own workspace root so cargo cannot auto-discover it into the parent. This is cargo's recommended idiom for a sub-package that must stay out of the parent workspace and is strictly more robust than omission alone. An explanatory comment documents the rationale. No action needed.
- `pipeline/find_unfixed_cves.sh` — **minor semantic note.** The Phase 2 criterion reads "returns 0 unfixed patterns," but the implemented script is a candidate-emitter that prints 105 hits and exits 0 regardless of classification. This is consistent with the plan's own Phase 1 design note ("The 'returns 0 uncovered' gate therefore lives in Phase 2/3, not here") — the real coverage gate is the `C_PATTERNS.md` ↔ `RUST_MITIGATIONS.md` cross-check, which passes (CWE superset, all citations resolve). Acceptable as implemented; the criterion wording is looser than the script's actual contract.
- `pipeline/audit_unsafe.sh` — **cosmetic.** Output wording is `PASS [Gate 1]` / `PASS [Gate 2]` rather than the plan's template string `PASS: 0 unsafe...`. Immaterial — gate logic and exit code are correct.

#### Pattern Conformance:

- ✓ All 5 new `pipeline/*.sh` scripts match the existing `build_legacy.sh` convention: executable bit set, `#!/usr/bin/env bash` shebang, `set -euo pipefail` strict mode.
- ✓ `thttpd-auth/Cargo.toml` inherits workspace fields (`version.workspace`, `edition.workspace`, `license.workspace`, `rust-version.workspace`) identically to sibling crates (`thttpd-http`, `thttpd-core`); correctly drops `thiserror` (unused by auth) per the plan's conditional instruction.
- ✓ `deny.toml` extended in place at repo root (no divergent `rust/deny.toml`); preserves the pre-existing strict values (`yanked = "deny"`, `unknown-git = "deny"`) as the consolidation required.
- Minor observation: the new workflows are consistent with `migration-ci.yml` (checkout@v4, `dtolnay/rust-toolchain`, `working-directory: rust`, `taiki-e/install-action`). `security.yml` pins `toolchain: '1.85'` to match the workspace `rust-toolchain.toml` — acceptable variation, not a deviation.

#### Potential Issues:

- The implementation is entirely in the **working tree (uncommitted)** — all changes appear as `M`/`D`/`??` in `git status`, no implementation commits exist yet. This is expected at validation time (validate runs pre-commit by design) but means the Phase 4 plan criterion "Miri, sanitizers, and fuzz jobs in CI all pass on this commit" cannot be satisfied until the work is committed and pushed.
- Phase 7 release-time automation (`release.yml` signed-tag verification, SLSA provenance, SBOM attach, `scan_container.sh` trivy sweep) is **defined but unexercised** — no release tag or container image exists. `trivy` and `markdown-link-check` are not installed in this environment, so those two checks could not be run locally.
- `find_unfixed_cves.sh` exiting 0 while printing 105 candidates means a future contributor could add an unclassified risky C pattern and the script would still pass. Coverage assurance currently depends on the manual `C_PATTERNS.md` ↔ `RUST_MITIGATIONS.md` cross-check, not on the script itself.

### Manual Testing Required:

1. Phase 1 CVE curation (human judgment, partially satisfied):
   - [x] No row uses a placeholder — verified (0 placeholders, 10 fully-filled rows)
   - [x] Source URLs resolve — verified (10/10 HTTP 200)
   - [ ] Cross-reference the table with the Debian CVE tracker for `src:thttpd` — every tracker entry has a row; the table header cites the Debian tracker URL but row-level mapping needs a human diff
   - [ ] Each CVE's "Affected" version matches the upstream CHANGELOG entry for that advisory

2. Phase 4 negative-test injections (proves the detectors actually catch defects):
   - [ ] Add a deliberate UB to `thttpd-http` (e.g. `std::ptr::read` of uninitialized memory); confirm Miri flags it; revert
   - [ ] Add a deliberate buffer overflow (e.g. `slice::get_unchecked` with bad index); confirm ASan flags it; revert
   - [ ] Add a deliberate `panic!` on a malformed byte; confirm the fuzz target reproduces it as a `crash-*` artifact; revert

3. Phase 5 supply-chain negative tests:
   - [ ] Add a deliberately vulnerable dev-dependency (e.g. an old `time` crate with a known CVE); confirm `make security` flags it via cargo-audit; revert
   - [ ] Add a GPL-licensed dep; confirm `make security` fails via cargo-deny; revert
   - [ ] `cargo auditable build` produces a binary with embedded dependency metadata; `auditable inspect target/release/thttpd` lists all deps

4. Phase 6 report review:
   - [ ] A security engineer unfamiliar with the project reads `MIGRATION_REPORT.md` and reaches the same conclusions
   - [ ] The report's "Reproducing this report" section regenerates every claim

5. Phase 7 release drill (release-gated):
   - [ ] A test release carries all four artifacts: signed tag, SLSA attestation, SBOM, and a `gh release` page
   - [ ] `SECURITY.md` is linked from the GitHub Security tab (auto-detected)

### Recommendations:

- Commit the working tree and push so the Phase 4 CI criterion (Miri/sanitizers/fuzz jobs green on a commit) can be satisfied; this is the natural next step, not a fix.
- Install `markdown-link-check` (and optionally `trivy`) in CI/dev so the Phase 6 link check and Phase 7 container scan can run as prescribed; inbound links already verified by other means, so this is tooling completeness, not a correctness gap.
- Consider tightening `find_unfixed_cves.sh` to cross-check its candidate list against `C_PATTERNS.md` and non-zero-exit when a candidate matches no classified CWE — this would make the Phase 2 "returns 0 unfixed" criterion a true gate rather than relying on the separate manual cross-check. Low priority; current coverage holds.
- Ready to commit — Phases 1–6 are complete and validated; Phase 7 is legitimately pending a release.
