# libhttpd — HTTP Protocol Engine

## Source
`legacy/src/libhttpd.c` (4,230 lines) → `rust/crates/thttpd-http/src/` (1,993 lines across 11 modules)

## Status
Migrated. 105 differential scenarios pass.

## What It Does
The heart of the server. Handles HTTP request parsing, authentication, response generation, CGI dispatch, directory listing, and error pages. In C this was a single monolithic file; in Rust it's split into focused modules:

| Rust module | Responsibility |
|-------------|---------------|
| `lib.rs` | Public module exports |
| `parse.rs` | Incremental request-line FSM (byte-by-byte state machine) |
| `parse_state.rs` | FSM state enum and GotRequest result |
| `conn.rs` | HttpConn struct — per-connection state |
| `auth.rs` | Basic auth parsing and `.htpasswd` verification |
| `response.rs` | Response builder, error pages, header generation |
| `cgi.rs` | CGI execution via `std::process::Command` |
| `dirlist.rs` | HTML directory index generation |
| `method.rs` | HTTP method enum |
| `url.rs` | URL parsing and percent-decoding |
| `error.rs` | HTTP status codes and error types |

## Key Decisions
- CGI uses `std::process::Command` instead of `fork()/execve()`. stdin pipe must always be closed (even when no POST body) to prevent deadlocks with `cat`-style CGI scripts.
- Parser FSM state persists across incremental reads (matching C's `hc->checked_state`) to handle slow-loris byte-by-byte delivery.
- Negative Content-Length values are rejected (filtered to `None`), matching C's `atol()` behavior where -1 is the sentinel for "unspecified."
