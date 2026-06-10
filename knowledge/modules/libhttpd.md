# libhttpd — HTTP Protocol Engine

## Source
`legacy/src/libhttpd.c` (4,230 lines) → `rust/crates/thttpd-http/src/` (995 lines across 8 modules)

## Status
Migrated. 80/80 differential tests pass.

## What It Does
The heart of the server. Handles HTTP request parsing, response generation, CGI dispatch, directory listing, and error pages. In C this was a single monolithic file; in Rust it's split into focused modules:

| Rust module | Responsibility |
|-------------|---------------|
| `parse.rs` | Incremental request-line FSM (byte-by-byte state machine) |
| `parse_state.rs` | FSM state enum and GotRequest result |
| `conn.rs` | HttpConn struct — per-connection state |
| `response.rs` | Response builder, error pages, header generation |
| `cgi.rs` | CGI execution via `std::process::Command` |
| `dirlist.rs` | HTML directory index generation |
| `method.rs` | HTTP method enum |
| `url.rs` | URL parsing and percent-decoding |
| `error.rs` | HTTP status codes and error types |

## Key Decisions
- **4.3× compression** — C's `libhttpd.c` was 4,230 lines; Rust equivalent is 995 lines.
- CGI uses `std::process::Command` instead of `fork()/execve()`. stdin pipe must always be closed (even when no POST body) to prevent deadlocks with `cat`-style CGI scripts.
- Parser FSM state persists across incremental reads (matching C's `hc->checked_state`) to handle slow-loris byte-by-byte delivery.
- Negative Content-Length values are rejected (filtered to `None`), matching C's `atol()` behavior where -1 is the sentinel for "unspecified."
