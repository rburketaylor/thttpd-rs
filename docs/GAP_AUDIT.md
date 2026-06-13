# thttpd-rs — Gap Audit & Interview Readiness

**Date:** 2026-06-13 · **Commit:** df455f1 · **Auditor:** automated review of repo state vs. FRD acceptance criteria and C reference

---

## TL;DR

The Rust port is **genuinely impressive where it works** — 105/105 differential tests pass C-vs-Rust byte-for-byte, the HTTP parsing FSM, CGI dispatch, mmap cache, `.htpasswd` Basic Auth, vhost, chroot/setuid, and symlink-escape prevention are all real. The methodology (golden master → differential testing → repair loop) is exactly the kind of story that lands in an interview.

But "fully validated byte-exact drop-in replacement" **overstates** what's there. Five categories of gap remain, and several headline claims in the README/FRD are **factually wrong against the current tree**. Fixing the claims is cheap; fixing the runtime gaps is the real work.

> The single most important thing to understand before the interview: **bandwidth throttling — the "tt" in thttpd — is parsed but not enforced at runtime.** This is the project's defining feature and it's the gap most likely to draw a sharp question.

---

## Part 1 — Gaps to "full drop-in replacement"

### 🔴 CRITICAL (breaks the drop-in claim outright)

#### 1. Bandwidth throttling is not enforced at runtime
- **What's there:** `thttpd-core/src/throttle.rs` fully implements throttlefile parsing, fair-share math, and rolling-average (all unit-tested). `ConnState::Pausing` exists in the enum.
- **What's missing:** The event loop **never calls** `check_throttles()`, never loads the throttle table from `config.throttle_file`, and never transitions a connection to `Pausing`. `grep` for `throttle`/`rate`/`pause` in `eventloop.rs` returns zero hits outside comments.
- **Why tests don't catch it:** The 4 throttle tests (`test_throttle_rate_limiting`, `test_throttle_fair_share`, etc.) only assert matching status codes and that the full body arrives — they do **not** assert timing or throughput. Both servers pass trivially because both return the whole file; the C server throttles, the Rust server doesn't, but the test never measures the difference.
- **Files:** `rust/crates/thttpd-core/src/throttle.rs` (ready), `rust/crates/thttpd-core/src/eventloop.rs` (needs wiring), `rust/crates/thttpd-http/src/conn.rs` (`Pausing` unused).
- **Interview risk:** HIGH. Anyone who reads the project name and the "throttling HTTP server" framing will ask "show me a throttled transfer." Right now there's nothing to show.

#### 2. chroot → setuid → bind ordering bug (privileged ports)
- **What's wrong:** `main.rs` runs `do_chroot → drop_privileges (setgid/setuid) → server::new → eventloop::run`, and **binding happens inside `eventloop::run` (`eventloop.rs:32`)** — i.e., **after** privileges are dropped. The C binary binds **before** setuid (see `legacy/src/thttpd.c:637`, comment: *"so that we can bind to a privileged port"*).
- **Consequence:** Running as root with `-u nobody -p 80` (the canonical thttpd deployment) **fails** — the setuid'd process can't bind port 80. Tests never exercise this because they all bind high ports (>1024) as a non-root user.
- **The irony:** The source comment in `startup.rs` literally documents the correct order: *"Security-critical ordering: chroot → bind → setuid."* The code does the opposite.
- **Fix:** Bind in `main.rs` right after chroot and before `drop_privileges`; pass the bound listener into the server/event loop.
- **Files:** `rust/crates/thttpd-core/src/main.rs`, `rust/crates/thttpd-core/src/startup.rs`, `rust/crates/thttpd-core/src/eventloop.rs`.

#### 3. Config-file parsing (`-C configfile`) is a stub
- **What's wrong:** `-C` is declared in the CLI struct and `config_file` flows into `from_cli`, but **`from_cli` never opens or parses it**. Every directive (`port=`, `dir=`, `cgipat=`, `throttles=`, …) is silently ignored.
- **Why it matters:** The man page leads with `-C configfile` as the primary invocation. Most real deployments are config-file-driven. A drop-in user who runs `thttpd-rs -C /etc/thttpd.conf` gets default behavior with no warning.
- **Fix:** Implement `read_config()` mirroring `legacy/src/thttpd.c:1001-1175` (name=value parser; ~20 directives, all already enumerated in the C source).
- **Files:** `rust/crates/thttpd-core/src/config.rs` (add `read_config`, merge over CLI).

---

### 🟠 HIGH (advertised features that are no-ops)

#### 4. Daemonization is not implemented
- `config.daemonize` is set (`!cli.debug`), but there is **no `fork()`/`setsid()`/`daemon()`** anywhere. The binary always runs in the foreground regardless of `-D`. The C binary daemonizes by default (foreground only with `-D`).
- **Files:** `rust/crates/thttpd-core/src/main.rs`.

#### 5. Request logging is a stub
- `eventloop.rs:96-97`: SIGHUP handling prints `eprintln!("thttpd: SIGHUP — would reopen logfile {:?}")`. There is **no request logging at all** — no `log_request`, no access log, no syslog. The C server logs every request (syslog by default, or a `-l` logfile). A drop-in replacement produces zero observability.
- **Note:** `no_log` (C's `-nl`) concept doesn't exist either.
- **Files:** `rust/crates/thttpd-core/src/eventloop.rs` (add a log line per served request), new `logging.rs`.

#### 6. pidfile is never written
- `-i` is accepted and stored, but no code writes the PID to the file. `init.d`/systemd wrappers that read the pidfile will break.
- **Files:** `rust/crates/thttpd-core/src/main.rs`.

---

### 🟡 MEDIUM (CLI flag mismatches — break exact `argv` compatibility)

thttpd's value proposition is "swap the binary, nothing else changes." These short flags won't parse:

| C flag | Rust status | Impact |
|--------|-------------|--------|
| `-h host` | ❌ Rust uses `-H`/`--hostname` (clap reserves `-h` for `--help`) | Wrong flag; `-h` gives help instead of setting host |
| `-P P3P` | ❌ Only `--p3p` long form exists | Short flag rejected |
| `-V` (version) | ⚠️ clap gives `--version`, `-V` not wired | May differ |
| `-g` / `-nog` (global passwd) | ❌ Rust uses `--noP` only | Short flags rejected |
| `-dd data_dir` | ❌ Missing entirely | Data-dir feature unavailable |
| `-s` / `-nos` (symlinks) | ❌ Not exposed (symlink-escape prevention is hardcoded on) | Can't disable symlink checks |

**Plus:** `--cgi-limit` is a non-standard Rust-only flag (C has `cgilimit` only in the config file).

#### 7. IPv6 not supported
- C binds both IPv4 and IPv6 sockets. Rust binds a single socket on the resolved `hostname:port` (default `0.0.0.0` → IPv4 only). IPv6-only clients can't connect.
- **Files:** `rust/crates/thttpd-core/src/startup.rs:bind_listeners`.

---

## Part 2 — Claims-vs-reality (these will get you caught in an interview)

A reviewer who runs the commands in your own README will hit these. **Fix the docs even if you don't fix the code** — an inaccurate README undercuts the rigor story.

| README / FRD claim | Reality (verified 2026-06-13) |
|---|---|
| "Zero `unsafe` blocks in the workspace" | **5 `unsafe` blocks** exist: `signal.rs:56`, `auth.rs:142,147,164` (FFI `crypt(3)`), `mmc/lib.rs:99` |
| "`cargo clippy -- -W pedantic` = zero warnings" | **212 pedantic warnings**, **20 default warnings** |
| "81 differential tests" | **105 differential tests** (JOURNEY.md is correct; README table is stale) |
| "2,429 lines of Rust" | **3,753** non-test / **5,107** total (README per-crate table is stale too: core listed 454, actual 1,823) |
| "≥200 golden master cases" (FRD criterion) | `baseline.json` has **45** entries — and it's **gitignored**, so a fresh clone has none |
| "`grep -r unsafe` returns zero" (FRD criterion) | False — see above |
| FRD acceptance checkboxes | All `[ ]` unchecked, yet JOURNEY says complete |
| FRD "References: PLAN.md, EXECUTION_PLAN.md, migration_path.md" | **Files do not exist** |

**How to frame the `unsafe` honestly (recommended):** The crypt(3) FFI in `auth.rs` is the *right* call — it's the only way to get byte-exact `.htpasswd` hash compatibility. Reframe the claim as *"zero hand-written unsafe; the 5 remaining `unsafe` blocks are audited FFI boundaries (crypt, signal handling) with documented safety invariants."* That's a *stronger* story than a false "zero."

---

## Part 3 — What's genuinely solid (lead with this in the interview)

These are real, verified, and impressive. Don't bury them while fixing gaps:

1. **105/105 differential tests pass**, byte-exact across status, headers (order + values), body SHA, connection lifecycle — proven by a re-run during this audit (305s).
2. **The JOURNEY.md "Discovery" story is gold** — "all 22 phases reported 'fully implemented' but tests told a different story" is a mature engineering narrative about structural-vs-behavioral gates. *This is the single best talking point in the repo.*
3. **Real, wired features:** incremental HTTP FSM (survives byte-by-byte slow-loris delivery), CGI with NPH + stdin-pipe deadlock fix, `.htpasswd` Basic Auth via crypt(3), vhost mapping, mmap file cache with refcounting, symlink-escape prevention, the three subtle final bugs (chunked-stdin deadlock, negative Content-Length, FSM state-reset).
4. **91 unit tests pass**, `cargo doc` is clean, knowledge YAML validates.
5. **Clean crate decomposition** (8 crates mirroring the C modules) with a documented C→Rust decision table.
6. **Working CI** (5 jobs, correct 105-count).

---

## Part 4 — Prioritized work for "full drop-in" + interview polish

### Tier 1 — Do these before the interview (highest demo value)
1. **Wire throttling into the event loop** + add one timing-based differential test (e.g. assert a 100KB file under a 10KB/s cap takes ≥9s on both binaries). Turns the project's weakest area into a live demo of the signature feature.
2. **Fix the chroot→bind→setuid order** (small change, big correctness win — shows you understand privilege-binding).
3. **Correct the README/FRD numbers** (test count 81→105, LOC, the `unsafe` framing). Cheap, but an inaccurate README is the fastest way to lose credibility.

### Tier 2 — Mention as "known next steps" if asked
4. Config-file parser (`-C`).
5. Request logging + pidfile.
6. CLI flag parity (`-h`/`-P`/`-V`/`-g`/`-dd`/`-s`).
7. Commit `baseline.json` (or regenerate in CI) so a reviewer can reproduce.

### Tier 3 — Backlog / honest limitations
8. Daemonization, IPv6, clippy-pedantic cleanup, knowledge `concepts/*.md` + ADRs (FRD wanted these).

---

## Part 5 — Interview-ready doc updates

- **README "Story" section:** swap "byte-exact drop-in replacement" for *"byte-exact for every served request, with a documented list of unimplemented operational surfaces (throttle enforcement, config file, logging, daemonization)."* Honest scoping reads as *more* senior, not less.
- **Add a "Known Limitations" section** to README listing Tier 2/3. Reviewers actively look for this; its absence looks naive.
- **JOURNEY.md:** add a "Phase G — Open Items" section pointing here, and reconcile the 105 vs the README's 81.
- **Live demo script:** a `demo.sh` that (a) curls both binaries on `/index.html` and `diff`s, (b) shows the differential test run, (c) *once wired* shows a throttled vs unthrottled transfer side by side. The side-by-side curl diff is the most compelling 30 seconds you can show.
- **Numbers-at-a-glance table:** correct to 5,107 LOC / 105 diff tests, and add a row for "operational features parity" set to "partial."

---

*Appendix: every line/figure in this document was verified against the working tree at commit df455f1 on 2026-06-13 — differential suite re-run to 105/105, unit tests to 91/91, clippy/grep/LOC recomputed live.*
