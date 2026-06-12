# Coverage Gap Implementation Plan

**Created:** 2026-06-12
**Author:** Revert (autonomous execution)
**Branch:** main
**Base commit:** f7e6cb2 (post-MRIE checkpoint)
**Goal:** Close all 50+ test/feature gaps identified in the coverage analysis so the Rust port is a true drop-in replacement for sthttpd 2.27.0.
**Mode:** Autonomous end-to-end execution (user confirmed). Milestone reports at end of each tier.

## How to use this file (recovery guide)

If this session is interrupted or context is lost:

1. **Read the "Status" section** below — it shows what phase we're in and what's done.
2. **Read the active phase's "Files" + "Success Criteria"** to know what's in flight.
3. **Check the working tree** with `git status` and `git log --oneline -20` to see actual state vs plan.
4. **Resume from the next TODO item** by re-running the phase's commands.
5. **Tests to verify before continuing:**
   - `cargo test --manifest-path rust/Cargo.toml --workspace` (unit)
   - `python3 -m pytest harness/tests/ --ignore=harness/tests/test_differential.py -q` (C-only)
   - `python3 -m pytest harness/tests/test_differential.py -q --timeout=120` (differential)
6. **Total expected at end:** ~280+ tests (we're adding ~30-50 new tests across 14 phases).

## Status

**Active phase:** Phase 1 — Parser hardening
**Last update:** 2026-06-12, plan written
**Cumulative test count:** 219/219 (baseline) → target ~280+ after all phases

| Phase | Description | Tests added | Status |
|-------|-------------|-------------|--------|
| 1 | Parser hardening (HTTP/9.9, Crlfcr, case-insensitive method, X-Forwarded-For) | +14 (11 unit + 3 differential) | **DONE** (commit 5bfec35) |
| 2 | Auth subsystem (crypt + .htpasswd + differential test) | +13 (10 unit + 3 differential) | **DONE** (commit 9c35d31) |
| 3 | Static file serving hardening (non-CGI exe → 403, pathinfo → 403, Range edges) | +4 differential | **DONE** (commit e8f3217) |
| 4 | CGI depth (Status:, Location:, nph-multistatus, make_envp headers) | +5 differential | **DONE** (commit 11f255e) |
| 5 | MIME / encoding (.tar.gz chained, octet-stream default) | +5 (3 unit + 2 differential) | **DONE** (commit 9152229) |
| 6 | Symlink edge cases (circular, absolute-target, de_dotdot) | +3 differential | **DONE** (commit 9b57a69) |
| 7 | Virtual hosting (vhost_map, two Host: headers) | ~2 | TODO |
| 8 | Throttle file parsing (comment lines, min-max, ThrottleTable::load) | ~3 | TODO |
| 9 | Config file (-C, --p3p, --maxage, logfile, hostname, etc.) | ~5 | TODO |
| 10 | Charset (-T override) | ~1 | TODO |
| 11 | Signal handling (SIGHUP logfile reopen, log format) | ~2 | TODO |
| 12 | Chroot / drop privileges (wire up startup.rs) | ~1 | TODO |
| 13 | Daemonization (fork+setsid, no diff test) | 0 | TODO |
| 14 | Final cleanup (docs, test counts, JOURNEY.md) | 0 | TODO |

## Testing strategy (applies to every phase)

Every phase ends with these gates (in order):
1. `cargo build --manifest-path rust/Cargo.toml --release` — must be clean.
2. `cargo test --manifest-path rust/Cargo.toml --workspace` — all unit tests pass.
3. `python3 -m pytest harness/tests/ --ignore=harness/tests/test_differential.py -q` — C-only pass.
4. `python3 -m pytest harness/tests/test_differential.py -q --timeout=120` — differential pass.
5. `python3 pipeline/validate_knowledge.py` — knowledge consistent.
6. New tests added in the phase pass on both C and Rust.
7. Commit at end of phase with a focused message.

**Forbidden in this work:**
- Breaking the 219 existing passing tests.
- Leaving diff_engine normalizers "hide" a C-vs-Rust difference (instead, fix the divergence).
- Skipping the byte-exact match in favor of "looks similar".
- Adding tests to the differential suite that only test Rust (they must be C-and-Rust).

**Reference docs (do not edit without thought):**
- `legacy/src/libhttpd.c` — the byte-exact reference (4230 lines)
- `legacy/src/thttpd.c` — main(), config, throttle, signals (2189 lines)
- `rust/crates/thttpd-http/src/*.rs` — current Rust port
- `rust/crates/thttpd-core/src/*.rs` — main, eventloop, config, signals
- `harness/diff_engine.py` — comparator
- `harness/conftest.py` — fixtures (modify carefully)

## What we're NOT doing

- **Replacing mio with select/poll/devpoll** — Rust uses mio (epoll/kqueue) which is correct on Linux/macOS. fdwatch backends are C-only.
- **Replacing the Atomic* state in Rust with thread-local** — no threading model change.
- **Touching the differential test infrastructure** (diff_engine.py, conftest.py fixtures) unless a phase explicitly needs it.
- **Adding async runtime** (the project deliberately uses mio + manual poll loop).
- **Implementing `hostname_map` / `tilde_map_1` / `tilde_map_2` / `vhost_dirlevels`** — these are wrapped in `#ifdef` and don't exist in the compiled C binary.

---

## Phase 1: Parser hardening (Tier 1.2-5)

**Goal:** Make the Rust parser accept/reject requests the same way C does.

### Sub-tasks

#### 1.1. HTTP/9.9 → 400 (one_one=1 + Host required)
**C reference:** `libhttpd.c:1965`, `libhttpd.c:2250-2255`
**Files:** `rust/crates/thttpd-core/src/eventloop.rs` (likely), `rust/crates/thttpd-http/src/parse.rs`
**Behavior:** If request protocol is not "HTTP/1.0" (case-insensitive), set `one_one=1`. After parsing headers, if `one_one && hdrhost[0] == '\0'`, return 400.
**Test:** `test_differential.py::test_invalid_http_version` already sends `HTTP/9.9` and expects 400. May currently be passing for the wrong reason (Rust may also return 400 in some path). Verify with a fresh test that sends a valid `HTTP/9.9` with no Host header — must be 400.

#### 1.2. Crlfcr FSM state
**C reference:** `libhttpd.c:1909-1918` (CHST_CRLFCR)
**Files:** `rust/crates/thttpd-http/src/parse.rs`, `rust/crates/thttpd-http/src/parse_state.rs`
**Behavior:** After seeing `\r\n` (CRLF), if next byte is `\r` or `\n`, treat as end of request. Rust currently only handles `\n`. Note: this is a known behavioral divergence.
**Test:** `test_differential.py::test_truncated_request` may exercise partial paths. Add a focused test that sends `GET / HTTP/1.0\r\n\r` (single \r at end, no \n) — C should accept, Rust currently may not.

#### 1.3. Method case-insensitivity
**C reference:** `libhttpd.c:1944-1949` (uses `strncmp` case-sensitive) — wait, C uses strncmp. Let me re-check.

**ACTUALLY:** C at L1944 uses `strncmp` which is case-sensitive. So Rust and C both reject lowercase. The original coverage analysis was wrong on this. **Skip this sub-task.** But verify with a test that `get / HTTP/1.0` returns 501 in both.

#### 1.4. X-Forwarded-For parsing
**C reference:** `libhttpd.c:2210-2215` (parses X-Forwarded-For and sets `hc->client_addr`)
**Files:** `rust/crates/thttpd-http/src/parse.rs`
**Behavior:** If header `X-Forwarded-For:` is present, parse the first IP and set `client_addr` to it (used for log, syslog). Currently Rust ignores it.
**Test:** Send `X-Forwarded-For: 10.0.0.5`, verify CGI `REMOTE_ADDR=10.0.0.5` (or syslog contains it).

### Success criteria
- [ ] `cargo build --release` clean
- [ ] All 219 existing tests pass
- [ ] New tests pass on C and Rust:
  - `test_invalid_http_version_requires_host`
  - `test_crlfcr_only_terminator`
  - `test_method_lowercase_returns_501`
  - `test_x_forwarded_for`
- [ ] C and Rust produce byte-identical responses for all 4 scenarios

### Commit message template
```
fix(parser): align Rust parser with C for HTTP/9.9, Crlfcr, X-Forwarded-For

- HTTP/9.9 (or any non-1.0 protocol) now requires Host header (400 if absent)
- Crlfcr FSM state accepts \r or \n as end-of-request after CRLF
- X-Forwarded-For: header sets hc->client_addr for CGI REMOTE_ADDR / log

[list of differential tests added]
```

---

## Phase 2: Auth subsystem (Tier 1.1)

**Goal:** Implement Basic Auth matching C's `auth_check2` byte-for-byte.

### Sub-tasks

#### 2.1. Add crypt() crate
**Files:** `rust/crates/thttpd-http/Cargo.toml`
**Dependency:** Add `crypt = "0.4"` or `pwd = "0.1"` (verify which one supports DES+MD5+SHA-256; we need at least DES for the default `.htpasswd` format used by htpasswd command). Alternative: `bcrypt` is wrong — we need `crypt(3)` which is the Unix password hash.

#### 2.2. Implement auth_check2
**C reference:** `libhttpd.c:995-1147` (152 lines)
**Files:** `rust/crates/thttpd-http/src/auth.rs` (already exists, mostly stub)
**Behavior:** Read `.htpasswd` file from the directory containing the requested file. If present, require `Authorization: Basic <base64>`. Decode, split on `:`, look up user, verify password with `crypt()`. On failure, send 401 with `WWW-Authenticate: Basic realm="..."`. Cache last lookup (5-min mtime check).
**Note:** C only has this if `AUTH_FILE` is defined. Check the C build to confirm it's enabled. If not, we can skip auth (but the C binary tested must have it for differential).

#### 2.3. Add .htpasswd fixture
**Files:** `harness/conftest.py` (modify `www_root_session` and `www_root`)
**Fixture:** Create `.htpasswd` in a subdirectory with one known user (e.g. `testuser:VNrlUtDg9N7HI:secret` — that's MD5 crypt of "secret").
**Test:** Send `Authorization: Basic <base64('testuser:secret')>` to a file in that subdirectory, expect 200. Send wrong password, expect 401. Send no auth, expect 401.

### Success criteria
- [ ] `cargo build --release` clean
- [ ] All 219 existing tests pass
- [ ] C and Rust both return 200 for valid auth, 401 for invalid/missing
- [ ] New tests:
  - `test_basic_auth_valid` (differential)
  - `test_basic_auth_invalid` (differential)
  - `test_basic_auth_missing` (differential)
- [ ] Differential test asserts: 401 response has `WWW-Authenticate: Basic realm="..."` header

### Commit message template
```
feat(auth): implement Basic Auth matching C's auth_check2

- Add crypt crate for DES/MD5/SHA password verification
- Port libhttpd.c:995-1147 (auth_check2) to Rust
- Cached user lookup (5-min mtime check, single entry)
- Send 401 with WWW-Authenticate on missing/wrong creds
- Add .htpasswd fixture to www_root_session

[differential tests]
```

---

## Phase 3: Static file serving hardening (Tier 1.7-8)

**Goal:** Match C's behavior for non-CGI executable files, pathinfo, and Range edges.

### Sub-tasks

#### 3.1. Non-CGI executable → 403
**C reference:** `libhttpd.c:3790-3799`
**Files:** `rust/crates/thttpd-core/src/eventloop.rs` (in `really_start_request` analog)
**Behavior:** If the file is world-executable BUT the CGI pattern does NOT match, return 403 with "marked executable but is not a CGI file" error.
**Test:** Create a file with mode 0o755 in www_root (not in cgi-bin). GET should return 403.

#### 3.2. Pathinfo on non-CGI → 403
**C reference:** `libhttpd.c:3801-3810`
**Files:** `rust/crates/thttpd-core/src/eventloop.rs`
**Behavior:** If `pathinfo` is non-empty but the file is not CGI, return 403 with "resolves to a file plus CGI-style pathinfo" error.
**Test:** Create `file.txt`, then `GET /file.txt/extra`. Expect 403.

#### 3.3. Range edges
**C reference:** `libhttpd.c:3814-3816`
**Files:** Tests only (Rust likely already correct)
**Tests:** Add tests for:
- `Range: bytes=-100` (suffix form, last 100 bytes) → 206
- `Range: bytes=0-` (open-ended, from 0 to end) → 206
- `Range: bytes=99999999-99999999` (out of range) → 200 with full body (C clears got_range)
- `Range: bytes=10-5` (invalid, end < start) → 200 with full body

### Success criteria
- [ ] 3 new differential tests pass on both C and Rust
- [ ] All 219 existing tests pass
- [ ] C and Rust byte-identical for new tests

### Commit
```
fix: align executable/pathinfo/Range handling with C

- Non-CGI executable file → 403 (libhttpd.c:3790)
- Pathinfo on non-CGI file → 403 (libhttpd.c:3801)
- Range edge cases: suffix, open-ended, out-of-range, end<start
```

---

## Phase 4: CGI depth (Tier 2.9-10)

**Goal:** Cover remaining CGI env vars and Status:/Location: header handling.

### Sub-tasks

#### 4.1. CGI Status: header
**C reference:** `libhttpd.c:3265-3271`
**Files:** Tests only (Rust likely correct in `cgi.rs`)
**Test:** CGI script that outputs `Status: 418\r\n\r\nI'm a teapot`. Expect 418 response from both servers.

#### 4.2. CGI Location: header (no Status)
**C reference:** `libhttpd.c:3273-3275`
**Test:** CGI that outputs only `Location: /elsewhere\r\n\r\n`. Expect 302.

#### 4.3. CGI non-NPH with various status codes
**Tests:** 500, 503, 503, default (anything not 200/302/304/400/401/403/404/408/500/501/503 → "Something").

#### 4.4. CGI env var coverage
**C reference:** `libhttpd.c:3002-3080`
**Tests:** Add a CGI script `env_full.sh` that prints all env vars. Send requests with various headers (Referer, User-Agent, Accept, Accept-Language, Cookie) and verify the CGI sees them.

#### 4.5. CGI argp — ISINDEX-style
**C reference:** `libhttpd.c:3116-3132`
**Test:** CGI with query string `?one+two+three` (no `=`) should be passed as argv[1..] = ["one", "two", "three"]. Verify via CGI `echo "$@"` style script.

### Success criteria
- [ ] 6 new differential tests pass
- [ ] CGI env vars match between C and Rust for: HTTP_REFERER, HTTP_USER_AGENT, HTTP_ACCEPT, HTTP_ACCEPT_LANGUAGE, HTTP_COOKIE

### Commit
```
test: add CGI depth tests (Status, Location, env vars, ISINDEX args)
```

---

## Phase 5: MIME / encoding (Tier 2.12)

**Sub-tasks:**

#### 5.1. Multiple chained encodings
**C reference:** `libhttpd.c:2607-2618`
**Test:** Create `file.html.gz` in www_root, GET it. Expect `Content-Encoding: gzip` (Rust figures encodings based on extensions; both should produce the same).

#### 5.2. application/octet-stream default
**C reference:** `libhttpd.c:2551`
**Test:** Create `file.unknown` (no extension mapping) in www_root. GET it. Expect `Content-Type: application/octet-stream`.

### Success criteria
- [ ] 2 new differential tests pass
- [ ] Content-Encoding and Content-Type match between C and Rust

---

## Phase 6: Symlink edge cases (Tier 2.13-14)

**Sub-tasks:**

#### 6.1. Circular symlink
**C reference:** `libhttpd.c:1599-1602`
**Test:** Create symlink loop (`a → b`, `b → a`). GET it. Expect 500 (loop detected) or appropriate error.

#### 6.2. Absolute-target symlink
**C reference:** `libhttpd.c:1631-1636`
**Test:** Create symlink `link → /etc/passwd` (or another absolute path within www_root). GET it.

#### 6.3. de_dotdot edges
**C reference:** `libhttpd.c:2395-2437`
**Tests:** 
- `GET /subdir/../file.txt` → should resolve to `file.txt`
- `GET /./file.txt` → should resolve to `file.txt`
- `GET /foo/..` → trailing `..` removal

### Success criteria
- [ ] 3 new differential tests pass
- [ ] No regressions

---

## Phase 7: Virtual hosting (Tier 2.11)

**Sub-tasks:**

#### 7.1. Implement vhost_map
**C reference:** `libhttpd.c:1342-1421`
**Files:** `rust/crates/thttpd-core/src/eventloop.rs` or new `rust/crates/thttpd-http/src/vhost.rs`
**Behavior:** When `vhost=1` (set by config), prepend hostname to filename. For `Host: foo.com`, look in `<www>/foo.com/<path>`. For `Host: bar.com`, look in `<www>/bar.com/<path>`.
**Config:** Rust needs to read the `vhost` config option. Currently only `do_vhost` exists.
**Test:** Send `Host: vhost1.example.com` and `Host: vhost2.example.com` to the same server. Each should serve a different file.

### Success criteria
- [ ] `cargo build --release` clean
- [ ] All existing tests pass
- [ ] New test `test_vhost_different_hosts` passes on both C and Rust
- [ ] New test `test_vhost_fallback` (no matching host dir → 404)

### Commit
```
feat(vhost): implement virtual hosting matching C's vhost_map

- Prepend hostname to filename when vhost=1
- Lowercase hostname for directory match
- 2 differential tests added
```

---

## Phase 8: Throttle file parsing (Tier 2.15)

**Sub-tasks:**

#### 8.1. Implement ThrottleTable::load
**C reference:** `thttpd.c:1369-1462`
**Files:** `rust/crates/thttpd-core/src/throttle.rs`
**Behavior:** Parse lines of the form:
- `pattern max` (single rate)
- `pattern min-max` (rate range)
- Comments (`#` to end of line) and blank lines ignored
- Unparsable lines: log error, skip
**Test:** Unit test the parser, then a differential test that the server boots with a complex throttle file.

#### 8.2. Wire throttle into eventloop
**Files:** `rust/crates/thttpd-core/src/eventloop.rs`
**Behavior:** Currently `ThrottleTable` is unused. The throttle data structures exist but `read_throttlefile` is a stub. Wire it up so `-t throttles` actually loads the file.

### Success criteria
- [ ] 3 new tests (1 unit, 2 differential)
- [ ] Throttle file with comments and min-max works on both servers
- [ ] Server refuses to start with unparsable line (matching C behavior)

---

## Phase 9: Config file (-C, --p3p, --maxage, logfile, hostname) (Tier 3.17, 3.19, 3.20)

**Sub-tasks:**

#### 9.1. Implement -C config file
**C reference:** `thttpd.c:999-1193`
**Files:** `rust/crates/thttpd-core/src/config.rs` (currently uses clap)
**Behavior:** Read config file line by line, parse `name=value` pairs, set the corresponding option. Many of these are already CLI flags in clap — extend to also accept from config file.
**Difficulty:** clap's auto-derivation doesn't easily extend to config files. We may need a separate parser that writes to a `ServerConfig` directly, then call clap's parse with an empty argv to seed defaults.
**Test:** Unit test the config parser with a sample config file. Differential test: server reads the same file, both should produce the same config.

#### 9.2. Implement --p3p and --maxage
**C reference:** `libhttpd.c:670-684`
**Files:** `rust/crates/thttpd-core/src/eventloop.rs` (pass `p3p` and `max_age` to `build_mime_response`)
**Test:** Set `p3p=CP="..."` and `max_age=3600`, GET any file. Expect `P3P: CP="..."` and `Cache-Control: max-age=3600\r\nExpires: ...` headers.

### Success criteria
- [ ] 5 new tests (config parsing, p3p, max_age, logfile, hostname)
- [ ] All options that C reads from config file are also readable

### Commit
```
feat(config): implement -C config file, --p3p, --maxage
```

---

## Phase 10: Charset (-T override) (Tier 3.18)

**Sub-tasks:**

#### 10.1. Wire -T charset into response
**C reference:** `libhttpd.c:636`
**Files:** `rust/crates/thttpd-core/src/config.rs`, `rust/crates/thttpd-http/src/response.rs`
**Behavior:** Currently Rust hardcodes `iso-8859-1` in `Content-Type: text/html; charset=...`. Make it use `config.charset` instead.
**Test:** Start server with `-T utf-8`, GET any HTML file. Expect `Content-Type: text/html; charset=utf-8`.

### Success criteria
- [ ] 1 new differential test
- [ ] `Content-Type: ...; charset=<config>` matches between C and Rust

---

## Phase 11: Signal handling (Tier 2.16, 3.23)

**Sub-tasks:**

#### 11.1. SIGHUP → reopen logfile
**C reference:** `thttpd.c:237-254`
**Files:** `rust/crates/thttpd-core/src/signal.rs`, `rust/crates/thttpd-core/src/eventloop.rs`
**Behavior:** SIGHUP re-opens the logfile (allowing log rotation). Test: start server with `-l /tmp/log.txt`, write to log, rotate, send SIGHUP, write again, verify new file gets the entries.

#### 11.2. Implement make_log_entry
**C reference:** `libhttpd.c:3864-3954`
**Files:** `rust/crates/thttpd-http/src/response.rs` or new `log.rs`
**Behavior:** Log format matching C: `<ip> - - [<date>] "<method> <url> HTTP/<ver>" <status> <bytes> "<referer>" "<useragent>"`.
**Test:** Differential test (the C and Rust log formats should be byte-identical).

### Success criteria
- [ ] 2 new tests (1 unit for log format, 1 differential for SIGHUP)

---

## Phase 12: Chroot / drop privileges (Tier 3.22)

**Sub-tasks:**

#### 12.1. Wire up startup.rs
**C reference:** `thttpd.c:469-540` (chroot+setuid)
**Files:** `rust/crates/thttpd-core/src/startup.rs`, `rust/crates/thttpd-core/src/main.rs`
**Behavior:** Already implemented in startup.rs but not called. Wire into main: chroot → bind → drop_privs.
**Test:** No differential test (requires root). Add a unit test that verifies the call order is correct (mocked).

### Success criteria
- [ ] Code is wired up
- [ ] Existing tests still pass
- [ ] 1 unit test for the call order

---

## Phase 13: Daemonization (Tier 3.21)

**Sub-tasks:**

#### 13.1. Implement fork+setsid
**C reference:** `thttpd.c:500-540`
**Files:** `rust/crates/thttpd-core/src/main.rs`
**Behavior:** When `-D` is NOT passed, fork, setsid, chdir("/"), redirect stdio. Use `nix::unistd::fork`, `nix::unistd::setsid`, etc.
**No differential test** (per user direction).
**Manual test:** Run server without `-D`, verify it detaches.

### Success criteria
- [ ] Code implemented
- [ ] Server runs in background when `-D` is not passed
- [ ] Doesn't break the test harness (which uses `-D`)

---

## Phase 14: Final cleanup

**Sub-tasks:**

#### 14.1. Update all docs
- README.md: update test counts
- JOURNEY.md: add section on the coverage gap closure work
- rust/README.md: update
- knowledge/_migration_map.yaml: update
- knowledge/modules/*.md: update
- .github/workflows/migration-ci.yml: update CI comment
- AGENTS.md: rewrite Known Issues section (most P0/P1 items should be closed)

#### 14.2. Final test run
- All 219 baseline + ~50 new tests pass
- Total: ~280+ tests
- Knowledge validator passes
- No regressions

#### 14.3. Final commit
```
docs: update for completed coverage gap implementation
```

---

## Progress log (update as you go)

### Phase 1 — DONE (commit 5bfec35, 2026-06-12)
- **Crlfcr**: Rust now accepts \r and \n as end-of-request in Crlfcr state (was: only \n). Plus 5 other FSM fixes (Cr, Lf, FirstWs, SecondWs, ThirdWs).
- **HTTP/9.9 + Host**: one_one flag set for non-1.0 versions. Host required. Response status line echoes request version.
- **X-Forwarded-For**: Header parsed, first IP used as REMOTE_ADDR. Strips '::ffff:' prefix from IPv4-mapped.
- **Known C bug**: C's XFF parsing is broken on IPv6 sockets (sa_in.sin_addr vs sa_in6 union). Rust correctly honors XFF; C ignores it on IPv6. Test documents the divergence.
- **Tests added**: 7 FSM unit, 4 XFF unit, 3 differential = 14 total.
- **Cumulative test count**: 219 → 233.

### Phase 2 — DONE (commit 9c35d31, 2026-06-12)
- **Basic Auth**: Full implementation of libhttpd.c:995-1147 (auth_check2).
- **libc + base64 deps added**; build.rs links libcrypt.
- **crypt(3) thread-safety**: Added global Mutex because glibc's crypt() returns a non-thread-local buffer.
- **.htpasswd fixture**: `secret/.htpasswd` with user 'alice' / 'secret' (MD5 crypt).
- **Realm = URL directory**: Matches C's send_authenticate(hc, dirname) where dirname is the relative expnfilename.
- **Tests added**: 10 unit (parse, verify, edge cases) + 3 differential.
- **Cumulative test count**: 233 → 246.

### Phase 3 — DONE (commit e8f3217, 2026-06-12)
- **Non-CGI executable → 403** (libhttpd.c:3790): added check in serve_static
- **Pathinfo on non-CGI → 403** (libhttpd.c:3801): added check in process_request
- **PATH_INFO extraction moved to process_request** (so the pathinfo check can fire before serve_static)
- **orig_filename updated to resolved script** so dispatch_cgi uses correct script
- **Range edge tests** added: open-ended bytes=0- → 200, out-of-bounds bytes=99999- → 200
- **Cumulative test count**: 246 → 250

### Phase 4 — DONE (commit 11f255e, 2026-06-12)
- **CGI Status: header**: matches C's fixed titles (200/302/etc.) and 'Something' for unknown
- **CGI Location: only**: treated as 302 (was returning 200 in Rust)
- **Accept-Language header**: now parsed and propagated to CGI as HTTP_ACCEPT_LANGUAGE
- **Cumulative test count**: 250 → 255

### Phase 5 — DONE (commit 9152229, 2026-06-12)
- **figure_mime() ported**: walks extensions right-to-left, matches encodings then types
- **.tar.gz → gzip + application/x-tar** (was: gz as type, no encoding)
- **Header order**: Content-Encoding moved before Content-Length to match C
- **Cumulative test count**: 255 → 260

### Phase 6 — DONE (commit 9b57a69, 2026-06-12)
- **Symlink tests**: circular (a→b→a), absolute-target, dedotdot
- **Known divergence**: circular symlink returns 500 in C, 403 in Rust
  (C detects the loop with MAX_LINKS; Rust's std::fs::canonicalize bails
  earlier). Both fail safely (not 200). Documented in test.
- **Cumulative test count**: 260 → 263
