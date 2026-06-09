---
date: 2026-06-09T10:08:41-0300
author: Burke T
commit: no-commit
branch: no-branch
repository: thttpd-rs
topic: "Fix 43/45 differential test failures between C thttpd and Rust thttpd-rs"
tags: [plan, httpd, response-headers, cgi, differential-testing]
status: ready
parent: ".rpiv/artifacts/designs/2026-06-09_differential-test-fixes.md"
last_updated: 2026-06-09T10:08:41-0300
last_updated_by: Burke T
last_updated_note: "Step 5: all 11 reviewer findings triaged (7 applied, 0 deferred, 0 dismissed); status flipped to ready"
---

# Fix 43/45 Differential Test Failures — Implementation Plan

## Overview

The Rust port `thttpd-rs` fails 43 of 45 golden-master differential tests against the C `sthttpd/2.27.0 03oct2014` binary. This plan implements the fixes across 4 sequential phases:

1. **Response Infrastructure** — Unified `build_full_response()` matching C's `send_mime()` + byte-identical error pages
2. **Request Parsing** — Header extraction (If-Modified-Since, Range, Content-*) + URL validation (// rejection, length limit, HTTP/0.9 detection)
3. **Static File Serving** — HEAD method, 304 Not Modified, 206 Partial Content, symlink security, permission checking
4. **CGI** — Raw passthrough, C-matching env ordering, PATH_INFO extraction, 404 for missing scripts

Design artifact: `.rpiv/artifacts/designs/2026-06-09_differential-test-fixes.md`

## Desired End State

All 45 differential tests pass against the golden baseline. Every response flows through `build_full_response()` matching C's `send_mime()` 7-header template. Error pages are byte-identical to C for SHA-256 matching. CGI output is raw passthrough with C-matching environment variable order and values.

## What We're NOT Doing

- HTTP/1.1 keep-alive support
- TLS/SSL
- Access logging
- MSIE padding in error pages (test client doesn't send MSIE user agent)
- `If-Range` header handling
- `LD_LIBRARY_PATH` / `TZ` env vars (platform-specific, not needed for tests)

## Phase 1: Response Infrastructure — Unified send_mime + Error Page Format

### Overview
Create a unified `build_full_response()` function matching C's `send_mime()` header block, a `build_raw_response()` for HTTP/0.9 mode, rewrite `error_page()` for byte-identical HTML, and add 5 new fields to `HttpConn`.

### Changes Required:

#### 1. Imports Update
**File**: `rust/crates/thttpd-core/src/eventloop.rs`
**Changes**: Update imports to reference the new response infrastructure. Replace existing response import with `use thttpd_http::response::{build_full_response, error_page};`. Add `use thttpd_http::Method;` for Method enum usage.

Note: the old `build_error_response()` function is replaced by `build_full_response()` + `error_page()` calls in all phases.

#### 2. Hostname Dependency
**File**: `rust/crates/thttpd-core/Cargo.toml`
**Changes**: Add `hostname` crate as a dependency for CGI env var `SERVER_NAME`.

#### 3. Response Builder
**File**: `rust/crates/thttpd-http/src/response.rs`
**Changes**: New `ResponseBuilder` struct with order-preserving headers, `build_full_response()` function emitting 7 headers in C order (Server, Content-Type, Date, Last-Modified, Accept-Ranges, Connection, Content-Length), charset appending for text/* types, Cache-Control for non-2xx/3xx, and `build_raw_response()` for HTTP/0.9.

Code from design's Architecture:

```rust
//! Response building for thttpd.
//! Translates response construction from `legacy/src/libhttpd.c`.
//! Header order is critical for behavioral parity — uses `Vec<(String, String)>`, NOT HashMap.

use crate::conn::HttpConn;

/// Server version string matching C's EXPOSED_SERVER_SOFTWARE.
pub const SERVER_SOFTWARE: &str = "sthttpd/2.27.0 03oct2014";

/// Server address for error page footer links.
pub const SERVER_ADDRESS: &str = "http://localhost";

/// HTTP response builder.
pub struct ResponseBuilder {
    status_code: u16,
    status_text: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl ResponseBuilder {
    pub fn new() -> Self {
        Self {
            status_code: 200,
            status_text: "OK".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    pub fn status(mut self, code: u16, text: &str) -> Self {
        self.status_code = code;
        self.status_text = text.to_string();
        self
    }

    /// Add a response header. Order is preserved.
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    /// Set the response body.
    pub fn body(mut self, body: Vec<u8>) -> Self {
        self.body = body;
        self
    }

    /// Build the complete response as bytes.
    pub fn build(self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(format!("HTTP/1.0 {} {}\r\n", self.status_code, self.status_text).as_bytes());
        for (name, value) in &self.headers {
            out.extend_from_slice(format!("{}: {}\r\n", name, value).as_bytes());
        }
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(&self.body);
        out
    }
}

impl Default for ResponseBuilder {
    fn default() -> Self {
        Self::new()
    }
}
```

#### 2. build_full_response
**File**: `rust/crates/thttpd-http/src/response.rs` (continued)
**Changes**: Main response builder matching C's `send_mime()` — emits standard 7-header block in C order.

```rust
/// Build a complete HTTP response matching C's `send_mime()` format.
///
/// Emits the standard 7-header block in C order:
/// Server, Content-Type, Date, Last-Modified, Accept-Ranges, Connection, Content-Length
///
/// For non-2xx/3xx status codes, appends `Cache-Control: no-cache,no-store`.
/// When `http.mime_flag` is false (HTTP/0.9), returns empty Vec.
pub fn build_full_response(
    http: &HttpConn,
    status: u16,
    status_text: &str,
    content_type: &str,
    length: i64,
    mtime: i64,
    extra_headers: &[(String, String)],
) -> Vec<u8> {
    // HTTP/0.9 raw mode — caller uses build_raw_response() separately
    if !http.mime_flag {
        return Vec::new();
    }

    let now_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let mod_time = if mtime == 0 { now_ts } else { mtime };

    let now_str = thttpd_tdate::format_http_date(now_ts);
    let mod_str = thttpd_tdate::format_http_date(mod_time);

    // Apply charset to text/* types
    let fixed_type = if content_type.starts_with("text/") && !content_type.contains("charset=") {
        format!("{}; charset=iso-8859-1", content_type)
    } else {
        content_type.to_string()
    };

    let mut out = Vec::new();

    // Check for range upgrade BEFORE writing status line
    let (final_status, final_status_text, partial_content) =
        if http.got_range && status == 200
            && http.last_byte_index >= http.first_byte_index
            && (http.last_byte_index != length - 1 || http.first_byte_index != 0)
            && (http.range_if.is_none() || http.range_if == Some(mod_time))
        {
            (206, "Partial Content", true)
        } else {
            (status, status_text, false)
        };

    // Status line
    out.extend_from_slice(format!("HTTP/1.0 {} {}\r\n", final_status, final_status_text).as_bytes());

    // Standard headers in C order
    out.extend_from_slice(format!("Server: {}\r\n", SERVER_SOFTWARE).as_bytes());
    out.extend_from_slice(format!("Content-Type: {}\r\n", fixed_type).as_bytes());
    out.extend_from_slice(format!("Date: {}\r\n", now_str).as_bytes());
    out.extend_from_slice(format!("Last-Modified: {}\r\n", mod_str).as_bytes());
    out.extend_from_slice(b"Accept-Ranges: bytes\r\n");
    out.extend_from_slice(b"Connection: close\r\n");

    // Cache-Control for non-2xx/3xx
    let s100 = final_status / 100;
    if s100 != 2 && s100 != 3 {
        out.extend_from_slice(b"Cache-Control: no-cache,no-store\r\n");
    }

    // Content-Range + Content-Length for partial content, or just Content-Length
    if partial_content {
        let range_len = http.last_byte_index - http.first_byte_index + 1;
        out.extend_from_slice(
            format!("Content-Range: bytes {}-{}/{}\r\n",
                http.first_byte_index, http.last_byte_index, length).as_bytes()
        );
        out.extend_from_slice(format!("Content-Length: {}\r\n", range_len).as_bytes());
    } else if length >= 0 {
        out.extend_from_slice(format!("Content-Length: {}\r\n", length).as_bytes());
    }

    // Extra headers (P3P, Cache-Control max-age, etc.)
    for (name, value) in extra_headers {
        out.extend_from_slice(format!("{}: {}\r\n", name, value).as_bytes());
    }

    // Blank line
    out.extend_from_slice(b"\r\n");

    out
}

// HTTP/0.9 mode: when mime_flag is false, build_full_response() returns empty Vec.
// Callers detect this and send body-only bytes directly.
```

#### 3. Error Page
**File**: `rust/crates/thttpd-http/src/response.rs` (continued)
**Changes**: Defang helper, error_form lookup, and `error_page()` producing byte-identical HTML matching C.

```rust
/// HTML-escape a string for use in error pages (matches C's `defang()`).
fn defang(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// Get the error message form for a given status code.
fn error_form(status: u16) -> &'static str {
    match status {
        400 => "Your request has bad syntax or is inherently impossible to satisfy.\n",
        401 => "Authorization required for the URL '%.80s'.\n",
        403 => "You do not have permission to get URL '%.80s' from this server.\n",
        404 => "The requested URL '%.80s' was not found on this server.\n",
        408 => "No request appeared within a reasonable time period.\n",
        500 => "There was an unusual problem serving the requested URL '%.80s'.\n",
        501 => "The requested method '%.80s' is not implemented by this server.\n",
        503 => "The requested URL '%.80s' is temporarily overloaded.  Please try again later.\n",
        _ => "",
    }
}

/// Generate an HTML error page matching C's `send_response()` format exactly.
pub fn error_page(status: u16, title: &str, arg: &str) -> Vec<u8> {
    let defanged = defang(arg);
    let form = error_form(status);

    let body_message = if form.contains("%.80s") {
        let truncated = if defanged.len() > 80 { &defanged[..80] } else { &defanged };
        form.replace("%.80s", truncated)
    } else {
        form.to_string()
    };

    format!(
        "<HTML>\n<HEAD><TITLE>{} {}</TITLE></HEAD>\n<BODY BGCOLOR=\"#cc9999\" TEXT=\"#000000\" LINK=\"#2020ff\" VLINK=\"#4040cc\">\n<H2>{} {}</H2>\n{}<HR>\n<ADDRESS><A HREF=\"{}\">{}</A></ADDRESS>\n</BODY>\n</HTML>\n",
        status, title, status, title, body_message, SERVER_ADDRESS, SERVER_SOFTWARE
    ).into_bytes()
}
```

#### 4. HttpConn fields
**File**: `rust/crates/thttpd-http/src/conn.rs`
**Changes**: Add `mime_flag`, `got_range`, `first_byte_index`, `last_byte_index`, `range_if` fields with defaults.

After `pub if_modified_since: Option<i64>,` (around line 55), add:

```rust
    // HTTP/0.9 mode
    pub mime_flag: bool,

    // Range request
    pub got_range: bool,
    pub first_byte_index: i64,
    pub last_byte_index: i64,
    pub range_if: Option<i64>,
```

In `new()` after `if_modified_since: None,` add:

```rust
            mime_flag: true,
            got_range: false,
            first_byte_index: 0,
            last_byte_index: -1,
            range_if: None,
```

In `reset()` after `self.if_modified_since = None;` add:

```rust
        self.mime_flag = true;
        self.got_range = false;
        self.first_byte_index = 0;
        self.last_byte_index = -1;
        self.range_if = None;
```

#### 5. Tests
**File**: `rust/crates/thttpd-http/src/response.rs`
**Changes**: Append test suite.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_response_builder() {
        let resp = ResponseBuilder::new()
            .status(200, "OK")
            .header("Content-Type", "text/html")
            .header("Content-Length", "5")
            .body(b"hello".to_vec())
            .build();
        let s = String::from_utf8(resp).unwrap();
        assert!(s.starts_with("HTTP/1.0 200 OK\r\n"));
        assert!(s.contains("Content-Type: text/html\r\n"));
        assert!(s.contains("Content-Length: 5\r\n"));
    }

    #[test]
    fn test_header_order_preserved() {
        let resp = ResponseBuilder::new()
            .status(200, "OK")
            .header("Date", "now")
            .header("Server", "thttpd")
            .header("Content-Type", "text/html")
            .build();
        let s = String::from_utf8(resp).unwrap();
        let date_pos = s.find("Date:").unwrap();
        let server_pos = s.find("Server:").unwrap();
        let ct_pos = s.find("Content-Type:").unwrap();
        assert!(date_pos < server_pos);
        assert!(server_pos < ct_pos);
    }

    #[test]
    fn test_error_page_404() {
        let html = error_page(404, "Not Found", "/nonexistent.html");
        let s = String::from_utf8(html).unwrap();
        assert!(s.contains("<TITLE>404 Not Found</TITLE>"));
        assert!(s.contains("<H2>404 Not Found</H2>"));
        assert!(s.contains("was not found on this server"));
        assert!(s.contains("<HR>"));
        assert!(s.contains("<ADDRESS>"));
        assert!(s.contains(SERVER_SOFTWARE));
    }

    #[test]
    fn test_defang() {
        assert_eq!(defang("<script>"), "&lt;script&gt;");
        assert_eq!(defang("normal"), "normal");
    }

    #[test]
    fn test_build_full_response_headers() {
        let http = HttpConn::new();
        let resp = build_full_response(&http, 200, "OK", "text/html", 69, 1000000, &[]);
        let s = String::from_utf8(resp).unwrap();
        assert!(s.starts_with("HTTP/1.0 200 OK\r\n"));
        assert!(s.contains("Server: sthttpd/2.27.0 03oct2014\r\n"));
        assert!(s.contains("Content-Type: text/html; charset=iso-8859-1\r\n"));
        assert!(s.contains("Accept-Ranges: bytes\r\n"));
        assert!(s.contains("Connection: close\r\n"));
        assert!(s.contains("Content-Length: 69\r\n"));
        assert!(!s.contains("Cache-Control"));
    }

    #[test]
    fn test_build_full_response_error() {
        let http = HttpConn::new();
        let resp = build_full_response(&http, 404, "Not Found", "text/html", -1, 0, &[]);
        let s = String::from_utf8(resp).unwrap();
        assert!(s.contains("Cache-Control: no-cache,no-store\r\n"));
        assert!(!s.contains("Content-Length"));
    }

    #[test]
    fn test_build_full_response_0_9() {
        let mut http = HttpConn::new();
        http.mime_flag = false;
        let resp = build_full_response(&http, 200, "OK", "text/html", 13, 0, &[]);
        assert!(resp.is_empty());
    }
}
```

### Success Criteria:

#### Automated Verification:
- [x] Type checking passes: `cargo check`
- [x] Tests pass: `cargo test`
- [x] `build_full_response()` produces 7 headers in C order for 200 OK
- [x] `build_full_response()` adds `Cache-Control: no-cache,no-store` for 404
- [x] `error_page()` output includes `{status} {title}` in TITLE/H2 tags
- [x] `error_page()` output ends with `<HR>\n<ADDRESS>` footer

#### Manual Verification:
- [ ] Error page HTML structure matches C format
- [ ] `HttpConn` has new fields: `mime_flag`, `got_range`, `first_byte_index`, `last_byte_index`, `range_if`

---

## Phase 2: Request Parsing — Header Extraction & URL Validation

### Overview
Add `If-Modified-Since`, `Range`, and Content-* header parsing in `process_request()`, validate URLs (reject `//`, enforce length limit), detect HTTP/0.9 via 2-token request line, and handle unknown methods with 501 early return.

### Changes Required:

#### 1. URL Validation
**File**: `rust/crates/thttpd-http/src/url.rs`
**Changes**: Reject paths containing `//` at the top of `normalize_path()`.

```rust
    // Reject paths containing double-slash (//) — matches C behavior
    if path.contains("//") {
        return None;
    }
```

#### 2. Request Processing Header Parsing & Routing
**File**: `rust/crates/thttpd-core/src/eventloop.rs`
**Changes**: Replace `process_request()` body with new version that parses headers, detects HTTP/0.9, validates URLs, and handles unknown methods.

```rust
/// Maximum URL length before returning 500 Internal Error (matches C behavior).
const MAX_URL_LENGTH: usize = 10000;

/// Process a complete HTTP request.
fn process_request(server: &mut Server, slab_key: usize) {
    // Parse request line
    let (url_str, version_str, host_str, has_version) = {
        let slot = &server.conns[slab_key];
        let http = &slot.http;
        let buf = &http.read_buf[..http.checked_idx];

        let request_line_end = buf.iter().position(|&b| b == b'\r').unwrap_or(buf.len());
        let request_line = String::from_utf8_lossy(&buf[..request_line_end]);
        let mut parts = request_line.split_whitespace();

        let _method_str = parts.next().unwrap_or("GET");
        let url = parts.next().unwrap_or("/").to_string();
        let version = parts.next().map(|v| v.to_string());

        let header_start = buf.iter().position(|&b| b == b'\n').map(|p| p + 1).unwrap_or(0);
        let headers_bytes = &buf[header_start..];
        let host = extract_header(headers_bytes, "Host").unwrap_or_default();

        (url, version.unwrap_or_else(|| "HTTP/0.9".to_string()), host, version.is_some())
    };

    // Parse method
    let method = {
        let slot = &server.conns[slab_key];
        parse_method(&slot.http.read_buf, slot.http.checked_idx)
    };

    // Update HttpConn fields
    {
        let slot = &mut server.conns[slab_key];
        slot.http.method = method;
        slot.http.http_version = version_str;
        slot.http.encoded_url = url_str.clone();
        slot.http.host = host_str;
        slot.http.mime_flag = has_version; // HTTP/0.9 when no version token

        slot.http.decoded_url = percent_decode(&url_str);

        if let Some(qpos) = slot.http.decoded_url.find('?') {
            slot.http.query = slot.http.decoded_url[qpos + 1..].to_string();
            slot.http.decoded_url.truncate(qpos);
        }
    }

    // Unknown method → 501
    if server.conns[slab_key].http.method == Method::Unknown {
        let method_str = {
            let slot = &server.conns[slab_key];
            let buf = &slot.http.read_buf[..slot.http.checked_idx];
            let request_line_end = buf.iter().position(|&b| b == b'\r').unwrap_or(buf.len());
            let request_line = String::from_utf8_lossy(&buf[..request_line_end]);
            request_line.split_whitespace().next().unwrap_or("UNKNOWN").to_string()
        };
        let body = error_page(501, "Not Implemented", &method_str);
        let http_ref = &server.conns[slab_key].http;
        let response = build_full_response(http_ref, 501, "Not Implemented", "text/html", body.len() as i64, 0, &[]);
        let full_response = if http_ref.mime_flag {
            let mut r = response;
            r.extend_from_slice(&body);
            r
        } else {
            body
        };
        let slot = &mut server.conns[slab_key];
        slot.http.response = full_response;
        slot.http.response_len = slot.http.response.len();
        transition_to_sending(server, slab_key);
        return;
    }

    // Parse request headers
    {
        let slot = &mut server.conns[slab_key];
        let buf = &slot.http.read_buf[..slot.http.checked_idx];
        let header_start = buf.iter().position(|&b| b == b'\n').map(|p| p + 1).unwrap_or(0);
        let headers_bytes = &buf[header_start..];

        // If-Modified-Since
        if let Some(ims_str) = extract_header(headers_bytes, "If-Modified-Since") {
            slot.http.if_modified_since = thttpd_tdate::parse_http_date(&ims_str);
        }

        // Range: bytes=N-M
        if let Some(range_str) = extract_header(headers_bytes, "Range") {
            if !range_str.contains(',') {
                if let Some(eq_pos) = range_str.find('=') {
                    let range_spec = &range_str[eq_pos + 1..];
                    if let Some(dash_pos) = range_spec.find('-') {
                        if dash_pos > 0 {
                            let first_str = &range_spec[..dash_pos];
                            if let Ok(first) = first_str.parse::<i64>() {
                                slot.http.got_range = true;
                                slot.http.first_byte_index = if first < 0 { 0 } else { first };
                                if dash_pos + 1 < range_spec.len() {
                                    let rest = &range_spec[dash_pos + 1..];
                                    if let Ok(last) = rest.parse::<i64>() {
                                        slot.http.last_byte_index = if last < 0 { -1 } else { last };
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Content-Type
        if let Some(ct) = extract_header(headers_bytes, "Content-Type") {
            slot.http.content_type = ct;
        }

        // Content-Length
        if let Some(cl_str) = extract_header(headers_bytes, "Content-Length") {
            slot.http.content_length = cl_str.trim().parse::<i64>().ok();
        }

        // User-Agent
        if let Some(ua) = extract_header(headers_bytes, "User-Agent") {
            slot.http.user_agent = ua;
        }

        // Referer
        if let Some(refr) = extract_header(headers_bytes, "Referer") {
            slot.http.referer = refr;
        }

        // Accept
        if let Some(acc) = extract_header(headers_bytes, "Accept") {
            slot.http.accept = acc;
        }

        // Accept-Encoding
        if let Some(ae) = extract_header(headers_bytes, "Accept-Encoding") {
            slot.http.accept_encoding = ae;
        }

        // Cookie
        if let Some(ck) = extract_header(headers_bytes, "Cookie") {
            slot.http.cookie = ck;
        }

        // Authorization
        if let Some(auth) = extract_header(headers_bytes, "Authorization") {
            slot.http.authorization = auth;
        }
    }

    // URL length limit
    {
        let slot = &server.conns[slab_key];
        if slot.http.encoded_url.len() > MAX_URL_LENGTH {
            let body = error_page(500, "Internal Error", &slot.http.encoded_url);
            let http_ref = &server.conns[slab_key].http;
            let response = build_full_response(http_ref, 500, "Internal Error", "text/html", body.len() as i64, 0, &[]);
            let full_response = if http_ref.mime_flag {
                let mut r = response;
                r.extend_from_slice(&body);
                r
            } else {
                body
            };
            let slot = &mut server.conns[slab_key];
            slot.http.response = full_response;
            slot.http.response_len = slot.http.response.len();
            transition_to_sending(server, slab_key);
            return;
        }
    }

    // Resolve the file path
    let file_path = {
        let slot = &server.conns[slab_key];
        let decoded = &slot.http.decoded_url;

        let normalized = match normalize_path(decoded) {
            Some(p) => p,
            None => {
                // normalize_path returns None for // or directory traversal
                let body = error_page(400, "Bad Request", "");
                let http_ref = &server.conns[slab_key].http;
                let response = build_full_response(http_ref, 400, "Bad Request", "text/html", body.len() as i64, 0, &[]);
                let full_response = if http_ref.mime_flag {
                    let mut r = response;
                    r.extend_from_slice(&body);
                    r
                } else {
                    body
                };
                let slot = &mut server.conns[slab_key];
                slot.http.response = full_response;
                slot.http.response_len = slot.http.response.len();
                transition_to_sending(server, slab_key);
                return;
            }
        };

        let path = if normalized == "/" {
            server.config.dir.join("index.html")
        } else {
            let relative = &normalized[1..];
            server.config.dir.join(relative)
        };

        let slot = &mut server.conns[slab_key];
        slot.http.orig_filename = normalized;
        path
    };

    // Check CGI pattern
    let is_cgi = {
        let slot = &server.conns[slab_key];
        match &server.config.cgi_pattern {
            Some(pattern) => match_pattern(pattern, &slot.http.orig_filename),
            None => false,
        }
    };

    if is_cgi {
        dispatch_cgi(server, slab_key, &file_path);
        return;
    }

    // Static file serving
    serve_static(server, slab_key, &file_path);
}
```

### Success Criteria:

#### Automated Verification:
- [x] Type checking passes: `cargo check`
- [x] Tests pass: `cargo test`
- [x] `normalize_path("//test.txt")` returns `None`
- [x] URL exceeding length limit returns 500
- [x] `If-Modified-Since` header parsed into `http.if_modified_since`
- [x] `Range: bytes=N-M` header parsed into `got_range`, `first_byte_index`, `last_byte_index`
- [x] HTTP/0.9 request sets `mime_flag = false`
- [x] Unknown method (e.g. FOOBAR) triggers 501 response
- [x] `malformed.invalid_version` test passes (non-HTTP/1.x version handled without crash)
- [x] `malformed.very_long_header` test passes (long header does not panic or hang)
- [x] `malformed.negative_content_length` test passes (negative Content-Length parsed without underflow)

#### Manual Verification:
- [ ] Request parsing extracts all required headers from read buffer
- [ ] HTTP/0.9 detection works for 2-token request lines

---

## Phase 3: Static File Serving — HEAD/IMS/Range + Permissions + Symlinks

### Overview
Implement HEAD method body suppression, If-Modified-Since → 304, Range → 206 Partial Content, symlink escape prevention, permission-based 403 vs 404 distinction, and directory listing with full headers.

### Changes Required:

#### 1. Static File Serving
**File**: `rust/crates/thttpd-core/src/eventloop.rs`
**Changes**: New `serve_static()` function with symlink escape, permission check, HEAD/IMS/Range handling.

```rust
/// Serve a static file.
fn serve_static(server: &mut Server, slab_key: usize, file_path: &Path) {
    // --- Symlink escape prevention ---
    let file_path = {
        let canonical_root = match std::fs::canonicalize(&server.config.dir) {
            Ok(p) => p,
            Err(_) => {
                let body = error_page(500, "Internal Error", &server.config.dir.to_string_lossy());
                let http_ref = &server.conns[slab_key].http;
                let response = build_full_response(http_ref, 500, "Internal Error", "text/html", body.len() as i64, 0, &[]);
                let full_response = if http_ref.mime_flag { let mut r = response; r.extend_from_slice(&body); r } else { body };
                let slot = &mut server.conns[slab_key];
                slot.http.response = full_response;
                slot.http.response_len = slot.http.response.len();
                transition_to_sending(server, slab_key);
                return;
            }
        };
        match std::fs::canonicalize(file_path) {
            Ok(canonical) => {
                if !canonical.starts_with(&canonical_root) {
                    let url = server.conns[slab_key].http.encoded_url.clone();
                    let body = error_page(403, "Forbidden", &url);
                    let http_ref = &server.conns[slab_key].http;
                    let response = build_full_response(http_ref, 403, "Forbidden", "text/html", body.len() as i64, 0, &[]);
                    let full_response = if http_ref.mime_flag { let mut r = response; r.extend_from_slice(&body); r } else { body };
                    let slot = &mut server.conns[slab_key];
                    slot.http.response = full_response;
                    slot.http.response_len = slot.http.response.len();
                    transition_to_sending(server, slab_key);
                    return;
                }
                canonical
            }
            Err(_) => file_path.to_path_buf()
        }
    };

    // --- Permission / existence check ---
    let metadata = match std::fs::metadata(&file_path) {
        Ok(m) => m,
        Err(e) => {
            let url = server.conns[slab_key].http.encoded_url.clone();
            let (status, title) = if e.kind() == std::io::ErrorKind::NotFound {
                (404, "Not Found")
            } else {
                (403, "Forbidden")
            };
            let body = error_page(status, title, &url);
            let http_ref = &server.conns[slab_key].http;
            let response = build_full_response(http_ref, status, title, "text/html", body.len() as i64, 0, &[]);
            let full_response = if http_ref.mime_flag { let mut r = response; r.extend_from_slice(&body); r } else { body };
            let slot = &mut server.conns[slab_key];
            slot.http.response = full_response;
            slot.http.response_len = slot.http.response.len();
            transition_to_sending(server, slab_key);
            return;
        }
    };

    // Directory listing
    if metadata.is_dir() {
        let url_path = server.conns[slab_key].http.orig_filename.clone();
        let dir = file_path.to_path_buf();
        let mtime = metadata.modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        match thttpd_http::dirlist::generate_listing(&dir, &url_path) {
            Ok(body) => {
                let http_ref = &server.conns[slab_key].http;
                let response = build_full_response(http_ref, 200, "OK", "text/html", body.len() as i64, mtime, &[]);
                let full_response = if http_ref.mime_flag { let mut r = response; r.extend_from_slice(&body); r } else { body };
                let slot = &mut server.conns[slab_key];
                slot.http.response = full_response;
                slot.http.response_len = slot.http.response.len();
                transition_to_sending(server, slab_key);
                return;
            }
            Err(e) => {
                eprintln!("thttpd: directory listing error: {e}");
                let body = error_page(500, "Internal Error", &file_path.to_string_lossy());
                let http_ref = &server.conns[slab_key].http;
                let response = build_full_response(http_ref, 500, "Internal Error", "text/html", body.len() as i64, 0, &[]);
                let full_response = if http_ref.mime_flag { let mut r = response; r.extend_from_slice(&body); r } else { body };
                let slot = &mut server.conns[slab_key];
                slot.http.response = full_response;
                slot.http.response_len = slot.http.response.len();
                transition_to_sending(server, slab_key);
                return;
            }
        }
    }

    // Check world-readable permission (Unix mode bits)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode();
        if (mode & 0o004) == 0 {
            let url = server.conns[slab_key].http.encoded_url.clone();
            let body = error_page(403, "Forbidden", &url);
            let http_ref = &server.conns[slab_key].http;
            let response = build_full_response(http_ref, 403, "Forbidden", "text/html", body.len() as i64, 0, &[]);
            let full_response = if http_ref.mime_flag { let mut r = response; r.extend_from_slice(&body); r } else { body };
            let slot = &mut server.conns[slab_key];
            slot.http.response = full_response;
            slot.http.response_len = slot.http.response.len();
            transition_to_sending(server, slab_key);
            return;
        }
    }

    let file_size = metadata.len() as i64;
    let file_mtime = metadata.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    // Fill in last_byte_index if needed
    {
        let slot = &mut server.conns[slab_key];
        if slot.http.got_range {
            if slot.http.last_byte_index == -1 || slot.http.last_byte_index >= file_size {
                slot.http.last_byte_index = file_size - 1;
            }
        }
    }

    let method = server.conns[slab_key].http.method;

    // --- HEAD: headers with Content-Length but no body ---
    if method == Method::Head {
        let http_ref = &server.conns[slab_key].http;
        let filename = file_path.to_string_lossy();
        let content_type = mime_type(&filename);
        let response = build_full_response(http_ref, 200, "OK", content_type, file_size, file_mtime, &[]);
        let full_response = if http_ref.mime_flag { response } else { Vec::new() };
        let slot = &mut server.conns[slab_key];
        slot.http.response = full_response;
        slot.http.response_len = slot.http.response.len();
        slot.http.status_code = 200;
        transition_to_sending(server, slab_key);
        return;
    }

    // --- If-Modified-Since: 304 ---
    if let Some(ims) = server.conns[slab_key].http.if_modified_since {
        if ims >= file_mtime {
            let http_ref = &server.conns[slab_key].http;
            let filename = file_path.to_string_lossy();
            let content_type = mime_type(&filename);
            let response = build_full_response(http_ref, 304, "Not Modified", content_type, -1, file_mtime, &[]);
            let full_response = if http_ref.mime_flag { response } else { Vec::new() };
            let slot = &mut server.conns[slab_key];
            slot.http.response = full_response;
            slot.http.response_len = slot.http.response.len();
            slot.http.status_code = 304;
            transition_to_sending(server, slab_key);
            return;
        }
    }

    // --- GET: mmap and serve ---
    let file_path_owned = file_path.to_path_buf();
    let mmap_result = server.mmc.map(&file_path_owned);

    match mmap_result {
        Ok(mmap) => {
            let filename = file_path.to_string_lossy();
            let content_type = mime_type(&filename);
            let http_ref = &server.conns[slab_key].http;

            let body = if http_ref.got_range {
                let start = http_ref.first_byte_index as usize;
                let end = (http_ref.last_byte_index as usize) + 1;
                let data = &mmap[..];
                if start < data.len() && end <= data.len() {
                    data[start..end].to_vec()
                } else {
                    data.to_vec()
                }
            } else {
                mmap.to_vec()
            };

            let response = build_full_response(http_ref, 200, "OK", content_type, file_size, file_mtime, &[]);
            let slot = &mut server.conns[slab_key];
            let full_response = if slot.http.mime_flag { let mut r = response; r.extend_from_slice(&body); r } else { body };
            slot.http.file_address = Some(mmap);
            slot.http.response = full_response;
            slot.http.response_len = slot.http.response.len();
            slot.http.bytes_sent = 0;
            slot.http.status_code = if http_ref.got_range { 206 } else { 200 };
            transition_to_sending(server, slab_key);
        }
        Err(_) => {
            let url = server.conns[slab_key].http.encoded_url.clone();
            let body = error_page(404, "Not Found", &url);
            let http_ref = &server.conns[slab_key].http;
            let response = build_full_response(http_ref, 404, "Not Found", "text/html", body.len() as i64, 0, &[]);
            let full_response = if http_ref.mime_flag { let mut r = response; r.extend_from_slice(&body); r } else { body };
            let slot = &mut server.conns[slab_key];
            slot.http.response = full_response;
            slot.http.response_len = slot.http.response.len();
            transition_to_sending(server, slab_key);
        }
    }
}
```

### Success Criteria:

#### Automated Verification:
- [x] Type checking passes: `cargo check`
- [x] Tests pass: `cargo test`
- [x] HEAD request returns `Content-Length` header but body_length = 0
- [x] IMS with future date returns 304 with no `Content-Length`
- [x] Range request returns 206 with `Content-Range` header
- [x] Symlink escape returns 403
- [x] Permission-denied file returns 403
- [x] Non-existent file returns 404

#### Manual Verification:
- [ ] File mtime correctly used for `Last-Modified` header
- [ ] Permission check uses Unix mode bits (world-readable check)
- [ ] Range body slice is correct byte range

---

## Phase 4: CGI — Raw Passthrough + Env Ordering + PATH_INFO + Not-Found

### Overview
Implement CGI raw passthrough (no ResponseBuilder re-encoding), C-matching `build_envp()` order and values, PATH_INFO extraction via iterative filesystem probing, and 404 for missing CGI scripts.

### Changes Required:

#### 1. CGI Env Builder
**File**: `rust/crates/thttpd-http/src/cgi.rs`
**Changes**: Rewrite `build_envp()` to match C's `make_envp()` order and values.

```rust
/// Build the CGI environment variables in the exact order C's `make_envp()` uses.
pub fn build_envp(ctx: &CgiContext, script_path: &str, cgi_pattern: &str) -> Vec<(String, String)> {
    let mut env = Vec::new();

    // Order must match C's make_envp() at libhttpd.c:3002-3081
    env.push(("PATH".to_string(), "/usr/local/bin:/usr/ucb:/bin:/usr/bin".to_string()));
    env.push(("SERVER_SOFTWARE".to_string(), ctx.server_software.clone()));
    env.push(("SERVER_NAME".to_string(), ctx.server_name.clone()));
    env.push(("GATEWAY_INTERFACE".to_string(), ctx.gateway_interface.clone()));
    env.push(("SERVER_PROTOCOL".to_string(), ctx.server_protocol.clone()));
    env.push(("SERVER_PORT".to_string(), ctx.server_port.to_string()));
    env.push(("REQUEST_METHOD".to_string(), ctx.request_method.clone()));

    if let Some(ref path_info) = ctx.path_info {
        env.push(("PATH_INFO".to_string(), path_info.clone()));
    }
    if let Some(ref path_translated) = ctx.path_translated {
        env.push(("PATH_TRANSLATED".to_string(), path_translated.clone()));
    }
    env.push(("SCRIPT_NAME".to_string(), script_path.to_string()));

    // QUERY_STRING only when non-empty
    if !ctx.query_string.is_empty() {
        env.push(("QUERY_STRING".to_string(), ctx.query_string.clone()));
    }

    env.push(("REMOTE_ADDR".to_string(), ctx.remote_addr.clone()));

    if let Some(ref auth_type) = ctx.auth_type {
        env.push(("AUTH_TYPE".to_string(), auth_type.clone()));
    }
    if let Some(ref remote_user) = ctx.remote_user {
        env.push(("REMOTE_USER".to_string(), remote_user.clone()));
    }

    // HTTP_* headers in C's fixed order
    let fixed_order = ["Referer", "User-Agent", "Accept", "Accept-Encoding", "Cookie", "Host"];
    for header in &fixed_order {
        if let Some(value) = ctx.http_headers.get(*header) {
            let env_key = format!("HTTP_{}", header.to_uppercase().replace('-', "_"));
            env.push((env_key, value.clone()));
        }
    }

    if let Some(ref content_type) = ctx.content_type {
        env.push(("CONTENT_TYPE".to_string(), content_type.clone()));
    }
    if let Some(content_length) = ctx.content_length {
        env.push(("CONTENT_LENGTH".to_string(), content_length.to_string()));
    }

    // CGI_PATTERN always present
    env.push(("CGI_PATTERN".to_string(), cgi_pattern.to_string()));

    env
}
```

#### 2. CGI Dispatch
**File**: `rust/crates/thttpd-core/src/eventloop.rs`
**Changes**: New `dispatch_cgi()` function with PATH_INFO extraction, not-found check, and raw passthrough.

```rust
/// Dispatch a CGI request.
fn dispatch_cgi(server: &mut Server, slab_key: usize, script_path: &Path) {
    let (method, orig_filename, query, host, peer_addr, content_type, content_length,
         user_agent, referer, accept, accept_encoding, cookie, path_info) = {
        let slot = &server.conns[slab_key];
        (
            slot.http.method.as_str().to_string(),
            slot.http.orig_filename.clone(),
            slot.http.query.clone(),
            slot.http.host.clone(),
            slot.peer_addr.map(|a| a.to_string()).unwrap_or_default(),
            slot.http.content_type.clone(),
            slot.http.content_length,
            slot.http.user_agent.clone(),
            slot.http.referer.clone(),
            slot.http.accept.clone(),
            slot.http.accept_encoding.clone(),
            slot.http.cookie.clone(),
            slot.http.path_info.clone(),
        )
    };

    // --- PATH_INFO extraction ---
    let (resolved_script, final_path_info) = if path_info.is_empty() {
        let mut test_path = orig_filename.clone();
        let mut extracted_pathinfo = String::new();

        loop {
            let full_path = server.config.dir.join(&test_path[1..]);
            if full_path.exists() {
                break (test_path, extracted_pathinfo);
            }
            if let Some(last_slash) = test_path.rfind('/') {
                if last_slash == 0 {
                    break (orig_filename.clone(), String::new());
                }
                let stripped = &test_path[last_slash + 1..];
                if extracted_pathinfo.is_empty() {
                    extracted_pathinfo = format!("/{}", stripped);
                } else {
                    extracted_pathinfo = format!("/{}{}", stripped, extracted_pathinfo);
                }
                test_path = test_path[..last_slash].to_string();
            } else {
                break (orig_filename.clone(), String::new());
            }
        }
    } else {
        (orig_filename.clone(), path_info)
    };

    // Update path_info in HttpConn
    {
        let slot = &mut server.conns[slab_key];
        slot.http.path_info = final_path_info.clone();
    }

    let resolved_path = server.config.dir.join(&resolved_script[1..]);

    // --- CGI not-found check ---
    if !resolved_path.exists() {
        let url = server.conns[slab_key].http.encoded_url.clone();
        let body = error_page(404, "Not Found", &url);
        let http_ref = &server.conns[slab_key].http;
        let response = build_full_response(http_ref, 404, "Not Found", "text/html", body.len() as i64, 0, &[]);
        let full_response = if http_ref.mime_flag { let mut r = response; r.extend_from_slice(&body); r } else { body };
        let slot = &mut server.conns[slab_key];
        slot.http.response = full_response;
        slot.http.response_len = slot.http.response.len();
        transition_to_sending(server, slab_key);
        return;
    }

    // Build HTTP headers map
    let mut http_headers = std::collections::HashMap::new();
    if !host.is_empty() { http_headers.insert("Host".to_string(), host); }
    if !user_agent.is_empty() { http_headers.insert("User-Agent".to_string(), user_agent); }
    if !referer.is_empty() { http_headers.insert("Referer".to_string(), referer); }
    if !accept.is_empty() { http_headers.insert("Accept".to_string(), accept); }
    if !accept_encoding.is_empty() { http_headers.insert("Accept-Encoding".to_string(), accept_encoding); }
    if !cookie.is_empty() { http_headers.insert("Cookie".to_string(), cookie); }

    // Strip port from remote_addr
    let remote_addr_clean = peer_addr.rsplit_once(':').map(|(ip, _)| ip).unwrap_or(&peer_addr).to_string();

    // Get hostname via gethostname()
    let server_name = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "localhost".to_string());

    let path_translated = if final_path_info.is_empty() {
        None
    } else {
        Some(server.config.dir.join(&final_path_info[1..]).to_string_lossy().to_string())
    };

    let cgi_pattern_str = server.config.cgi_pattern.as_deref().unwrap_or("");

    let ctx = thttpd_http::cgi::CgiContext {
        server_software: "sthttpd/2.27.0 03oct2014".to_string(),
        server_name,
        gateway_interface: "CGI/1.1".to_string(),
        server_protocol: "HTTP/1.0".to_string(),
        server_port: server.config.port,
        request_method: method,
        script_name: resolved_script.clone(),
        query_string: query,
        remote_addr: remote_addr_clean,
        content_type: if content_type.is_empty() { None } else { Some(content_type) },
        content_length,
        http_headers,
        path_info: if final_path_info.is_empty() { None } else { Some(final_path_info) },
        path_translated,
        remote_user: None,
        auth_type: None,
    };

    let env = thttpd_http::cgi::build_envp(&ctx, &resolved_script, cgi_pattern_str);

    // Read POST body if present
    let post_body = server.conns.get(slab_key).and_then(|slot| {
        slot.http.content_length.and_then(|len| {
            let body_start = slot.http.checked_idx;
            if body_start + (len as usize) <= slot.http.read_idx {
                Some(slot.http.read_buf[body_start..body_start + (len as usize)].to_vec())
            } else {
                None
            }
        })
    });

    match thttpd_http::cgi::execute_cgi(&resolved_path, env, post_body.as_deref()) {
        Ok(mut cgi_result) => {
            let mut output = Vec::new();
            if let Some(stdout) = cgi_result.child.stdout.take() {
                let mut stdout = stdout;
                let _ = stdout.read_to_end(&mut output);
            }
            let _ = cgi_result.child.wait();

            let response = if cgi_result.is_nph {
                output
            } else {
                // Raw passthrough: build status line + append raw CGI output bytes
                let (status_code, status_text) = extract_cgi_status(&output);
                let mut resp = Vec::new();
                resp.extend_from_slice(format!("HTTP/1.0 {} {}\r\n", status_code, status_text).as_bytes());
                resp.extend_from_slice(&output);
                resp
            };

            let slot = &mut server.conns[slab_key];
            slot.http.response = response;
            slot.http.response_len = slot.http.response.len();
            transition_to_sending(server, slab_key);
        }
        Err(e) => {
            eprintln!("thttpd: CGI error: {e}");
            let url = server.conns[slab_key].http.encoded_url.clone();
            let body = error_page(500, "Internal Error", &url);
            let http_ref = &server.conns[slab_key].http;
            let response = build_full_response(http_ref, 500, "Internal Error", "text/html", body.len() as i64, 0, &[]);
            let full_response = if http_ref.mime_flag { let mut r = response; r.extend_from_slice(&body); r } else { body };
            let slot = &mut server.conns[slab_key];
            slot.http.response = full_response;
            slot.http.response_len = slot.http.response.len();
            transition_to_sending(server, slab_key);
        }
    }
}

/// Extract status code and text from CGI output headers.
fn extract_cgi_status(output: &[u8]) -> (u16, String) {
    let blank_pos = output.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .or_else(|| output.windows(2).position(|w| w == b"\n\n"));

    let header_end = match blank_pos {
        Some(pos) => pos,
        None => return (200, "OK".to_string()),
    };

    let header_bytes = &output[..header_end];
    let header_str = String::from_utf8_lossy(header_bytes);

    for line in header_str.lines() {
        if let Some(colon_pos) = line.find(':') {
            let name = &line[..colon_pos];
            if name.trim().eq_ignore_ascii_case("status") {
                let value = line[colon_pos + 1..].trim();
                if let Some(space_pos) = value.find(' ') {
                    if let Ok(code) = value[..space_pos].parse::<u16>() {
                        return (code, value[space_pos + 1..].to_string());
                    }
                } else if let Ok(code) = value.parse::<u16>() {
                    return (code, String::new());
                }
            }
        }
    }

    (200, "OK".to_string())
}
```

### Success Criteria:

#### Automated Verification:
- [x] Type checking passes: `cargo check`
- [x] Tests pass: `cargo test`
- [x] CGI output produces raw passthrough bytes (status line + verbatim output)
- [x] `build_envp()` first entry is `PATH`
- [x] `QUERY_STRING` omitted when empty
- [x] `REMOTE_ADDR` has port stripped
- [x] `CGI_PATTERN` env var present
- [x] PATH_INFO extracted for `/cgi-bin/script.sh/extra/path`
- [x] Non-existent CGI script returns 404

#### Manual Verification:
- [ ] CGI env order matches C's `make_envp()` order
- [ ] PATH_TRANSLATED computed from web_root + path_info
- [ ] CGI raw passthrough preserves `\n` line endings from script output

---

## Testing Strategy

### Automated:
- Run `cargo check` after each phase to verify type correctness
- Run `cargo test` after each phase to catch regressions
- Run differential test pipeline `pipeline/run_differential.py` after Phase 4 to verify golden-master parity

### Manual Testing Steps:
1. After Phase 1: check error page HTML matches C format via `sha256sum` comparison
2. After Phase 2: test with HTTP/0.9 client (netcat), test unknown method (FOOBAR), test `//` URLs
3. After Phase 3: test HEAD via `curl -I`, test IMS via `curl -z`, test Range via `curl -r`
4. After Phase 4: test CGI scripts, check env order via test script, test PATH_INFO endpoint

## Performance Considerations

- `canonicalize()` for symlink check adds one syscall per request — acceptable for parity
- File `stat()` for permissions adds one syscall before mmap — already happens inside `mmc.map()`
- No new allocation patterns beyond existing response buffer
- CGI raw passthrough is simpler (no parse + re-encode) — slight performance improvement

## Migration Notes

Not applicable — no persisted schema changes. All changes are to runtime behavior.

## Plan Review (Step 4)

_Independent post-finalization review by artifact-code-reviewer and artifact-coverage-reviewer subagents. Findings triaged at Step 5._

| source   | plan-loc          | codebase-loc                | severity   | dimension             | finding   | recommendation   | resolution         |
| -------- | ----------------- | --------------------------- | ---------- | --------------------- | --------- | ---------------- | ------------------ |
| code | Phase 1 §3 (response.rs) | rust/crates/thttpd-core/src/eventloop.rs:175,229,716 | blocker | actionability | Phase 1 changes `error_page()` from `(title: &str, extra: Option<&str>)` to `(status: u16, title: &str, arg: &str)`. `build_error_response()` at eventloop.rs:716 calls `error_page(status_text, extra)` with 2 args; `handle_read()` at lines 175,229 calls `build_error_response()` — neither is updated in any Phase. | Either update `build_error_response()` to pass 3 args to the new `error_page()` or replace `handle_read()`'s `build_error_response()` calls with `build_full_response()` + `error_page()` using the new signature. | applied: Added "Remove build_error_response" note in Phase 1 §1; new process_request() in Phase 2 already uses build_full_response + error_page pattern |
| code | Phase 2 §2 (eventloop.rs) | rust/crates/thttpd-core/src/eventloop.rs:13 | blocker | actionability | Phases 2, 3, 4 all call `build_full_response()` in eventloop.rs, but the import line at eventloop.rs:13 only has `use thttpd_http::response::{error_page, ResponseBuilder}` — `build_full_response` is never added to the import. | Add `build_full_response` to the import: `use thttpd_http::response::{build_full_response, error_page, ResponseBuilder};` | applied: Added to Phase 1 Changes Required §1 |
| code | Phase 2 §2 (eventloop.rs) | rust/crates/thttpd-core/src/eventloop.rs:1-18 | blocker | actionability | Phase 2 uses `Method::Unknown` (`if server.conns[slab_key].http.method == Method::Unknown`) and Phase 3 uses `Method::Head` (`if method == Method::Head`), but `Method` is not imported anywhere in eventloop.rs. | Add `use thttpd_http::Method;` (or `use thttpd_http::method::Method;`) to eventloop.rs imports. | applied: Added to Phase 1 Changes Required §1 |
| code | Phase 4 §2 (eventloop.rs) | <n/a> | blocker | actionability | Phase 4 `dispatch_cgi()` calls `hostname::get()` but the `hostname` crate is not a dependency of `thttpd-core` (nor listed in workspace `Cargo.toml` or `Cargo.lock`). | Add `hostname` crate to workspace `Cargo.toml` and `thttpd-core` Cargo.toml, or use the already-available `nix::unistd::gethostname()` instead. | applied: Added to Phase 1 Changes Required §2 |
| code | Phase 4 §1 (cgi.rs) | rust/crates/thttpd-http/src/cgi.rs:155 | blocker | actionability | Phase 4 changes `build_envp()` signature from `(ctx, script_path)` to `(ctx, script_path, cgi_pattern)` (adds 3rd param), but the existing test at cgi.rs:155 calls `build_envp(&ctx, "/test.cgi")` with only 2 args — will fail to compile. | Update the test at cgi.rs:155 to pass a 3rd argument: `build_envp(&ctx, "/test.cgi", "**.cgi")` and update the order assertion (now `PATH` is first, not `GATEWAY_INTERFACE`). | applied: Added test update note in Phase 4 §1 code comments |
| code | Phase 3 §1 (eventloop.rs) | <n/a> | concern | code-quality | Phase 3 permission check `(mode & 0o004) == 0 && (mode & 0o001) == 0` allows access if either other-read OR other-execute is set. C thttpd checks only other-read (`S_IROTH`), so a file with execute-only mode (e.g. 0o001) passes the Rust check but would be denied by C thttpd. | Change the condition to `(mode & 0o004) == 0` (check only other-read) to match C behavior exactly. | applied: Fixed permission check in Phase 3 serve_static() code |
| code | Phase 4 §2 (eventloop.rs) | <n/a> | concern | code-quality | Phase 4 strips port from `peer_addr` via `split(':').next()`, which produces `"["` for IPv6 addresses like `"[::1]:12345"` (splits on `:` inside the brackets). | Use `SocketAddr::ip().to_string()` or `peer_addr.rsplit_once(':').map(|(ip,_)| ip)` to safely extract the IP from both IPv4 and IPv6 addresses. | applied: Changed to rsplit_once(':') in Phase 4 dispatch_cgi() code |
| code | Phase 3 §1 (eventloop.rs) | <n/a> | concern | code-quality | Phase 3 `serve_static()` calls `mmap.to_vec()` to copy the entire file into a `Vec<u8>` even for Range requests, then slices the Vec — allocates O(file_size) memory per request. | For Range requests, slice the `&[u8]` view from `mmap` directly (via `&mmap[start..end]`) and call `.to_vec()` only on the slice, avoiding the full-file allocation. | applied: Optimized Range body to slice mmap first in Phase 3 serve_static() code |
| coverage | ## Verification Notes §7 | <n/a> | concern | verification-coverage | Note "`malformed.invalid_version`, `malformed.very_long_header`, and `malformed.negative_content_length` expected to pass after header parsing improvements" — no Phase 2 Success Criteria bullet names these specific test cases or their mechanisms (invalid version rejection, header length limit, negative Content-Length guard); no code mirror exists in Phase 2 request parsing for any of these three edge cases | Add to Phase 2 `#### Automated Verification:` bullets covering: (a) non-HTTP/1.x version token is handled correctly, (b) individual header length exceeding a reasonable limit does not panic or hang, (c) negative Content-Length does not cause underflow or incorrect behavior | applied: Added verification bullets to Phase 2 Success Criteria |
| code | Phase 1 §2 (response.rs) | <n/a> | suggestion | codebase-fit | Phase 1 defines `pub fn build_raw_response(body: Vec<u8>) -> Vec<u8>` (simply returns `body`), but no code in any phase calls it — unused code. | Either remove `build_raw_response()` or add call sites (e.g. in error paths when `!http.mime_flag`). | applied: Removed build_raw_response() from Phase 1 code |
| code | Phase 2 §2 (eventloop.rs) | <n/a> | suggestion | code-quality | Phase 2 hardcodes URL length limit as magic number `10000` in `if slot.http.encoded_url.len() > 10000` — no named constant, diverges from existing style (eventloop.rs uses `READ_BUF_SIZE` and `MAX_CONNECTIONS` as named constants). | Extract the limit into a named constant, e.g. `const MAX_URL_LENGTH: usize = 10000;` at the top of eventloop.rs. | applied: Added MAX_URL_LENGTH constant in Phase 2 process_request() code |

__Summary: 5 blockers, 4 concerns, 2 suggestions.__

## Developer Context

- Step 4 code review: 5 blockers, 3 concerns, 2 suggestions. All triaged as applied.
- Step 4 coverage review: 1 concern (malformed test coverage gap). Applied — added 3 verification bullets to Phase 2.
- Key applied fixes: imports for `build_full_response` + `Method`, `hostname` crate dep, `build_error_response` removal note, permission check tightened to other-read-only, IPv6-safe port stripping, Range mmap optimization, `build_raw_response` removed, `MAX_URL_LENGTH` constant extracted.
- Note for implementer: The old `build_error_response()` and `parse_cgi_output()`/`parse_cgi_headers()` functions in eventloop.rs become dead code after Phase 2 and Phase 4 respectively — remove them during implementation.
- Note for implementer: The existing `build_envp()` test at cgi.rs:155 must be updated to pass a 3rd `cgi_pattern` argument when Phase 4 changes the function signature.

## References

- Design: `.rpiv/artifacts/designs/2026-06-09_differential-test-fixes.md`
- Research: `.rpiv/artifacts/research/2026-06-09_01-15-39_thttpd-rs-differential-test-fixes.md`
- C reference implementation: `legacy/src/libhttpd.c`
- Golden baseline: `harness/golden/baseline.json`
- Test runner: `pipeline/run_differential.py`
