---
date: 2026-06-09T01:15:39-0300
author: Burke T
commit: no-commit
branch: no-branch
repository: thttpd-rs
topic: "Fix 43/45 differential test failures between C thttpd and Rust thttpd-rs"
tags: [research, codebase, eventloop, httpd, cgi, headers, differential-testing]
status: complete
last_updated: 2026-06-09T01:15:39-0300
last_updated_by: Burke T
---

# Research: Fix 43/45 Differential Test Failures (C thttpd vs Rust thttpd-rs)

## Research Question

The Rust port `thttpd-rs` fails 43 out of 45 golden-master differential tests against the C `sthttpd/2.27.0 03oct2014` binary. The baseline is captured at `harness/golden/baseline.json` and the runner is `pipeline/run_differential.py`. What exact behavioral differences exist, and what must change in each Rust function to achieve parity?

## Summary

The failures fall into 6 categories: (1) incomplete response headers — Rust emits 4 headers where C emits 7, with wrong values and order; (2) missing HEAD method body suppression; (3) missing If-Modified-Since (304) and Range (206) support; (4) missing HTTP/0.9 raw response mode; (5) missing symlink escape prevention and permission-based 403 vs 404 distinction; (6) CGI output re-encoding, wrong environment values, and missing PATH_INFO extraction. Nearly all fixes target `eventloop.rs`, with supporting changes to `conn.rs`, `response.rs`, `cgi.rs`, and `method.rs`. The C `send_mime()` function at `libhttpd.c:597-670` is the single template for all response construction.

## Detailed Findings

### F1: Incomplete Response Headers (affects ~40 tests)

**C baseline** (`send_mime()` at `libhttpd.c:630-641`): Every HTTP response emits exactly 7 headers in this order: `Server`, `Content-Type`, `Date`, `Last-Modified`, `Accept-Ranges: bytes`, `Connection: close`, `Content-Length`. For non-2xx/non-3xx status codes, `Cache-Control: no-cache,no-store` is appended after the seventh header (`libhttpd.c:642-648`).

**Rust current** (`serve_static()` at `eventloop.rs:380-385`): Emits only 4 headers in wrong order: `Content-Type`, `Content-Length`, `Date`, `Server`. Missing: `Last-Modified`, `Accept-Ranges`, `Connection`. `build_error_response()` at `eventloop.rs:715-722` emits only `Content-Type` and `Content-Length` — missing 6 headers.

**Specific issues**:
- Server value: Rust uses `"thttpd-rs"` (`eventloop.rs:384`); C uses `"sthttpd/2.27.0 03oct2014"` (from `version.h:7` via `EXPOSED_SERVER_SOFTWARE`)
- Content-Type charset: Rust returns bare `"text/html"` from `mime_type()`; C substitutes `%s` in MIME type strings with configured charset (`iso-8859-1`) at `libhttpd.c:635-636`
- `Last-Modified` for error pages: C uses `mod = now` when `mod == 0` (`libhttpd.c:631-632`), so error responses get current timestamp
- Header order: C is `Server, Content-Type, Date, Last-Modified, Accept-Ranges, Connection, Content-Length`; Rust is `Content-Type, Content-Length, Date, Server`

**Fix location**: `serve_static()` at `eventloop.rs:380-395`, `build_error_response()` at `eventloop.rs:715-722`, `ResponseBuilder` call sites. Need a shared header-building helper that emits the standard 7-header block in C order.

### F2: HEAD Method Body Suppression (affects `static.head_text_file`, `edge.head_request`)

**C baseline** (`really_start_request()` at `libhttpd.c:3820-3825`): When `method == METHOD_HEAD`, calls `send_mime()` with `hc->sb.st_size` as length (emitting `Content-Length: <size>`) but never calls `mmc_map()` — `file_address` stays NULL. The main loop at `thttpd.c:1677-1686` detects NULL `file_address` and calls `finish_connection()` which sends only the buffered headers (no body).

**Rust current** (`serve_static()` at `eventloop.rs:323-395`): Always calls `server.mmc.map()` and `mmap.to_vec()`. No method check. HEAD requests receive full body (13 bytes), but baseline expects `body_length: 0`.

**Fix location**: `serve_static()` — add `if method == Method::Head` branch before mmap. Get file size via `std::fs::metadata()`, build response with `Content-Length: <size>` but empty body.

### F3: If-Modified-Since / 304 Not Modified (affects `static.if_modified_since_304`, `static.if_modified_since_200`)

**C baseline** (`httpd_parse_request()` at `libhttpd.c:2134-2139`): Parses `If-Modified-Since:` header via `tdate_parse()`, stores in `hc->if_modified_since`. In `really_start_request()` at `libhttpd.c:3826-3831`, if `if_modified_since != -1 && if_modified_since >= st_mtime`, returns 304 with `length = -1` (no `Content-Length`, no body).

**Rust current**: No `If-Modified-Since` parsing in `process_request()` (`eventloop.rs:259-261`). The `HttpConn.if_modified_since` field exists at `conn.rs:55` but is never populated.

**Fix location**: `process_request()` — extract `If-Modified-Since` header, parse with `thttpd_tdate::parse_http_date()`, store in `slot.http.if_modified_since`. In `serve_static()`, compare against file mtime, return 304 if `ims >= mtime`.

### F4: Range Requests / 206 Partial Content (affects `static.range_request`)

**C baseline** (`httpd_parse_request()` at `libhttpd.c:2147-2173`): Parses `Range: bytes=N-M` header, sets `hc->got_range`, `hc->first_byte_index`, `hc->last_byte_index`. In `send_mime()` at `libhttpd.c:613-628`, if `status == 200 && got_range && valid range && not whole file`, upgrades to 206 with `Content-Range: bytes N-M/total`. Body is the slice `mmap[first..=last]`.

**Rust current**: No `Range` header parsing. No range-related fields in `HttpConn`.

**Fix location**: Add `got_range: bool`, `first_byte_index: i64`, `last_byte_index: i64`, `range_if: Option<i64>` to `HttpConn` at `conn.rs`. Parse `Range` header in `process_request()`. In `serve_static()`, detect partial content conditions, serve byte slice with 206 status and `Content-Range` header.

### F5: HTTP/0.9 Raw Response (affects `edge.http09_simple`)

**C baseline** (`httpd_parse_request()` at `libhttpd.c:1952-1954`): When request line has only 2 tokens (no HTTP version), sets `hc->mime_flag = 0`. `send_mime()` at `libhttpd.c:611` checks `if (hc->mime_flag)` — when false, skips ALL header generation including status line. Response is raw body bytes only.

**Rust current**: `process_request()` at `eventloop.rs:190` defaults to `"HTTP/1.0"` when version token is absent. `ResponseBuilder::build()` at `response.rs:50-57` always emits `HTTP/1.0 {status} {text}\r\n` status line + headers + `\r\n` separator. No mechanism to suppress HTTP framing.

**Fix location**: Add `mime_flag: bool` field to `HttpConn`. In `process_request()`, detect 2-token request line (version was `None`), set `mime_flag = false`. In `ResponseBuilder`, add `build_raw()` method that returns only body bytes. Route through `serve_static()` and `build_error_response()`.

### F6: Unknown Method / 501 (affects `errors.501_not_implemented`, `malformed.invalid_method`)

**C baseline** (`httpd_parse_request()` at `libhttpd.c:2001-2013`): Compares method against GET/HEAD/POST via `strcasecmp()`. If none match, calls `httpd_send_err(hc, 501, ...)` with the method name embedded in the error body.

**Rust current**: `parse_method()` at `method.rs:13-28` returns `Method::Unknown` for unrecognized methods, but `process_request()` at `eventloop.rs:267` never checks the result. Unknown methods proceed to file serving as if they were GET.

**Fix location**: In `process_request()`, immediately after `parse_method()`, add `if slot.http.method == Method::Unknown` → build 501 error response with method name in body, return early.

### F7: Symlink Escape Prevention (affects `errors.403_symlink_escape`, `static.get_symlink`)

**C baseline** (`expand_symlinks()` at `libhttpd.c:1430-1660` + containment check at `libhttpd.c:2337-2361`): Resolves symlinks component-by-component via `readlink()`. After resolution, checks `strncmp(expnfilename, hs->cwd, strlen(hs->cwd))` — if resolved path doesn't start with the web root, returns 403.

**Rust current** (`normalize_path()` at `url.rs:38-60`): String-level normalization only (splits on `/`, resolves `.` and `..`). No filesystem resolution. No containment check.

**Fix location**: In `process_request()`, after building `file_path`, call `std::fs::canonicalize()` on both `file_path` and `server.config.dir`. Verify canonical path starts with canonical root. Return 403 if escape detected.

### F8: Permission Denied vs Not Found (affects `errors.403_forbidden`, `errors.404_not_found`)

**C baseline** (`really_start_request()` at `libhttpd.c:3614-3625`): `stat()` file, check `S_IROTH | S_IXOTH` bits. If neither set, return 403. If file doesn't exist (stat fails), return 500 (C quirk — actually uses 500 for stat failure; the baseline has 404 for nonexistent files via a different path).

**Rust current** (`serve_static()` at `eventloop.rs:341-346`): All `mmc.map()` failures → 404. Cannot distinguish permission denied from not found.

**Fix location**: In `serve_static()`, before mmap, call `std::fs::metadata()`. If `NotFound` → 404. If file exists but not world-readable (check Unix mode bits `0o004 | 0o001`) → 403.

### F9: CGI Output Passthrough (affects `cgi.simple_cgi`, `cgi.query_string`, `cgi.post_body`, `cgi.env_variables`, `cgi.fail_script`, `cgi.content_length`, `cgi.path_info`)

**C baseline** (`cgi_interpose_output()` at `libhttpd.c:3208-3348`): Reads CGI stdout, finds `\r\n\r\n` or `\n\n` separator, extracts Status/Location headers for response code, then writes `HTTP/1.0 <code> <title>\r\n` + ALL raw CGI output bytes (headers + separator + body) directly to socket. CGI scripts using `echo` produce `\n` line endings, so the response lacks `\r\n\r\n` between status line and body.

**Rust current** (`dispatch_cgi()` at `eventloop.rs:353-365` + `parse_cgi_output()` at `eventloop.rs:373-390`): Parses CGI output into structured headers, then re-encodes via `ResponseBuilder::build()` which adds proper `\r\n` line endings. This produces different bytes than C's raw passthrough.

**Fix location**: Rewrite `dispatch_cgi()` to: (1) scan for separator, (2) extract status code from Status/Location headers, (3) build response as `HTTP/1.0 <code> <title>\r\n` + raw CGI output bytes appended verbatim. Do NOT use `ResponseBuilder` for the body portion.

### F10: CGI Environment Variables (affects `cgi.env_variables`)

**C baseline** (`make_envp()` at `libhttpd.c:3002-3081`):
- `SERVER_SOFTWARE` = `"sthttpd/2.27.0 03oct2014"` (from `version.h:8`)
- `SERVER_NAME` = result of `gethostname()` (returns `"desktop"` in test env)
- Env order: PATH first, then SERVER_SOFTWARE, SERVER_NAME, GATEWAY_INTERFACE, SERVER_PROTOCOL, SERVER_PORT, REQUEST_METHOD, PATH_INFO, PATH_TRANSLATED, SCRIPT_NAME, QUERY_STRING (conditional), REMOTE_ADDR, HTTP_*, CONTENT_TYPE, CONTENT_LENGTH, CGI_PATTERN

**Rust current** (`dispatch_cgi()` at `eventloop.rs:317-318` + `build_envp()` at `cgi.rs:46-111`):
- `server_software` = `"thttpd-rs/0.1"` — wrong
- `server_name` = `server.config.hostname` or `"localhost"` — should use `gethostname()`
- Env order starts with GATEWAY_INTERFACE — should start with PATH
- `QUERY_STRING` always emitted — should be conditional (only when non-empty)
- `remote_addr` includes port (`"127.0.0.1:49543"`) — should strip port to `"127.0.0.1"`
- Missing `CGI_PATTERN` env var
- HTTP_* headers sorted alphabetically — C preserves request order

**Fix location**: `dispatch_cgi()` — change `server_software` to `"sthttpd/2.27.0 03oct2014"`, change `server_name` to call `gethostname()`. `build_envp()` — reorder to match C, make QUERY_STRING conditional, strip port from remote_addr, add CGI_PATTERN.

### F11: CGI PATH_INFO Extraction (affects `cgi.path_info`)

**C baseline** (`expand_symlinks()` at `libhttpd.c:1430-1660`): When a trailing path component doesn't exist (e.g., `/extra/path` after `/cgi-bin/pathinfo.sh`), saves it as `hc->pathinfo = "extra/path"`. Then `make_envp()` at `libhttpd.c:3047-3055` constructs `PATH_INFO=/extra/path` and `PATH_TRANSLATED=<www_root>/extra/path`.

**Rust current**: No pathinfo extraction. `slot.http.path_info` exists but is never populated. URL `/cgi-bin/pathinfo.sh/extra/path` is treated as a whole path.

**Fix location**: In `process_request()`, when the full URL path doesn't exist on disk but CGI is enabled, iteratively strip trailing components until finding an existing file that matches the CGI pattern. Set `path_info` to the stripped suffix and `path_translated` to `www_root + path_info`.

### F12: CGI Not Found (affects `cgi.cgi_not_found`)

**C baseline**: Returns 404 with standard error headers when CGI script doesn't exist.

**Rust current** (`dispatch_cgi()` at `eventloop.rs:404-409`): Returns 500 Internal Server Error on CGI execution failure.

**Fix location**: In `dispatch_cgi()`, check if script exists before executing. If not, return 404.

### F13: Directory Listing Headers (affects `errors.directory_without_index`)

**Rust current** (`serve_static()` directory branch at `eventloop.rs:298-322`): Emits only `Content-Type: text/html` and `Content-Length`. Missing all standard headers (Server, Date, Last-Modified, Accept-Ranges, Connection) and charset suffix.

**Fix location**: Directory listing response builder — add all 7 standard headers matching `send_mime()` format.

### F14: Double Slash → 400 (affects `edge.double_slash`)

**Baseline**: `//test.txt` returns 400 Bad Request.

**Rust current**: `normalize_path()` collapses `//` to `/` and serves 200.

**Fix location**: In `normalize_path()` or `process_request()`, reject paths containing `//`.

### F15: Very Long URL → 500 (affects `edge.very_long_url`)

**Baseline**: Very long path returns 500 Internal Error.

**Rust current**: No URL length limit. Likely returns 404 or 200.

**Fix location**: In `process_request()`, check URL length against a limit and return 500 if exceeded.

## Code References

- `rust/crates/thttpd-core/src/eventloop.rs:247-420` — `serve_static()`, primary fix target
- `rust/crates/thttpd-core/src/eventloop.rs:183-310` — `process_request()`, header parsing and routing
- `rust/crates/thttpd-core/src/eventloop.rs:316-420` — `dispatch_cgi()`, CGI execution and output handling
- `rust/crates/thttpd-core/src/eventloop.rs:373-395` — `parse_cgi_output()`, CGI header/body split
- `rust/crates/thttpd-core/src/eventloop.rs:715-722` — `build_error_response()`, error page construction
- `rust/crates/thttpd-core/src/eventloop.rs:726-738` — `extract_header()`, header value extraction (reusable)
- `rust/crates/thttpd-http/src/response.rs:13-84` — `ResponseBuilder`, needs `build_raw()` method
- `rust/crates/thttpd-http/src/conn.rs:38-60` — `HttpConn`, needs range/mime_flag fields
- `rust/crates/thttpd-http/src/cgi.rs:46-111` — `build_envp()`, env ordering and values
- `rust/crates/thttpd-http/src/cgi.rs:13-44` — `CgiContext`, field values
- `rust/crates/thttpd-http/src/url.rs:38-60` — `normalize_path()`, needs `//` rejection
- `rust/crates/thttpd-http/src/method.rs:13-28` — `Method::from_str()`, case sensitivity
- `legacy/src/libhttpd.c:597-670` — `send_mime()`, the template for all response construction
- `legacy/src/libhttpd.c:3579-3847` — `really_start_request()`, HEAD/IMS/Range decision logic
- `legacy/src/libhttpd.c:1930-2180` — `httpd_parse_request()`, header parsing (IMS, Range, method)
- `legacy/src/libhttpd.c:3208-3348` — `cgi_interpose_output()`, CGI raw passthrough
- `legacy/src/libhttpd.c:3002-3081` — `make_envp()`, CGI environment construction
- `legacy/src/libhttpd.c:1430-1660` — `expand_symlinks()`, pathinfo decomposition
- `harness/golden/baseline.json` — 45 golden test cases with exact expected responses

## Integration Points

### Inbound References
- `rust/crates/thttpd-http/src/parse.rs:45-48` — FSM detects HTTP/0.9 (SecondWord state → GotRequest)
- `rust/crates/thttpd-http/src/parse.rs:73-109` — `got_request()` drives request detection into `process_request()`
- `rust/crates/thttpd-fdwatch/src/lib.rs` — poll events trigger `handle_read()` → `process_request()`
- `pipeline/run_differential.py` — differential test runner, compares Rust output against baseline.json

### Outbound Dependencies
- `rust/crates/thttpd-tdate/src/lib.rs:9` — `parse_http_date()` and `format_http_date()`, needed for IMS/Range
- `rust/crates/thttpd-mime/src/types.rs:12-38` — `mime_type()`, needs charset appending for text/* types
- `rust/crates/thttpd-mmc/src/lib.rs:68` — `MmapCache::map()`, currently maps all errors to FileNotFound
- `rust/crates/thttpd-match/src/lib.rs` — `match_pattern()`, CGI pattern matching

### Infrastructure Wiring
- `rust/crates/thttpd-http/src/conn.rs:38-60` — `HttpConn` struct, central state for request processing
- `rust/crates/thttpd-http/src/conn.rs:89` — `HttpConn::new()`, field initialization
- `rust/crates/thttpd-http/src/response.rs:13` — `ResponseBuilder`, all response construction flows through here

## Architecture Insights

1. **`send_mime()` is the single response gateway** in C — all responses (200, 206, 304, 400, 403, 404, 500, 501) flow through this one function. The Rust code should replicate this with a shared `build_response()` helper that accepts `(status, title, content_type, length, mtime, ranges)` and constructs the complete header block.

2. **Three-way branching in `really_start_request()`** — The C code uses `if (HEAD) ... else if (IMS) ... else (GET+Range)` at `libhttpd.c:3820-3843`. This ordering is load-bearing: HEAD requests must NOT trigger 304, and the Range upgrade from 200→206 happens inside `send_mime()` after the branch.

3. **CGI raw passthrough vs. re-encoding** — The golden capture's response parser expects the C binary's wire format where CGI output retains `\n` line endings. The Rust code must NOT use `ResponseBuilder` for CGI body — instead append raw bytes after the status line.

4. **`mime_flag` gates all HTTP framing** — A single boolean determines whether any HTTP framing is emitted. This affects both file responses AND error responses. Adding this to `HttpConn` and threading it through all response paths is a cross-cutting concern.

5. **Error page headers match success headers** — In C, error responses go through `send_mime()` with the same 7-header template (using `mod=now` for Last-Modified). The Rust `build_error_response()` must be upgraded to emit the full header set.

6. **`expand_symlinks()` does double duty** — It both validates security (containment within www_root) and extracts pathinfo (trailing non-existent components). The Rust implementation needs both behaviors, achievable via `canonicalize()` for security and iterative path probing for pathinfo.

## Precedents & Lessons

git history unavailable (no-commit state).

### Composite Lessons
- The C `send_mime()` function at `libhttpd.c:597-670` is the authoritative reference for ALL response format questions. When in doubt, trace the C code path rather than guessing.
- The golden capture harness (`run_differential.py`) compares 8 fields: `status_code`, `status_text`, `header_count`, `header_order`, `header_values`, `body_sha256`, `body_length`, `connection_result`. Header ORDER matters, not just presence.
- CGI test expectations in baseline.json have an unusual format (body content in header values) because the golden capture parser can't find `\r\n\r\n` in C's raw passthrough output. The Rust must produce the same parser-defeating format to match.

## Historical Context (from `.rpiv/artifacts/`)
- `.rpiv/artifacts/EXECUTION_PLAN.md` — Phase 4 verification plan (differential testing)
- `.rpiv/artifacts/PLAN.md` — Overall migration plan (C→Rust)

## Developer Context

(No developer checkpoint questions were needed — all findings were grounded in C/Rust code comparison and baseline analysis.)

## Related Research

(None — this is the first research artifact for this specific task.)

## Open Questions

1. **HTTP method case sensitivity**: The C code uses `strcasecmp()` for method matching (case-insensitive). The Rust `Method::from_str()` only matches exact uppercase. Should matching be case-insensitive? The baseline tests use uppercase methods only, so this may not matter for passing tests, but it's a correctness concern.

2. **Error body format**: The C error pages include `<HR>\n<ADDRESS>...` footer that the Rust `error_page()` function doesn't generate. Need to verify whether the baseline checks error body SHA-256 or just body_length. If SHA-256 is checked, the HTML template must match exactly.

3. **`malformed.binary_garbage` handling**: This test expects `200 OK` with empty headers and `body_length: 0` — likely because the binary garbage causes the C FSM to produce an empty response. The Rust FSM behavior for binary garbage needs investigation.

4. **`malformed.very_long_header` and `malformed.negative_content_length`**: These tests expect 200 OK with standard file serving. The C code appears to ignore malformed Content-Length headers and serve files normally. Need to verify the Rust parser handles these edge cases.
