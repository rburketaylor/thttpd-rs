# Migration Journey Log

## The Discovery (2026-06-08)

Ran the validation artifact after the initial rpiv-pi pipeline (discover → research → design → plan → implement → validate). All 22 phases reported "fully implemented." But running the tests told a different story:

- **48 Rust unit tests pass** — the building blocks work in isolation
- **80 harness tests error at fixture setup** — the C binary was never compiled
- **Event loop is a skeleton** — placeholder comments where dispatch logic should be
- **Pipeline scripts print "placeholder"** — golden capture never ran

### Root Cause

The plan's success criteria were **structural** ("file exists, compiles, test collected") not **behavioral** ("server responds to GET / with 200"). The implement skill met the letter of every exit gate. The validation phase caught the gaps after the fact.

This is the classic AI-assisted implementation trap: the plan gates on what's easy to check mechanically, not on what actually matters.

### The Fix Plan

Six phases, each gated on observable behavior:

- **A:** Wire up event loop dispatch → `curl localhost:8080` returns a response
- **B:** Build C binary → `legacy/src/thttpd` serves files
- **C:** Populate harness tests → real HTTP requests, real assertions
- **D:** Implement pipeline scripts → `baseline.json` gets captured
- **E:** Differential verification → 0 failures C vs Rust
- **F:** Update README with the real story

---

## Phase B: Build C Reference Binary — ✅ Complete

autotools `make` failed on modern GCC: `sigset` is an implicit function declaration, which is now a hard error. The code is 30 years old — this is expected.

Workaround: manual gcc invocation with `-Wno-implicit-function-declaration` plus `-DHAVE_CONFIG_H -I. -I..` for the generated config. Links against `-lcrypt -lrt -lresolv`.

Updated `pipeline/build_legacy.sh` with the working command. Binary verified: `curl localhost:19998/index.html` → `Hello from C thttpd`.

**Lesson:** 30-year-old C code doesn't compile cleanly on modern GCC. The build script now documents the workaround.

## Phase C: Populate Harness Tests — ✅ Complete

Replaced all 80 `pass` stubs with real socket-level HTTP requests. No external libraries — raw `socket` module only, which lets us test malformed requests that HTTP clients would refuse to send.

Also rewrote `conftest.py`:
- Added `http_request()` and `parse_response()` helpers
- Expanded `www_root` fixture with binary files, large files, zero-length files, symlinks, subdirectories, and a full `cgi-bin/` with 8 CGI scripts
- Added port-readiness polling instead of fixed sleep
- Added `server_process_with_throttle` fixture

**Behavioral gate:** `pytest harness/tests/ -v` → **80 passed, 0 failed, 29.57s** ✅

Every test sends real bytes to the C binary, reads the response, and asserts on status/headers/body. The golden master is real.

## Phase A: Event Loop Dispatch — ✅ Complete

The subagent rewrote `eventloop.rs` from skeleton to full dispatch. Added connection table (`slab::Slab<ConnSlot>`) and listener storage to `Server`. Added `HttpConn` and `peer_addr` fields to `ConnSlot`.

Implemented the full chain:
- **handle_accept()** — Accept TCP, create ConnSlot(Reading), register with mio
- **handle_read()** — Read into HttpConn.read_buf, run got_request() FSM
- **process_request()** — Parse method/URL, normalize path, check CGI patterns, dispatch
- **serve_static()** — mmap cache, directory listing, proper headers
- **dispatch_cgi()** — Build env, execute, parse output, handle NPH
- **handle_send()** — Write response bytes, reregister for partial writes
- **handle_lingering()** — Drain socket before close (prevent RST)

**Behavioral gate:** `curl localhost:19997/index.html` → `Hello from thttpd-rs` ✅
**Regression gate:** All 48 unit tests still pass ✅

---

## Phase D: Pipeline Scripts — ✅ Complete

Implemented all three pipeline scripts:
- `run_golden_capture.py` — Starts C binary, runs 45 test cases, captures `baseline.json`
- `run_differential.py` — Replays baseline against Rust binary, 8-field diff, exit 1 on failure
- `generate_report.py` — HTML diff report with category tables

**Behavioral gate:** `run_golden_capture.py` captured 45 responses against C binary ✅

---

## Phase E: Differential Verification — 🔧 In Progress

First differential run: **2/45 passed, 43 failed**. This is the pipeline doing its job.

### Failure Categories

**1. Missing response headers (most failures)**
C returns 7 headers: `Server, Content-Type, Date, Last-Modified, Accept-Ranges, Connection, Content-Length`
Rust returns 4: `Content-Type, Content-Length, Date, Server`
Missing: `Last-Modified`, `Accept-Ranges`, `Connection: close`
Also missing `charset=iso-8859-1` in Content-Type.

**2. Missing features**
- `If-Modified-Since` → 304 Not Modified (Rust returns 200)
- `Range` requests → 206 Partial Content (Rust returns full 200)
- `HEAD` method sends body (Rust sends 69 bytes, C sends 0)
- HTTP/0.9 returns headers (C returns none, Rust returns full response)
- `Connection: close` header missing

**3. Security gaps**
- Symlink escaping: symlink to `/etc/passwd` — C returns 403, Rust serves the file (200)
- Directory traversal: C returns 404, Rust returns 403 (different error code)
- Permission denied (chmod 000): C returns 403, Rust returns 404

**4. Method handling**
- Invalid method (PROPFIND): C returns 501, Rust serves the file (200)

**5. CGI completely broken**
C's CGI output parsing differs from Rust. C appears to merge CGI headers into the HTTP response differently. NPH scripts not handled correctly.

### What Passes
- `malformed.truncated_request` — both drop connection
- `malformed.binary_garbage` — both handle gracefully

### Next Step
Use the rpiv-pi workflow to fix the failures. See **Research Prompt** below.

---

## Remaining Work After Phase E

### Phase E.5: Modernization
Once differential tests pass, polish the Rust code:
- Replace magic numbers with named constants/enums
- Add `rustdoc` examples to all public APIs
- Fix compiler warnings: `suspicious_double_ref_op` in `cgi.rs:84`, unused variable in `parse.rs:126`
- Fix `-h` flag clash: clap auto-assigns `-h` to `--help`, breaking scripts that use `-h hostname`

### Phase E.6: Test Suite Reconciliation & Knowledge Cleanup
1. The 80 pytest tests (`harness/tests/`) only test against the C binary via the `server_process` fixture. The 45 differential tests (`pipeline/`) compare C vs Rust. These should either be unified or the pytest suite should get a Rust binary mode.
2. `knowledge/_index.yaml` says all modules are `pending`. `knowledge/_migration_map.yaml` says all are `migrated`. Reconcile to reflect actual state.

### Phase F: Final README
Write the honest story. Update this journey log with final results.

---

## Research Prompt for Phase E

Run `/research` with this prompt:

```
I need to fix 43 out of 45 differential test failures between the C thttpd binary and the Rust port (thttpd-rs). The golden master baseline is captured at harness/golden/baseline.json and the differential runner is at pipeline/run_differential.py.

Research the following:

1. Read rust/crates/thttpd-core/src/eventloop.rs — this is where all the fixes go. Understand how serve_static(), process_request(), handle_send(), and dispatch_cgi() work.

2. Read the C reference source at legacy/src/libhttpd.c and legacy/src/thttpd.c — specifically how they handle:
   - Response headers (Last-Modified, Accept-Ranges, Connection, charset in Content-Type)
   - HEAD method body suppression
   - If-Modified-Since conditional responses (304)
   - Range requests (206 Partial Content)
   - HTTP/0.9 raw response format
   - Unknown method rejection (501)
   - Symlink escape prevention (checking resolved path stays within www_root)
   - Permission denied vs not found error distinction
   - CGI output parsing (Status header, NPH scripts, header/body split)
   - Error response headers (Server, Date, Cache-Control on error pages)

3. Read harness/golden/baseline.json to see exactly what the C binary returns for each of the 45 test cases.

4. Run `python3 pipeline/run_differential.py --baseline harness/golden/baseline.json` to see the current failures.

Produce a research artifact documenting the exact behavioral differences and what needs to change in each Rust function.
```

After research completes, follow with `/design` → `/plan` → `/implement` → `/validate` using the research artifact.
