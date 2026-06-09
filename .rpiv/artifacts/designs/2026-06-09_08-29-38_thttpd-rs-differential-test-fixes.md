---
date: 2026-06-09T08:29:38-0300
author: Burke T
commit: no-commit
branch: no-branch
repository: thttpd-rs
topic: "Fix 43/45 differential test failures between C thttpd and Rust thttpd-rs"
tags: [design, differential-testing, response-headers, cgi, http-protocol]
status: in-progress
parent: .rpiv/artifacts/research/2026-06-09_01-15-39_thttpd-rs-differential-test-fixes.md
last_updated: 2026-06-09T08:29:38-0300
last_updated_by: Burke T
---

# Design: Fix 43/45 Differential Test Failures

## Summary

Fix 43 out of 45 golden-master differential test failures between the C `sthttpd/2.27.0 03oct2014` binary and the Rust thttpd-rs port. The approach centralizes response construction into a shared `build_mime_response()` helper modeled after C's `send_mime()` gateway function, rewrites CGI output handling to use raw byte passthrough instead of parse-and-re-encode, and adds missing HTTP protocol features (HEAD suppression, If-Modified-Since/304, Range/206, HTTP/0.9 raw mode, symlink containment, permission-based 403/404).

## Requirements

- Pass all 45 golden-master differential tests in `harness/golden/baseline.json`
- Match C's response header format exactly: 7-header preamble in `Server, Content-Type, Date, Last-Modified, Accept-Ranges, Connection, Content-Length` order
- Match C's error page HTML template exactly (body SHA-256 is verified)
- Match C's CGI output wire format (raw passthrough, not re-encoded)
- Support HTTP/0.9 raw response mode (no headers, body only)
- Support HEAD method (headers with Content-Length but no body)
- Support If-Modified-Since / 304 Not Modified
- Support Range requests / 206 Partial Content
- Return 501 for unknown HTTP methods
- Prevent symlink escape (containment within web root)
- Distinguish 403 Forbidden from 404 Not Found based on file permissions
- Extract CGI PATH_INFO from trailing path components
- Validate URLs (reject `//`, enforce length limit)

## Current State Analysis

### Key Discoveries

- **`send_mime()` is the single response gateway** in C (`libhttpd.c:597-670`) — all responses flow through it. Rust has 6 ad-hoc `ResponseBuilder` call sites with inconsistent headers.
- **Error body SHA-256 is verified** — baseline checks `body_sha256` for all tests including errors. Error page HTML template must match C's `send_response()` format at `libhttpd.c:725-775` exactly.
- **Error responses have no Content-Length** — C uses `length=-1` to suppress Content-Length in error pages.
- **CGI tests have unusual header format** — the golden parser can't find `\r\n\r\n` in C's raw passthrough, so CGI body content ends up embedded in header values.
- **HTTP/0.9 uses empty headers dict** — baseline shows `edge.http09_simple` with `headers: {}` and body_length=13.
- **`malformed.binary_garbage` and `malformed.truncated_request`** — FSM never completes; no response sent. Golden parser records status 200 with empty headers. No code changes needed.
- **`HttpConn.if_modified_since`** exists at `conn.rs:55` but is never populated or read.
- **`HttpConn.mime_flag`** does not exist — needs to be added for HTTP/0.9 support.
- **CGI env order starts with `GATEWAY_INTERFACE`** in Rust but should start with `PATH` per C's `make_envp()` at `libhttpd.c:3002-3081`.
- **`build_error_response()` called at 7 sites** — all pass `None` for `extra`, all produce inconsistent headers.

## Scope

### Building

- Shared `build_mime_response()` helper for standardized 7-header preamble
- HTTP/0.9 raw response mode via `mime_flag`
- HEAD method body suppression
- If-Modified-Since / 304 Not Modified support
- Range / 206 Partial Content support
- 501 Not Implemented for unknown methods
- Symlink escape prevention (canonicalize containment)
- Permission-based 403 vs 404 distinction
- CGI raw output passthrough
- CGI environment variable ordering and value fixes
- CGI PATH_INFO extraction
- CGI not found → 404
- Error page template matching C's format
- Content-Type charset appending for text/* types
- `//` rejection in URL paths
- URL length limit
- Directory listing with full 7-header set

### Not Building

- HTTP method case-insensitivity (baseline uses uppercase only)
- MSIE error page padding (`<!-- Padding so MSIE... -->` in `send_response()`)
- HTTP/1.1 keep-alive support
- New features beyond C parity
- Authentication (401) support
- Virtual host support
- P3P / max-age header differences beyond what baseline tests exercise

## Decisions

### Response Architecture: Centralized Helper

**Ambiguity:** Rust has 6 ad-hoc ResponseBuilder call sites with inconsistent header sets. C has a single `send_mime()` gateway.

**Decision:** Create a shared `build_mime_response()` free function in `eventloop.rs` that emits the standard 7-header preamble in C order (Server, Content-Type, Date, Last-Modified, Accept-Ranges, Connection, Content-Length). All response paths flow through this helper. ResponseBuilder remains the low-level byte serializer.

**Evidence:** C's `send_mime()` at `libhttpd.c:597-670`; Rust's scattered ResponseBuilder calls at `eventloop.rs:345, 378, 499, 715`.

### CGI Output: Raw Passthrough

**Ambiguity:** Rust parses CGI output into structured headers and re-encodes via ResponseBuilder. C prepends status line and writes raw bytes verbatim.

**Decision:** Rewrite `dispatch_cgi()` to use raw passthrough: prepend `HTTP/1.0 <status> <title>\r\n`, detect Status/Location headers for status code, then write ALL raw CGI output bytes verbatim. Do not use ResponseBuilder for CGI body.

**Evidence:** C's `cgi_interpose_output()` at `libhttpd.c:3208-3348`; Rust's `parse_cgi_output()` at `eventloop.rs:502-520`.

### Error Page Template: Match C Exactly

**Decision:** Rewrite `error_page()` to match C's `send_response()` format: `<H2>{status} {title}</H2>` (not just title), form text with defanged args, `<HR>\n<ADDRESS>` footer with server software link.

**Evidence:** C's `send_response()` at `libhttpd.c:725-775`; baseline verifies `body_sha256` for all error tests.

### Server String

**Decision:** Use `"sthttpd/2.27.0 03oct2014"` everywhere (matching C's `EXPOSED_SERVER_SOFTWARE`).

**Evidence:** `libhttpd.c:638` uses `EXPOSED_SERVER_SOFTWARE`; baseline expects `"Server": "sthttpd/2.27.0 03oct2014"`.

## Architecture

### rust/crates/thttpd-http/src/conn.rs:38-60 — MODIFY

Add 5 new fields to `HttpConn` struct after `if_modified_since: Option<i64>` (line 55):

```rust
    // Protocol flags
    pub mime_flag: bool,

    // Range request state
    pub got_range: bool,
    pub first_byte_index: i64,
    pub last_byte_index: i64,
    pub range_if: Option<i64>,
```

Initialize in `HttpConn::new()` (after `if_modified_since: None,`):

```rust
            mime_flag: true,
            got_range: false,
            first_byte_index: 0,
            last_byte_index: 0,
            range_if: None,
```

Clear in `HttpConn::reset()` (after `self.if_modified_since = None;`):

```rust
        self.mime_flag = true;
        self.got_range = false;
        self.first_byte_index = 0;
        self.last_byte_index = 0;
        self.range_if = None;
```

### rust/crates/thttpd-http/src/response.rs:13-84 — MODIFY

Add constants at module level (before `ResponseBuilder` struct):

```rust
/// Server software string matching C's EXPOSED_SERVER_SOFTWARE.
pub const SERVER_SOFTWARE: &str = "sthttpd/2.27.0 03oct2014";

/// Server address for error page footer.
pub const SERVER_ADDRESS: &str = "http://www.acme.com/software/thttpd/";
```

Add `build_raw()` method to `ResponseBuilder` impl block:

```rust
    /// Build response as raw body bytes only — no HTTP framing (for HTTP/0.9).
    pub fn build_raw(self) -> Vec<u8> {
        self.body
    }
```

### rust/crates/thttpd-http/src/url.rs:38-60 — MODIFY

```rust
```

### rust/crates/thttpd-http/src/cgi.rs:46-111 — MODIFY

```rust
```

### rust/crates/thttpd-http/src/method.rs:13-28 — MODIFY

```rust
```

### rust/crates/thttpd-mime/src/types.rs:12-38 — MODIFY

```rust
```

### rust/crates/thttpd-core/src/eventloop.rs:183-540 — MODIFY

Add `status_title()` function and `build_mime_response()` helper (before `build_error_response()`):

```rust
/// Map status codes to C's canonical title strings.
fn status_title(status: u16) -> &'static str {
    match status {
        200 => "OK",
        206 => "Partial Content",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        500 => "Internal Error",
        501 => "Not Implemented",
        503 => "Service Temporarily Overloaded",
        _ => "Something",
    }
}

/// Build a complete HTTP response with the standard 7-header preamble.
/// Modeled after C's send_mime() at libhttpd.c:597-670.
///
/// Parameters:
/// - status: HTTP status code
/// - content_type: MIME type (charset should already be appended)
/// - length: body length, or -1 to suppress Content-Length (error pages, 304)
/// - mtime: file modification time (epoch seconds), or 0 to use current time
/// - mime_flag: false for HTTP/0.9 raw body-only mode
/// - body: response body bytes
/// - server: reference to Server for config access
fn build_mime_response(
    status: u16,
    content_type: &str,
    length: i64,
    mtime: i64,
    mime_flag: bool,
    body: Vec<u8>,
    server: &Server,
) -> Vec<u8> {
    if !mime_flag {
        // HTTP/0.9: raw body only
        return body;
    }

    let title = status_title(status);
    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let now = format_http_date(now_epoch);
    let mod_time = format_http_date(if mtime == 0 { now_epoch } else { mtime });

    let mut out = Vec::new();

    // Status line
    out.extend_from_slice(format!("HTTP/1.0 {} {}\r\n", status, title).as_bytes());

    // 7 fixed headers in C order (libhttpd.c:638-641)
    out.extend_from_slice(format!("Server: {}\r\n", thttpd_http::response::SERVER_SOFTWARE).as_bytes());
    out.extend_from_slice(format!("Content-Type: {}\r\n", content_type).as_bytes());
    out.extend_from_slice(format!("Date: {}\r\n", now).as_bytes());
    out.extend_from_slice(format!("Last-Modified: {}\r\n", mod_time).as_bytes());
    out.extend_from_slice(b"Accept-Ranges: bytes\r\n");
    out.extend_from_slice(b"Connection: close\r\n");

    // Cache-Control for non-2xx/3xx (libhttpd.c:642-648)
    let s100 = status / 100;
    if s100 != 2 && s100 != 3 {
        out.extend_from_slice(b"Cache-Control: no-cache,no-store\r\n");
    }

    // Content-Length (suppressed when length == -1, used for errors and 304)
    if length >= 0 {
        out.extend_from_slice(format!("Content-Length: {}\r\n", length).as_bytes());
    }

    // P3P header if configured
    if let Some(ref p3p) = server.config.p3p {
        out.extend_from_slice(format!("P3P: {}\r\n", p3p).as_bytes());
    }

    // Cache-Control: max-age + Expires if configured
    if server.config.max_age >= 0 {
        let expires_epoch = now_epoch + server.config.max_age as i64;
        let expires = format_http_date(expires_epoch);
        out.extend_from_slice(format!("Cache-Control: max-age={}\r\n", server.config.max_age).as_bytes());
        out.extend_from_slice(format!("Expires: {}\r\n", expires).as_bytes());
    }

    // End of headers
    out.extend_from_slice(b"\r\n");

    // Body
    out.extend_from_slice(&body);

    out
}
```

## Slices

### Slice 1: HttpConn fields & shared response builder

**Files**: `rust/crates/thttpd-http/src/conn.rs`, `rust/crates/thttpd-http/src/response.rs`, `rust/crates/thttpd-core/src/eventloop.rs`

#### Automated Verification:
- [ ] `cargo build` compiles without errors: `cargo build --manifest-path rust/Cargo.toml`
- [ ] `cargo test` passes all existing tests: `cargo test --manifest-path rust/Cargo.toml`
- [ ] `grep -r "mime_flag" rust/crates/` returns matches in conn.rs and eventloop.rs
- [ ] `grep -r "build_mime_response" rust/crates/` returns match in eventloop.rs

#### Manual Verification:
- [ ] `build_mime_response()` emits 7 headers in correct C order: Server, Content-Type, Date, Last-Modified, Accept-Ranges, Connection
- [ ] `build_mime_response()` adds Cache-Control: no-cache,no-store for non-2xx/3xx status codes
- [ ] `build_mime_response()` suppresses Content-Length when length == -1
- [ ] `build_raw()` on ResponseBuilder returns body bytes only
- [ ] New HttpConn fields have correct defaults (mime_flag=true, got_range=false, range fields zeroed)

### Slice 2: Error page template & error response fixes

**Files**: `rust/crates/thttpd-http/src/response.rs`, `rust/crates/thttpd-core/src/eventloop.rs`

#### Automated Verification:

#### Manual Verification:

### Slice 3: Static file serving & HEAD method

**Files**: `rust/crates/thttpd-core/src/eventloop.rs`, `rust/crates/thttpd-mime/src/types.rs`

#### Automated Verification:

#### Manual Verification:

### Slice 4: If-Modified-Since / 304 & Range / 206

**Files**: `rust/crates/thttpd-core/src/eventloop.rs`

#### Automated Verification:

#### Manual Verification:

### Slice 5: HTTP/0.9 & URL validation

**Files**: `rust/crates/thttpd-core/src/eventloop.rs`, `rust/crates/thttpd-http/src/url.rs`

#### Automated Verification:

#### Manual Verification:

### Slice 6: Symlink & permission checks

**Files**: `rust/crates/thttpd-core/src/eventloop.rs`

#### Automated Verification:

#### Manual Verification:

### Slice 7: CGI raw passthrough & environment

**Files**: `rust/crates/thttpd-core/src/eventloop.rs`, `rust/crates/thttpd-http/src/cgi.rs`

#### Automated Verification:

#### Manual Verification:

### Slice 8: CGI PATH_INFO & remaining fixes

**Files**: `rust/crates/thttpd-core/src/eventloop.rs`, `rust/crates/thttpd-http/src/cgi.rs`, `rust/crates/thttpd-http/src/method.rs`

#### Automated Verification:

#### Manual Verification:

## Desired End State

From the differential test runner's perspective:

```bash
# Run the golden-master differential test suite
python3 pipeline/run_differential.py

# Expected: 45/45 tests pass (currently 2/45 pass)
# All 8 comparison fields match for every test:
# - status_code, status_text, headers, body_sha256, body_length, connection_result
```

Example responses after fix:

```
# Static file: 7 headers in C order, correct charset
HTTP/1.0 200 OK\r\n
Server: sthttpd/2.27.0 03oct2014\r\n
Content-Type: text/plain; charset=iso-8859-1\r\n
Date: Tue, 09 Jun 2026 03:20:35 GMT\r\n
Last-Modified: Tue, 09 Jun 2026 03:20:34 GMT\r\n
Accept-Ranges: bytes\r\n
Connection: close\r\n
Content-Length: 13\r\n
\r\n
Hello, world!

# Error response: no Content-Length, Cache-Control for non-2xx
HTTP/1.0 404 Not Found\r\n
Server: sthttpd/2.27.0 03oct2014\r\n
Content-Type: text/html; charset=iso-8859-1\r\n
Date: ...\r\n
Last-Modified: ...\r\n
Accept-Ranges: bytes\r\n
Connection: close\r\n
Cache-Control: no-cache,no-store\r\n
\r\n
<HTML>\n<HEAD><TITLE>404 Not Found</TITLE></HEAD>\n...

# CGI: raw passthrough
HTTP/1.0 200 OK\r\n
Content-Type: text/plain\n\nHello from CGI

# HTTP/0.9: raw body only (no headers)
Hello, world!
```

## File Map

```
rust/crates/thttpd-http/src/conn.rs       # MODIFY — add mime_flag, range fields
rust/crates/thttpd-http/src/response.rs    # MODIFY — add build_raw(), fix error_page() template
rust/crates/thttpd-http/src/url.rs         # MODIFY — add // rejection
rust/crates/thttpd-http/src/cgi.rs         # MODIFY — env ordering, values, PATH_INFO helpers
rust/crates/thttpd-http/src/method.rs      # MODIFY — case-insensitive matching
rust/crates/thttpd-mime/src/types.rs       # MODIFY — charset appending for text/*
rust/crates/thttpd-core/src/eventloop.rs   # MODIFY — shared helper, CGI passthrough, all fixes
```

## Ordering Constraints

- Slice 1 (foundation) must come first — all other slices depend on the shared response builder
- Slices 2-8 can be generated sequentially (each builds on the previous)
- Within eventloop.rs, changes are cumulative — each slice adds to the previous

## Verification Notes

- The golden capture harness (`pipeline/run_differential.py`) compares 8 fields: `status_code`, `status_text`, `header_count`, `header_order`, `header_values`, `body_sha256`, `body_length`, `connection_result`
- Header ORDER matters, not just presence — the comparison is field-by-field
- Error body SHA-256 is verified — HTML template must match C byte-for-byte
- CGI tests have unusual header format because golden parser can't find `\r\n\r\n` in raw passthrough
- `malformed.binary_garbage` and `malformed.truncated_request` need no code changes — FSM handles them
- `malformed.very_long_header` and `malformed.negative_content_length` expect normal 200 OK file serving (C ignores bad Content-Length)
- `edge.post_to_static` expects 200 OK with file content (C serves static files for POST too)

## Performance Considerations

- No performance implications — changes are about correctness parity, not performance
- `std::fs::canonicalize()` for symlink check adds one syscall per request with symlinks — acceptable
- `std::fs::metadata()` for permission check adds one syscall per request — same as C's `stat()` call

## Migration Notes

Not applicable — no persisted data or schema changes.

## Pattern References

- `legacy/src/libhttpd.c:597-670` — `send_mime()`: template for all response construction
- `legacy/src/libhttpd.c:725-775` — `send_response()`: template for error page HTML
- `legacy/src/libhttpd.c:782-790` — `send_response_tail()`: template for error page footer
- `legacy/src/libhttpd.c:3208-3348` — `cgi_interpose_output()`: template for CGI raw passthrough
- `legacy/src/libhttpd.c:3002-3081` — `make_envp()`: template for CGI environment ordering
- `legacy/src/libhttpd.c:1430-1660` — `expand_symlinks()`: template for PATH_INFO extraction
- `legacy/src/libhttpd.c:3579-3847` — `really_start_request()`: template for HEAD/IMS/Range decision logic

## Developer Context

- Developer confirmed centralized response helper approach (vs ad-hoc ResponseBuilder at each site)
- Developer confirmed CGI raw passthrough approach (vs parse-and-re-encode)
- Developer approved scope: all 15 findings, no MSIE padding, no method case-insensitivity
- Developer approved 8-slice decomposition

## Design History

- Slice 1: HttpConn fields & shared response builder — approved as generated
- Slice 2: Error page template & error response fixes — pending
- Slice 3: Static file serving & HEAD method — pending
- Slice 4: If-Modified-Since / 304 & Range / 206 — pending
- Slice 5: HTTP/0.9 & URL validation — pending
- Slice 6: Symlink & permission checks — pending
- Slice 7: CGI raw passthrough & environment — pending
- Slice 8: CGI PATH_INFO & remaining fixes — pending

## References

- Research artifact: `.rpiv/artifacts/research/2026-06-09_01-15-39_thttpd-rs-differential-test-fixes.md`
- Golden baseline: `harness/golden/baseline.json`
- Differential runner: `pipeline/run_differential.py`
- C reference: `legacy/src/libhttpd.c`
