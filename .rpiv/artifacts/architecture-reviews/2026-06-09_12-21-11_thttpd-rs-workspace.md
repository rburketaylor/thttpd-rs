---
template_version: 1
date: 2026-06-09T12:21:11-0300
author: Burke T
commit: 3884d1a
branch: main
repository: thttpd-rs
target: rust/
target_kind: directory
layer_count: 4
unresolved_finding_count: 0
status: in-progress
tags: [architecture-review, thttpd-rs, workspace, rust-port]
last_updated: 2026-06-09T12:21:11-0300
last_updated_by: Burke T
---

# Architecture review — thttpd-rs workspace

Pre-1.0 audit of the entire Rust workspace (`rust/`) — an 8-crate, 25-file port of sthttpd targeting byte-exact behavioral parity with the C reference binary. The migration is in a "differential test failures" phase: 43 of 45 differential tests fail against the C golden master, with concentrated gaps in event-loop dispatch (eventloop.rs is ~700 LOC and central to nearly every failure). This review establishes the structural baseline so downstream phases can land fixes without re-architecting.

---

## Conventions

### Finding shape

Each finding is a level-3 heading `### L<layer>-<seq> — <title>` followed by the fields below.

| Field | Meaning |
|---|---|
| **Evidence** | `file.ext:lineA-lineB` (+ short quote when useful) |
| **Current state** | what the code does today |
| **Desired state** | what we want it to look like |
| **Proposed improvement** | concrete action (rename, extract, merge, split, delete) |
| **Severity** | Low / Med / High — how wrong this is today |
| **Effort** | S / M / L — bounded changes ship cheaply |
| **Blast radius** | `internal` / `public-API` / `on-disk` / `cross-module` |
| **Class** | `polish` (rename / refactor / DRY) vs `redesign` (structural shift) |
| **Status** | `open` / `accepted` / `rejected` / `deferred` / `withdrawn` |
| **Depends on** | other finding IDs that must land first |
| **Cross-cut tag** | optional — see "Cross-cutting themes" |

### Status legend

- `open` — flagged, not yet triaged
- `accepted` — will land; includes the chosen option summary
- `rejected` — declined with reason inline
- `deferred` — accepted in principle but punted post-release
- `withdrawn` — initial diagnosis turned out incorrect; kept for audit

### Layers (top → down)

| # | Layer | Files |
|---|---|---|
| 0 | Entry / Public surface | 3 — `crates/thttpd-core/src/main.rs`, `crates/thttpd-core/src/lib.rs`, `crates/thttpd-http/src/lib.rs` |
| 1 | Server orchestration | 7 — `crates/thttpd-core/src/{config,server,connection,eventloop,startup,signal,throttle}.rs` |
| 1.1 | Lifecycle / Config | `config.rs`, `startup.rs`, `signal.rs`, `throttle.rs` |
| 1.2 | Event loop dispatch | `eventloop.rs` (focus), with `server.rs` + `connection.rs` as supporting types |
| 2 | HTTP protocol | 9 — `crates/thttpd-http/src/{conn,method,parse,parse_state,url,response,error,cgi,dirlist}.rs` |
| 3 | Foundational utilities | 7 — `crates/thttpd-{match,tdate,mime,mime/types,timers,mmc,fdwatch}/src/lib.rs` |

---

## Methodology principles

_Principles emerge during Step 5 triage and are captured at Step 6. Patterns that govern multiple decisions get named here; one block per principle._

<!--
### M{N} — {principle name}

**Origin:** {finding ID where it first surfaced + one-sentence quote from the developer's reasoning, if available}.

**Rule.** {One paragraph: what to do, why, when to apply.}

**Apply to (keep):** {bullet list of cases the principle says to preserve.}
**Apply to (drop / change):** {bullet list of cases the principle says to act on.}
-->

---

## Layer 0 — Entry / Public surface

_Files: `crates/thttpd-core/src/main.rs`, `crates/thttpd-core/src/lib.rs`, `crates/thttpd-http/src/lib.rs`._

The crate-barrel / binary-entry surface. The two `lib.rs` files declare their modules and re-export a small public vocabulary; `main.rs` wires the four-step boot sequence (parse CLI → install signals → construct server → run loop).

### Layer 0 — tally

| Status | Count |
|---|---|
| accepted | TBD |
| rejected | TBD |
| deferred | TBD |
| withdrawn | TBD |

Cross-cutting tags introduced: TBD.
Cross-cutting tags reused: TBD.

Dependency edges within Layer 0:

- TBD

---

## Layer 1 — Server orchestration

_Files: `crates/thttpd-core/src/{config,server,connection,eventloop,startup,signal,throttle}.rs`._

Holds all runtime state (`Server`, `ConnSlot`) and the mio-driven event loop that drives the server. The heart of the layer is `eventloop.rs` — every request enters here, is dispatched to either static serving or CGI, and the response is written back. Other files in this layer are the supporting machinery.

### Layer 1.1 — Lifecycle / Config

_Files: `config.rs`, `startup.rs`, `signal.rs`, `throttle.rs`._

Boot-time configuration, bind/chroot/setuid, signal handlers, and (currently stubbed) bandwidth throttling.

### Layer 1.1 — tally

| Status | Count |
|---|---|
| accepted | TBD |
| rejected | TBD |
| deferred | TBD |
| withdrawn | TBD |

Cross-cutting tags introduced: TBD.
Cross-cutting tags reused: TBD.

Dependency edges within Layer 1.1:

- TBD

### Layer 1.2 — Event loop dispatch

_Files: `eventloop.rs` (focus), `server.rs`, `connection.rs` as supporting types._

The 700-line dispatch chain: `handle_accept → handle_read → process_request → serve_static | dispatch_cgi → handle_send → handle_linger → close`. This is the layer where 43 of 45 differential test failures live.

### Layer 1.2 — tally

| Status | Count |
|---|---|
| accepted | TBD |
| rejected | TBD |
| deferred | TBD |
| withdrawn | TBD |

Cross-cutting tags introduced: TBD.
Cross-cutting tags reused: TBD.

Dependency edges within Layer 1.2:

- TBD

---

## Layer 2 — HTTP protocol

_Files: `crates/thttpd-http/src/{conn,method,parse,parse_state,url,response,error,cgi,dirlist}.rs`._

The HTTP wire-format vocabulary. Owns `HttpConn` (the request/response accumulator), the request-detection FSM, the response builder, URL normalization, the CGI environment + execution layer, and the in-process directory listing generator. Re-exported types appear in `lib.rs` for consumption by `thttpd-core`.

### Layer 2 — tally

| Status | Count |
|---|---|
| accepted | TBD |
| rejected | TBD |
| deferred | TBD |
| withdrawn | TBD |

Cross-cutting tags introduced: TBD.
Cross-cutting tags reused: TBD.

Dependency edges within Layer 2:

- TBD

---

## Layer 3 — Foundational utilities

_Files: `crates/thttpd-match/src/lib.rs`, `crates/thttpd-tdate/src/lib.rs`, `crates/thttpd-mime/src/lib.rs` (+ `types.rs`), `crates/thttpd-timers/src/lib.rs`, `crates/thttpd-mmc/src/lib.rs`, `crates/thttpd-fdwatch/src/lib.rs`._

Leaf crates with no internal dependencies. Each owns one bounded concern: glob matching, date parsing, MIME lookup, timer wheel, mmap cache, I/O multiplexing wrapper. They are consumed by both `thttpd-core` and `thttpd-http`.

### Layer 3 — tally

| Status | Count |
|---|---|
| accepted | TBD |
| rejected | TBD |
| deferred | TBD |
| withdrawn | TBD |

Cross-cutting tags introduced: TBD.
Cross-cutting tags reused: TBD.

Dependency edges within Layer 3:

- TBD

---

## Cross-cutting themes

_written last, after all layers have been seen._

---

## Consolidated polish plan

_phases assembled after Step 7 cross-cut synthesis._
