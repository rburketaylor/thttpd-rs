---
date: 2026-06-08T15:27:44-0300
author: Burke T
commit: no-commit
branch: no-branch
repository: thttpd-rs
topic: "thttpd C→Rust Migration Architecture & Translation Strategy"
tags: [research, codebase, migration, c-to-rust, thttpd, event-loop, cgi, mmap, timers, throttling, knowledge-system, golden-master]
status: complete
last_updated: 2026-06-08T15:27:44-0300
last_updated_by: Burke T
---

# Research: thttpd C→Rust Migration Architecture & Translation Strategy

## Research Question

How does the thttpd (sthttpd 2.27.0) C codebase map to the planned 8-crate Rust workspace? What are the critical translation decisions for the event loop (fdwatch→mio), connection struct (httpd_conn→Rust ownership), mmap cache (mmc→safe wrappers), timer system (timers→idiomatic Rust), CGI execution (fork/exec→std::process::Command), startup sequence (CLI/chroot/signals→clap/nix/signal-hook), bandwidth throttling, knowledge system, and golden master harness?

## Summary

The thttpd C codebase (~8,600 lines across 7 modules) is a single-threaded, `select()`/`poll()`-driven HTTP server with mmap-based file caching, CGI/1.1 support, and bandwidth throttling. The Rust migration targets 8 crates (`thttpd-core`, `thttpd-http`, `thttpd-fdwatch`, `thttpd-timers`, `thttpd-mmc`, `thttpd-match`, `thttpd-tdate`, `thttpd-mime`) using mio for I/O multiplexing, memmap2 for safe mmap, and zero `unsafe` blocks throughout.

The critical architectural decisions are: (1) fdwatch's polling API translates to mio's event-driven model via a `FdWatch` struct wrapping `mio::Poll` with token-based dispatch replacing `void*` client_data pointers; (2) the `httpd_conn` struct's 40+ fields — including three categories of `char*` (owned, borrowed into `read_buf`, static) — become owned `String`/`Vec<u8>` with eager parsing; (3) mmc's manual refcount + raw mmap pointer becomes `Rc<Mmap>` with automatic Drop; (4) the timer hash-of-sorted-lists becomes a `BinaryHeap<Reverse<TimerEntry>>` with `Box<dyn FnMut>` closures replacing `TimerProc` function pointers; (5) CGI's fork/exec/interposer pattern simplifies to `std::process::Command` with `Stdio::piped()`, eliminating interposer processes; (6) the startup sequence's chroot→bind→setuid ordering maps to nix crate calls with the same security-critical ordering; (7) bandwidth throttling's pause mechanism (fd del + timer) maps to mio deregister + timer-based reregister.

The golden master harness captures ≥200 C binary behavior snapshots across 9 test categories, comparing 8 response fields (status code, status text, header count, header order, header values, body SHA-256, body bytes, connection result) with strict byte-exact parity. The knowledge system provides institutional memory via YAML+MD artifacts that drive the dependency-ordered translation.

## Detailed Findings

### Event Loop: fdwatch → mio Translation

The C `fdwatch` abstraction (`legacy/src/fdwatch.h:68-93`) wraps select/poll/kqueue/devpoll with compile-time `#ifdef` selection. The main loop at `legacy/src/thttpd.c:537-609` follows the pattern: build fd set → `fdwatch(tmr_mstimeout(&tv))` → iterate ready fds → dispatch by connection state.

- **fdwatch uses fd-indexed arrays** (`fd_rw[]`, `fd_data[]` at `fdwatch.c:604-605`) for O(1) client_data lookup. The `void* client_data` at `fdwatch.c:112-120` tags each fd with a `connecttab*` pointer for connections, or `NULL` for listen sockets.
- **thttpd.c does NOT call `fdwatch_clear()`** in its main loop. Instead it incrementally adds/removes fds: add at connection accept (`thttpd.c:806`), del+add at read→write transition (`thttpd.c:897-898`), del for throttle pause (`thttpd.c:970`), add at wakeup (`thttpd.c:2160-2163`), del at close (`thttpd.c:2107`).
- **The poll backend** (`fdwatch.c:379-431`) is the most relevant for Linux. It builds a flat array of ready fd numbers (`poll_rfdidx[]`), then `fdwatch_get_next_client_data()` at `fdwatch.c:165-175` iterates this array returning the `void*` tag for each ready fd.
- **New connections get priority** at `thttpd.c:564-578`: IPv6 listen fd is checked first, then IPv4. If either accepts, the loop `continue`s, deferring existing connections to the next cycle. `handle_newconnect()` at `thttpd.c:660-813` contains an inner `for(;;)` loop that drains the accept queue completely.
- **The `fdwatch(tmr_mstimeout(&tv))` call** at `thttpd.c:546-552` is the sole integration point between the timer system and I/O multiplexer. `tmr_mstimeout()` at `timers.c:131-158` scans 67 hash buckets and returns the minimum time until next timer expiry (or `INFTIM`=-1 if no timers pending).
- **`tmr_run(&tv)` is called twice per loop iteration**: at `thttpd.c:556` (no fds ready case) and `thttpd.c:598` (after processing ready fds).

The mio translation uses token-based dispatch: `Token(0)` = LISTEN6, `Token(1)` = LISTEN4, `Token(CONN_BASE + idx)` = connection. The `FdWatch` struct internally maps tokens to client data, replacing C's fd-indexed arrays with `HashMap<Token, ClientData>`.

### Connection Struct: httpd_conn → Rust Type Hierarchy

The `httpd_conn` struct at `legacy/src/libhttpd.h:79-142` has ~40 fields with three ownership categories:

1. **Owned heap buffers** (freed in `httpd_destroy_conn` at `libhttpd.c:2415-2431`): `read_buf`, `decodedurl`, `origfilename`, `expnfilename`, `encodings`, `pathinfo`, `query`, `accept`, `accepte`, `reqhost`, `hostdir`, `remoteuser`, `response`. These become `String` or `Vec<u8>`.
2. **Pointers into `read_buf`** (no ownership): `encodedurl`, `protocol`, `referer`, `useragent`, `acceptl`, `cookie`, `contenttype`, `hdrhost`, `authorization`. These are set by pointer arithmetic during `httpd_parse_request()` at lines 1959-2114. In Rust, these become owned `String` fields (eager parsing) to avoid lifetime complexity.
3. **Static pointers**: `type` (points into mime type table), `hostname` (server hostname). These become `&'static str` or `String`.

- **The `checked_state` machine** (`libhttpd.h:147-158`) has 12 states (`CHST_FIRSTWORD` through `CHST_BOGUS`) implementing an incremental character-at-a-time FSM in `httpd_got_request()` at `libhttpd.c:1769-1925`. This becomes a Rust `enum CheckedState` with the same 12 variants. The FSM preserves the incremental-read pattern: `handle_read()` calls `got_request()` repeatedly as data arrives.
- **State transitions** trace a strict sequence: FIRSTWORD → FIRSTWS → SECONDWORD → SECONDWS → THIRDWORD → THIRDWS/LF/CR → LINE → LF/CR → CRLF → CRLFCR → GOT_REQUEST. HTTP/0.9 detection happens at CHST_SECONDWORD when `\n`/`\r` is seen (returning `GR_GOT_REQUEST` immediately).
- **The `checked_idx` field** serves double duty: it checkpoints the FSM in `got_request()`, then gets reset to 0 at `libhttpd.c:1935` for line-at-a-time header parsing in `httpd_parse_request()`. The POST body bytes after headers (`read_buf[checked_idx..read_idx]`) must be preserved for CGI interposer input.

### CGI Execution: fork/exec → std::process::Command

The CGI call chain is: `handle_read()` → `httpd_start_request()` (`libhttpd.c:3851`) → `really_start_request()` (`libhttpd.c:3590`) → `cgi()` (`libhttpd.c:3543`) → `cgi_child()` (`libhttpd.c:3322`).

- **CGI detection** at `libhttpd.c:3780-3784`: `hs->cgi_pattern != NULL && (sb.st_mode & S_IXOTH) && match(hs->cgi_pattern, expnfilename)`.
- **`make_envp()` at `libhttpd.c:3002-3081`** builds 25+ env vars in strict order. The order matters for legacy CGI scripts. Key variables: `PATH`, `SERVER_SOFTWARE`, `SERVER_NAME`, `GATEWAY_INTERFACE=CGI/1.1`, `REQUEST_METHOD`, `PATH_INFO`, `SCRIPT_NAME`, `QUERY_STRING`, `REMOTE_ADDR`, `HTTP_*` headers, `CONTENT_LENGTH`, `CONTENT_TYPE`, `REMOTE_USER`, `AUTH_TYPE=Basic`.
- **POST stdin interposer** at `libhttpd.c:3371-3412`: The C code forks an interposer process to pipe buffered POST data + remaining body to CGI stdin. In Rust, `Stdio::piped()` + parent writing to `ChildStdin` directly eliminates the interposer — a significant simplification that preserves identical behavior.
- **NPH detection** at `libhttpd.c:3419`: If script basename starts with `"nph-"`, output goes directly to client socket. Otherwise, `cgi_interpose_output()` at `libhttpd.c:3191-3296` parses the script's headers and prepends the correct HTTP status line.
- **Two-stage CGI kill**: `cgi_kill()` at `libhttpd.c:2567-2596` sends SIGINT, then schedules `cgi_kill2()` 5 seconds later to send SIGKILL if the process hasn't exited. This chain maps to timer callbacks in Rust.
- **`cgi_limit`/`cgi_count`** at `libhttpd.h:86-87` track concurrent CGI processes. Incremented in `cgi()` at `libhttpd.c:3555`, decremented in SIGCHLD handler at `thttpd.c:222-227`. Must be `AtomicI32` in Rust since SIGCHLD handler runs in signal context.
- **`ls()` directory listing** at `libhttpd.c:2628-2955` reuses the same `cgi_count`/`cgi_limit` mechanism despite not being CGI — it forks a child process that writes HTML. Must be preserved.

### Mmap Cache: mmc → Safe Rust

The `mmc.c` module implements a reference-counted, mmap-based file cache.

- **Data structures** at `mmc.c:55-74`: A `Map` struct with `ino`, `dev`, `size`, `ctime`, `refcount`, `reftime`, `addr` (raw `void*`), plus a hash table + singly-linked list for lookup and iteration.
- **`mmc_map()` at `mmc.c:113-203`**: Hash lookup by (ino, dev, size, ctime) → if found, increment refcount and return existing pointer. Otherwise, open file, mmap it, insert into hash table + linked list.
- **`mmc_unmap()` at `mmc.c:205-228`**: Only decrements refcount and updates reftime. Does NOT call munmap. Actual unmapping happens in `mmc_cleanup()`.
- **`mmc_cleanup()` at `mmc.c:230-264`**: Called every `OCCASIONAL_TIME` (120s) from `occasional()` timer at `thttpd.c:2131`. Walks linked list, unmaps entries where `refcount==0 && age >= expire_age`. Adaptive expiry adjusts `expire_age` based on memory pressure.
- **Safe Rust translation**: `memmap2::Mmap` wraps mmap/munmap. `Rc<Mmap>` replaces manual refcount — `Rc::strong_count()` indicates whether connections still hold references. `HashMap<FileKey, CacheEntry>` replaces the dual hash table + linked list. `HashMap::retain()` replaces the cleanup walk. Zero `unsafe` achieved because all system-level operations go through safe wrappers.
- **The `file_address` field** at `libhttpd.h:141` becomes `Option<Rc<Mmap>>` on the connection struct. Pointer arithmetic at `thttpd.c:1726` becomes safe slice access `&mmap[start..end]`.

### Timer System: timers → Idiomatic Rust

The C timer package at `legacy/src/timers.h:52-97` uses a hash table of 67 sorted doubly-linked lists.

- **`tmr_create()`** at `timers.c:180-220`: Allocates a `Timer`, computes trigger time = now + msecs, inserts into sorted position in the appropriate hash bucket.
- **`tmr_mstimeout()`** at `timers.c:240-273`: Scans first entry of each bucket (sorted, so first is soonest), returns minimum remaining time. Returns `INFTIM` (-1) if no timers.
- **`tmr_run()`** at `timers.c:277-310`: Walks all buckets, fires expired timers, reschedules periodic ones.
- **`ClientData` union** at `timers.h:42-46`: Holds `void*`/`int`/`long`. Used as `connecttab*` for connection timers, `pid_t` for CGI kill timers, or `JunkClientData` (unused) for periodic system timers.
- **`TimerProc` function pointer** at `timers.h:52-53`: Every callback receives `ClientData` + `struct timeval*`. Used for 8 distinct callbacks across thttpd.c and libhttpd.c.
- **Rust translation**: `BinaryHeap<Reverse<TimerEntry>>` replaces the hash-of-lists. `Box<dyn FnMut(&mut TimerCtx)>` closures replace function pointers + ClientData union — closure capture provides type-safe context. `Instant` replaces `struct timeval`. `TimerId` tokens replace raw `Timer*` pointers in `connecttab`. Lazy cancellation (set flag, skip on pop) replaces immediate unlink.
- **Periodic system timers** (4 total): `occasional` (120s), `idle` (5s), `update_throttles` (2s), `show_stats` (3600s) — all use `JunkClientData`, so closures need no captured data.
- **Per-connection timers** (2 per connection max): `wakeup_timer` (throttle pause) and `linger_timer` (500ms close timeout) — closures capture connection index.

### Startup Sequence & Signals

The C startup at `legacy/src/thttpd.c` follows a security-critical ordering.

- **CLI parsing** at `thttpd.c:295-416`: 10+ flags including `-p`, `-d`, `-r`, `-u`, `-l`, `-T`, `-c`, `-v`, `-P`, `-nor`/`-nov`/`-noP` negations. Plus config file reader at `thttpd.c:419-569`. Maps to `clap` derive struct with `#[group(multiple = false)]` for mutually exclusive flags.
- **Hostname resolution** at `thttpd.c:614-685`: Must happen before chroot (line 193 comment). Uses `getaddrinfo()` with `AI_PASSIVE`. Maps to `std::net::ToSocketAddrs`.
- **Security sequence** — **chroot→bind→setuid ordering is critical**: `chroot()` at `thttpd.c:234` (requires root), then `httpd_initialize()` at `thttpd.c:374` (binds listen socket, may need root for ports <1024), then `setgroups()`+`setgid()`+`initgroups()`+`setuid()` at `thttpd.c:308-327` (irreversible privilege drop). Maps to `nix::unistd::chroot()`, `nix::unistd::setuid()`, etc.
- **Daemonization** at `thttpd.c:260-285`: Single fork + `setsid()` (NOT the double-fork pattern). Maps to manual `nix::unistd::fork()` + `setsid()`.
- **Signal handlers** at `thttpd.c:346-372`: 8 signals (SIGTERM, SIGINT, SIGCHLD, SIGPIPE=IGN, SIGHUP, SIGUSR1, SIGUSR2, SIGALRM). Maps to `signal_hook::iterator::Signals` with mio registration — signals become just another mio event via the self-pipe trick. Flags like `got_hup`, `got_usr1` become `AtomicBool`.

### Bandwidth Throttling

The throttle system at `legacy/src/thttpd.c` uses a pattern-based rate limiter.

- **`throttletab` struct** at `thttpd.c:90-97`: Array of (pattern, max_limit, min_limit, rate, bytes_since_avg, num_sending) entries. Maps to `Vec<ThrottleEntry>` with `String` patterns.
- **Throttle file parsing** at `thttpd.c:688-770`: Reads lines in format `"pattern min-max"` or `"pattern max"`. Leading slashes stripped from patterns at lines 728-730 because match() is called against `expnfilename` (filesystem path after chroot, no leading `/`).
- **`check_throttles()`** at `thttpd.c:1316-1358`: Linear scan matching each pattern against the URL. Per-connection `max_limit = throttle.max_limit / throttle.num_sending` (fair sharing). Rejects if rate > 2× max_limit or rate < min_limit. Up to `MAXTHROTTLENUMS=10` patterns per connection.
- **Rolling average** at `thttpd.c:1375-1407`: `(2 * old_rate + bytes_since_avg / THROTTLE_TIME) / 3` — integer math, must replicate truncation exactly. Updated every 2 seconds via timer callback.
- **Pause mechanism** at `thttpd.c:1245-1267`: When send rate exceeds limit, `fdwatch_del_fd()` removes the fd from the poll set, and a one-shot timer re-registers it after the calculated delay. In mio: `registry.deregister()` + timer + `registry.register()`.
- **`CGI_BYTECOUNT=25000`** at `thttpd.h:335`: All CGI responses counted as 25KB for throttling regardless of actual output. Set at `libhttpd.c:3572`: `hc->bytes_sent = CGI_BYTECOUNT`.

### Knowledge System & Golden Master Pipeline

- **YAML schema** (PLAN.md §0.2): `functions:` list with `signature`, `callers`, `callees`, `complexity`, `notes`, `migration_target`. Plus `gotchas:` for undocumented behaviors.
- **6 concrete gotchas** identified in the C source: (1) Negative Content-Length via `atol()` at `libhttpd.c:2191` causing unsigned wraparound; (2) CGI env var order dependency at `libhttpd.c:3008-3082`; (3) `post_post_garbage_hack` at `libhttpd.c:3177-3193` for broken browsers; (4) Linux close-on-exec bug with `dup()` at `libhttpd.c:3330-3376`; (5) Throttle counter underflow at `thttpd.c:1899-1902`; (6) `CGI_BYTECOUNT=25000` constant at `thttpd.h:335`.
- **`_migration_map.yaml`** tracker: Status progression `pending → analyzed → translating → compiled → verified → modernized` gates phase transitions.
- **`validate_knowledge.py`**: CI enforcement of YAML schema consistency.
- **Golden master baseline format** (PLAN.md §2.3): JSON with `request` (method, path, headers, body) + `response` (status_code, status_text, headers, header_order, body_sha256, body_bytes, connection_result). Header order captured separately from values.
- **Diff engine** (PLAN.md §4.2): 8 strict checks — status_code, status_text, header_count, header_order, header_values, body_sha256, body_length, connection_result.
- **Repair loop** (PLAN.md §4.3): Classify mismatch → trace to Rust code path → feed diff to repair agent → patch → recompile → retest. Max 5 cycles per mismatch, then human escalation. Every repair captured in `knowledge/learnings/`.

## Code References

- `legacy/src/thttpd.c:537-609` — Main event loop (fdwatch + timer + dispatch)
- `legacy/src/thttpd.c:660-813` — `handle_newconnect()` with inner accept loop
- `legacy/src/thttpd.c:295-416` — CLI argument parsing
- `legacy/src/thttpd.c:234-257` — Chroot sequence
- `legacy/src/thttpd.c:308-327` — Privilege drop (setgroups/setgid/setuid)
- `legacy/src/thttpd.c:346-372` — Signal handler installation
- `legacy/src/thttpd.c:1316-1358` — `check_throttles()` per-connection throttle matching
- `legacy/src/thttpd.c:1375-1407` — `update_throttles()` rolling average calculation
- `legacy/src/thttpd.c:1245-1267` — Throttle pause mechanism (fd del + timer)
- `legacy/src/thttpd.c:599-608` — Graceful shutdown (SIGUSR1 → remove listen fds)
- `legacy/src/libhttpd.h:79-142` — `httpd_conn` struct definition (40+ fields)
- `legacy/src/libhttpd.h:147-158` — `checked_state` enum constants (12 states)
- `legacy/src/libhttpd.c:1769-1925` — `httpd_got_request()` incremental FSM parser
- `legacy/src/libhttpd.c:1929-2370` — `httpd_parse_request()` header parsing + URL resolution
- `legacy/src/libhttpd.c:3543-3580` — `cgi()` with limit check, fork, timer setup
- `legacy/src/libhttpd.c:3322-3540` — `cgi_child()` env setup, pipe setup, execve
- `legacy/src/libhttpd.c:3002-3081` — `make_envp()` CGI environment construction
- `legacy/src/libhttpd.c:3191-3296` — `cgi_interpose_output()` NPH header parsing
- `legacy/src/libhttpd.c:3126-3161` — `cgi_interpose_input()` POST body piping
- `legacy/src/libhttpd.c:2567-2596` — `cgi_kill()`/`cgi_kill2()` two-stage process kill
- `legacy/src/fdwatch.h:68-93` — fdwatch API surface
- `legacy/src/fdwatch.c:379-431` — poll backend (most relevant for Linux)
- `legacy/src/timers.h:52-97` — Timer API surface
- `legacy/src/timers.c:131-158` — `tmr_mstimeout()` timeout calculation
- `legacy/src/timers.c:180-220` — `tmr_create()` timer allocation and insertion
- `legacy/src/timers.c:277-310` — `tmr_run()` expired timer execution
- `legacy/src/mmc.h:34-53` — MMC API surface
- `legacy/src/mmc.c:113-203` — `mmc_map()` acquire mapping
- `legacy/src/mmc.c:205-228` — `mmc_unmap()` release reference
- `legacy/src/mmc.c:230-264` — `mmc_cleanup()` eviction engine
- `legacy/src/mmc.c:55-74` — Map struct and cache globals
- `legacy/src/thttpd.h:335` — `CGI_BYTECOUNT` constant (25000)
- `legacy/src/thttpd.h:360` — `THROTTLE_TIME` constant (2 seconds)
- `legacy/src/thttpd.h:365` — `SPARE_FDS` constant (10)
- `legacy/src/thttpd.h:370` — `MAXTHROTTLENUMS` constant (10)

## Integration Points

### Inbound References

- `PLAN.md` — 6-phase migration plan defining the entire pipeline structure, crate layout, and verification strategy
- `EXECUTION_PLAN.md` — Subagent execution plan with Groups A-H, dependency graph, parallel execution timeline
- `migration_path.md` — Original discussion establishing golden master + differential testing approach

### Outbound Dependencies

- `mio` — I/O multiplexing (replaces fdwatch's select/poll abstraction). Used via `Poll`, `Events`, `Token`, `Interest`, `Registry`.
- `memmap2` — Safe mmap wrapper (replaces raw `mmap()`/`munmap()` calls in mmc.c). Returns `Mmap` with `Deref<Target=[u8]>`.
- `clap` — CLI argument parsing (replaces hand-rolled `parse_args()` at `thttpd.c:295-416`).
- `nix` — chroot/setuid/setgid/setsid/fork (replaces direct system calls at `thttpd.c:234-327`).
- `signal-hook` — Signal handling (replaces `signal()`/`sigaction()` at `thttpd.c:346-372`). Provides `Signals` iterator with self-pipe for mio integration.
- `thiserror` — Error type derivation for all crate error types.

### Infrastructure Wiring

- `thttpd-core` depends on: `thttpd-http`, `thttpd-fdwatch`, `thttpd-timers`, `thttpd-mmc`, `thttpd-match`
- `thttpd-http` depends on: `thttpd-match`, `thttpd-tdate`, `thttpd-mmc`
- `thttpd-timers` depends on: nothing (leaf crate)
- `thttpd-fdwatch` depends on: `mio`
- `thttpd-mmc` depends on: `memmap2`
- Translation order: leaf crates (match, tdate, fdwatch) → infrastructure (timers, mmc) → core (libhttpd/http, thttpd/core)
- CI pipeline (PLAN.md §5.4): 5 jobs — `build-legacy`, `build-rust`, `unit-tests`, `differential-tests`, `knowledge-consistency`

## Architecture Insights

1. **Token-based dispatch replaces pointer-based tagging**: C's `void* client_data` → Rust's `Token(usize)` + connection table index. This is the fundamental architectural shift — all event dispatch changes from pointer dereference to array index lookup.

2. **Eager parsing simplifies ownership**: Converting all `char*` fields (including those pointing into `read_buf`) to owned `String` adds one allocation per header but eliminates lifetime complexity across the entire connection struct.

3. **Interposer process elimination**: C's CGI uses fork-based interposer processes for POST body piping and header parsing. Rust's `std::process::Command` with `Stdio::piped()` handles this directly — the parent writes to `ChildStdin` and reads from `ChildStdout` without extra forks. This is a behavioral simplification that must be verified via golden master testing.

4. **`Rc::strong_count()` replaces manual refcount**: The mmc cache uses `Rc<Mmap>` where `strong_count() == 1` means only the cache holds it (evictable). This is the exact semantic of `refcount == 0` in C but off by one (cache always holds one reference).

5. **Lazy timer cancellation**: Replacing C's immediate timer unlink with a `cancelled` flag + skip-on-pop avoids O(n) heap search while accumulating small heap bloat. For thttpd's modest timer count (dozens), this is acceptable.

6. **Integer arithmetic parity**: Throttle rate calculation uses integer division `(2 * rate + bytes / 2) / 3` which truncates differently than floating-point. Rust must replicate the exact integer arithmetic to pass differential testing.

7. **Header order matters for parity**: The C server builds response headers via sequential `add_response()` calls producing a fixed order: Date, Server, Last-Modified, Content-Type, Content-Length, Expires, P3P, Connection. Rust must use `Vec<(String, String)>` not `HashMap<String, String>` for headers.

## Precedents & Lessons

git history unavailable — no commit history in this repository.

### Composite Lessons

- The 6 concrete gotchas (negative Content-Length, CGI env order, post_post_garbage_hack, Linux close-on-exec bug, throttle underflow, CGI_BYTECOUNT) are the highest-risk translation points — each has been captured for the knowledge system and must have corresponding golden master test cases.
- The security-critical chroot→bind→setuid ordering at `thttpd.c:234-327` must be preserved exactly; reordering would create a vulnerability.
- The `ls()` function at `libhttpd.c:2628-2955` reuses `cgi_count` for directory listing — this non-obvious coupling must be preserved.

## Historical Context (from `.rpiv/artifacts/`)

- `.rpiv/artifacts/discover/2026-06-08_15-18-22_thttpd-rust-migration.md` — Feature Requirements Document for the thttpd migration

## Developer Context

**Q (discover: Portfolio showcase intent): What's the driving motivation behind migrating thttpd to Rust?**
A: Portfolio / showcase — building a demonstrably rigorous, well-documented migration artifact to showcase engineering capability

**Q (discover: Definition of done): Your plan calls for byte-exact parity, structured knowledge system, golden master harness, differential testing, and full modernization. Which definition of "done" fits the showcase?**
A: Full pipeline as planned — byte-exact parity, full knowledge system, golden master, differential testing, modernization pass

**Q (discover: Platform target): What platform(s) must the Rust binary support?**
A: Linux-only

**Q (discover: Dependency philosophy): Which dependency philosophy fits the showcase — standard crate selections, minimal dependencies, or custom?**
A: Standard crate selections (mio, thiserror, signal-hook, nix, clap)

**Q (discover: Knowledge system scope): Does the YAML+MD knowledge system with schema enforcement earn enough showcase value to justify the setup cost?**
A: Full YAML+MD system with schema enforcement and CI validation

**Q (discover: Rust edition): Which Rust edition should the workspace target?**
A: Rust 2024 edition, stable channel

**Q (discover: CI strategy): Should the repo include a working CI pipeline, or is local harness sufficient?**
A: GitHub Actions with full pipeline: build-legacy, build-rust, unit-tests, differential-tests, knowledge-consistency

**Q (discover: CGI support level): How much CGI support needs to work in the Rust port?**
A: Full CGI support — CGI/1.1 including NPH scripts, environment variable passing, POST body piping, exact fork/exec behavior

**Q (discover: CLI flag compatibility): Should the Rust binary accept the same command-line flags as the C thttpd binary?**
A: Full CLI compatibility — identical flags and behavior, drop-in replacement

**Q (discover: Unsafe code policy): What's the policy on `unsafe` Rust code given mmap, raw fds, fork/exec, and signal handling?**
A: Zero unsafe — no `unsafe` blocks in the entire workspace

**Q (discover: C source layout): The full sthttpd source lives in `legacy/` as a plain git clone. Keep this layout?**
A: Keep `legacy/` as a plain git clone

**Q (discover: Extras and CGI programs scope): Should `extras/` utilities and `www/cgi-bin/` programs be in scope?**
A: Out of scope for initial migration, deferred

**Q (discover: No work started): No Rust code, harness, knowledge system, or pipeline scripts exist yet?**
A: Confirmed — the entire 6-phase pipeline is to be executed

## Related Research

- None yet — this is the first research artifact for this migration.

## Open Questions

- **Extras translation scope**: Whether `extras/` utilities (htpasswd, makeweb, syslogtocern) should be translated in a future iteration. The developer deferred this decision. The CGI programs in `www/cgi-bin/` (phf.c, redirect.c, ssi.c) are likewise deferred.
