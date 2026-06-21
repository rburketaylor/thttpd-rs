# Rust-side Mitigations

> For each CWE class identified in [`C_PATTERNS.md`](C_PATTERNS.md), the mechanism
> in `rust/crates/*/src/*.rs` that prevents the class. Every `file:line` was
> regenerated against the current tree and confirmed to resolve (Phase 3
> automated success criterion). The headline guarantee — `thttpd-http` is free of
> raw memory operations — is enforced by [`pipeline/audit_unsafe.sh`](../../pipeline/audit_unsafe.sh)
> (Gate 1 = deterministic grep, Gate 2 = cargo-geiger set-membership).

## CWE → Rust mechanism table

| CWE | Rust mechanism | Evidence | Confidence |
|-----|----------------|----------|------------|
| **CWE-119 / CWE-787** (Buffer Overflow / OOB Write) | Reads are bounded by `&mut [u8]` slice length; `Vec<u8>`/`String` grow on demand; the parser indexes `read_buf` only within `[0, read_idx)` — no fixed-size destination buffer to overflow | `rust/crates/thttpd-core/src/eventloop.rs:204` `stream.read(&mut http.read_buf[http.read_idx..])` (slice bounds the write); `rust/crates/thttpd-http/src/parse.rs:14-19` `got_request(read_buf, checked_idx, read_idx, …)` loops `while checked_idx < read_idx` | High |
| **CWE-125** (Out-of-bounds Read) | All indexing is bounds-checked at runtime (panics, not UB); no raw pointer arithmetic exists in the request path (Gate 1 guarantees zero `unsafe` tokens in `thttpd-http/src/`) | `rust/crates/thttpd-http/src/parse.rs:23` `let c = read_buf[checked_idx]` (panics on OOB instead of reading garbage) | High |
| **CWE-20** (Improper Input Validation) | Numeric parsing uses `FromStr` → `Result`, propagated with `?`; negative `Content-Length` is filtered to `None` instead of wrapping to `MAX_USIZE` (the JOURNEY.md §"three parser edge cases" fix) | `rust/crates/thttpd-core/src/eventloop.rs:433` `cl_str.trim().parse::<i64>().ok().filter(\|&v\| v >= 0)`; CGI `Status:` code parsed as `u16` at `:1552` `code_str.parse::<u16>()` (rejects negatives and out-of-range) | High |
| **CWE-22** (Path Traversal) | `normalize_path` returns `Option<String>`; `..` above root returns `None` via `pop()?`, eliminating the escape; no in-place `char*` rewrite to get wrong | `rust/crates/thttpd-http/src/url.rs:38` `fn normalize_path(path: &str) -> Option<String>`; `:50` `components.pop()?` (traversal above root → `None`) | High |
| **CWE-78** (OS Command Injection) | CGI launches via `std::process::Command` with separated argv and explicit env — **no shell**, so metacharacters are literal arguments, never interpreted | `rust/crates/thttpd-http/src/cgi.rs:125` `Command::new(script_path)`; `:132` `cmd.env(key, value)` (argv/env passed as values, not through `/bin/sh -c`) | High |
| **CWE-79** (Reflected XSS) | Error-page arguments are HTML-escaped (`<`/`>` → `&lt;`/`&gt;`) before interpolation, matching C's `defang()` — but in a type-safe helper the formatter cannot bypass | `rust/crates/thttpd-http/src/response.rs:189` `fn defang(s: &str)`; `:212` `let defanged = defang(arg)` applied before `form.replace("%.80s", truncated)` | High |
| **CWE-476** (NULL Pointer Dereference) | Absent values are `Option<T>`, forced to `match`/`?` at the call site; the parser returns `GotRequest::NoRequest` rather than dereferencing a sentinel | `rust/crates/thttpd-http/src/parse_state.rs:6` `pub enum GotRequest { NoRequest, GotRequest, BadRequest }`; `rust/crates/thttpd-auth/src/lib.rs:40` `auth_check2` walks `path.parent()` as `Option<&Path>` | High |
| **CWE-59 / CWE-377** (Link Following / Insecure Temp File) | No temp-file creation in the request path; log/pid paths are fixed config values, not predictable temp names; symlink policy is explicit (`no_symlink_check` opt-in) | `rust/crates/thttpd-core/src/eventloop.rs:711` `// --- Symlink escape prevention ---` (explicit policy gate); CVE-2006-4248 was in the Debian `start_thttpd` shell wrapper, not the daemon | High |
| **CWE-264** (Permissions / Privileges) | Log-file open uses the configured path with explicit intent; privilege drop happens after binding (see `docs/SECURITY_NOTES.md` "Privilege Ordering"); `initgroups` is the single audited FFI | `rust/crates/thttpd-core/src/startup.rs:70` `unsafe { libc::initgroups(…) }` (the audited boundary; documented in `SECURITY_NOTES.md`) | Medium (FFI boundary — audited, not structurally impossible) |
| **CWE-668** (Resource Exposure to Wrong Sphere) | Auth check walks the path with the typed `std::path::Path` API; there is no `char*`-rewriting boundary condition for a trailing `/` to collapse | `rust/crates/thttpd-auth/src/lib.rs:40` `fn auth_check2(path: &Path, …)`; `:42-48` the `parent()` walk is structural, not string-surgery | High |

## Confidence legend

- **High** — the CWE class is *structurally impossible* in this code: either there
  is no buffer to overflow (the type system owns the memory), or the unsafe
  operation that could reach the request path is banned by Gate 1.
- **Medium** — the class is reachable only through an audited `unsafe` FFI
  boundary (`thttpd-auth`, `thttpd-core`, `thttpd-mmc`). The boundary is small,
  documented in `docs/SECURITY_NOTES.md`, and confined to crates that do not
  touch request parsing. Miri + ASan (Phase 4) exercise the safe wrappers around
  these boundaries.
- **Low** — the class is reachable only via a *logic* bug no compile-time check
  can prevent; enumerated in [`docs/KNOWN_DEVIATIONS.md`](../KNOWN_DEVIATIONS.md).
  No row in this table is Low.

## Boundary crates (the audited `unsafe` surface)

Gate 2 of `audit_unsafe.sh` enforces that the set of thttpd-* crates containing
`unsafe` is exactly:

- **`thttpd-auth`** — the `crypt(3)` FFI for `.htpasswd` hash verification
  (`rust/crates/thttpd-auth/src/lib.rs:147,152,168`). Isolated in its own crate
  precisely so `thttpd-http` can be certified `unsafe`-free.
- **`thttpd-core`** — `initgroups(3)` for privilege drop
  (`rust/crates/thttpd-core/src/startup.rs:70`).
- **`thttpd-mmc`** — `mmap(2)` for memory-mapped file serving
  (`rust/crates/thttpd-mmc/src/lib.rs:103`).

The SIGPIPE handler in `thttpd-core/src/signal.rs` was rewritten in Phase 3 to
use the safe `signal_hook::flag::register` path, removing the fourth historical
`unsafe` site. To add or remove a boundary crate, update
`EXPECTED_BOUNDARY_CRATES` in `pipeline/audit_unsafe.sh` **and**
`docs/SECURITY_NOTES.md` in the same commit.
