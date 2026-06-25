# Security Notes

thttpd-rs is a parity-focused port, not a claim that a 1990s CGI execution model
is a modern sandbox.

For the reporting flow (who to contact, response SLA, supported versions), see
[`SECURITY.md`](../SECURITY.md). For the full historical-CVE → Rust-mitigation
report, see [`security/MIGRATION_REPORT.md`](security/MIGRATION_REPORT.md).

## Audited Unsafe Boundaries

The workspace avoids raw memory operations in request parsing and protocol
handling. Enforced by [`pipeline/audit_unsafe.sh`](../pipeline/audit_unsafe.sh)
with two gates: Gate 1 (deterministic `grep`) requires zero literal `unsafe`
tokens in `thttpd-http/src/`, and Gate 2 (cargo-geiger set-membership) requires
that the set of thttpd-* crates containing any `unsafe` usage is **exactly**
these three audited OS/FFI boundary crates:

| Crate | FFI / OS call | Location | Purpose |
|-------|---------------|----------|---------|
| `thttpd-auth` | `crypt(3)` | `rust/crates/thttpd-auth/src/lib.rs:147` | `.htpasswd` hash verification (DES/MD5/SHA) for Basic Auth |
| `thttpd-core` | `initgroups(3)` | `rust/crates/thttpd-core/src/startup.rs:70` | supplementary-group initialization during privilege drop |
| `thttpd-mmc` | `mmap(2)` | `rust/crates/thttpd-mmc/src/lib.rs:103` | memory-mapped file serving |

Each boundary documents pointer, lifetime, serialization, and return-value
assumptions next to the `unsafe` block. The `crypt(3)` boundary was extracted
into its own `thttpd-auth` crate (Phase 3 of the security plan) precisely so
`cargo-geiger` can honestly report `thttpd-http` (the request-parsing crate) as
`unsafe`-free. The historical fourth site — the SIGPIPE handler in
`thttpd-core/src/signal.rs` — was rewritten to use the safe
`signal_hook::flag::register` path, removing the `unsafe` entirely.

Transitive-dependency `unsafe` is tracked separately by `cargo-audit` /
`cargo-deny` (see `security/MIGRATION_REPORT.md`), not vouched for here.

`cargo clippy -D warnings`, `cargo audit`, `cargo deny`, and `cargo geiger`
(all via `make security`) run in the verification workflow.

## CGI Risk

The current CGI path starts a child with cleared environment variables, pipes
stdin/stdout/stderr, writes the request body, and closes stdin to avoid the
legacy deadlock discovered during differential testing.

It does not yet provide:

- execution timeouts and forced termination
- bounded stdout/stderr capture
- operating-system resource limits
- filesystem or syscall sandboxing
- runtime concurrency enforcement for parsed `cgilimit`

These are recorded as operational deviations in
[Risks](RISKS.md) rather than hidden behind the
request-parity result. A hardened deployment should use a supervisor or sandbox
until native controls are implemented and tested.

## Privilege Ordering

Startup performs chroot, binds listeners, then drops group and user privileges.
This preserves the legacy requirement that privileged ports be opened before
setuid while keeping filesystem confinement in place first.
