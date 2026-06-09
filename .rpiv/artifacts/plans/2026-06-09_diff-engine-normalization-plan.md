# Plan: Diff Engine Normalization

Date: 2026-06-09
Status: Planned
Blocks: 36 differential test "failures" that are false negatives

## Problem

The diff engine (`harness/diff_engine.py`) compares golden baseline responses
against live Rust server responses using **exact equality** on all 8 fields.
This produces false negatives whenever a field contains a value that legitimately
differs between runs:

| Field | Why it differs | Example |
|-------|---------------|---------|
| `Date` header | Captured at different wall-clock times | `Tue, 09 Jun 2026 03:20:35 GMT` vs `Fri, 09 Jun 2026 15:05:07 GMT` |
| `Last-Modified` header | File mtime = directory creation time (varies per run) | `03:20:34` vs `15:04:55` |
| `header_values` (dict) | Contains `Date`/`Last-Modified` as values | All static/error/header/connection tests (32 tests) |
| `body_length` | Directory listings embed file mtimes; error pages embed URLs with tempdir paths | `errors.directory_without_index`: 432 vs 301 |
| `body_sha256` | Includes the timestamp-bearing body | Not currently checked (only in `--strict` mode) |
| `SERVER_PORT` in CGI env | Dynamically allocated port | `49543` vs `36777` |
| `HTTP_HOST` in CGI env | Includes dynamic port (Rust) vs bare IP (C baseline) | `127.0.0.1:36777` vs `127.0.0.1` |
| `PATH_TRANSLATED` in CGI env | Includes temp directory path | `/tmp/thttpd_golden_qz73fauz/...` vs `/tmp/thttpd_diff_XXXX/...` |

### Current false-negative count: 36 of 45 tests

All 36 have structurally correct responses. The mismatches are exclusively in
fields that are inherently non-deterministic across runs.

## Solution: Field-Level Normalization

Replace exact-value comparison with **normalized comparison** on fields that
contain non-deterministic content. The normalization strategy varies by field.

### Design

```
┌─────────────────────────────────────────────────────────────────┐
│  compare_responses(expected, actual)                            │
│                                                                 │
│  1. Parse both responses into structured dicts (unchanged)      │
│  2. For each comparison field:                                  │
│     a. If field has a normalizer → apply normalizer to both     │
│     b. Compare normalized values with exact equality            │
│     c. Report match/mismatch on normalized values               │
│  3. Return results with both raw and normalized values          │
│                                                                 │
│  Normalizers:                                                   │
│    status_code    → identity                                    │
│    status_text    → identity                                    │
│    header_count   → identity                                    │
│    header_order   → identity                                    │
│    header_values  → normalize_timestamps + normalize_paths      │
│    body_sha256    → identity (only checked in --strict)         │
│    body_length    → normalize_for_dirlist + normalize_for_error │
│    connection     → identity                                    │
└─────────────────────────────────────────────────────────────────┘
```

### Normalizer 1: Timestamp Fields

**Applies to**: `Date`, `Last-Modified` header values

**Strategy**: Validate format, ignore value.

For each of these header keys, instead of comparing the value strings:
1. Assert the value matches the RFC 1123 format: `^[A-Z][a-z]{2}, \d{2} [A-Z][a-z]{2} \d{4} \d{2}:\d{2}:\d{2} GMT$`
2. If both expected and actual match the format → mark as `match`
3. If either doesn't match the format → compare raw values (real mismatch)

**Implementation**:
```python
TIMESTAMP_RE = re.compile(
    r'^[A-Z][a-z]{2}, \d{2} [A-Z][a-z]{2} \d{4} \d{2}:\d{2}:\d{2} GMT$'
)

TIMESTAMP_HEADERS = {"Date", "Last-Modified"}

def normalize_header_values(expected_headers, actual_headers):
    """Compare header dicts, treating timestamp headers as format-only matches."""
    mismatches = {}
    all_keys = set(expected_headers.keys()) | set(actual_headers.keys())
    for key in all_keys:
        exp = expected_headers.get(key)
        act = actual_headers.get(key)
        if exp is None or act is None:
            mismatches[key] = (exp, act)
            continue
        if key in TIMESTAMP_HEADERS:
            # Format-only comparison
            if TIMESTAMP_RE.match(exp) and TIMESTAMP_RE.match(act):
                continue  # both valid timestamps → match
        if exp != act:
            mismatches[key] = (exp, act)
    return mismatches
```

**Tests affected**: 32 tests (all static, error, header, connection, edge, malformed tests)

### Normalizer 2: Temp Directory Path Substitution

**Applies to**: Any header value or body content containing a temp directory path.

**Strategy**: Replace temp directory paths with a canonical placeholder.

The golden baseline and diff runner both create temp directories with predictable
prefixes (`/tmp/thttpd_golden_XXXXXX` and `/tmp/thttpd_diff_XXXXXX`). Before
comparing, replace both with a canonical token.

**Implementation**:
```python
TEMP_DIR_RE = re.compile(r'/tmp/thttpd_(?:golden|diff)_[a-zA-Z0-9_]+')

def normalize_paths(value):
    """Replace temp directory paths with a canonical placeholder."""
    if isinstance(value, str):
        return TEMP_DIR_RE.sub('/tmp/thttpd_NORMALIZED', value)
    return value
```

Apply to `header_values` dict values and to body content before `body_length` and
`body_sha256` comparison.

**Tests affected**: `cgi.path_info` (PATH_TRANSLATED), `errors.directory_without_index`
(directory listing body contains file paths)

### Normalizer 3: Dynamic Port Normalization

**Applies to**: `SERVER_PORT`, `HTTP_HOST` in CGI output headers.

**Strategy**: These appear inside CGI response bodies (the CGI script prints its
environment). The header `Content-Type` value contains the full CGI output
including these values.

For CGI tests specifically, the response body is the CGI script's output which
includes env vars like `SERVER_PORT=12345`. Since the port is dynamically
allocated, we normalize it.

**Implementation**:
```python
PORT_RE = re.compile(r'(SERVER_PORT|HTTP_HOST|Host)=([^\n]*)')

def normalize_cgi_output(value):
    """Normalize dynamic port values in CGI output."""
    if isinstance(value, str):
        # Replace port numbers in env var lines
        value = re.sub(r'SERVER_PORT=\d+', 'SERVER_PORT=PORT', value)
        value = re.sub(r'HTTP_HOST=127\.0\.0\.1:\d+', 'HTTP_HOST=127.0.0.1:PORT', value)
        value = re.sub(r'HTTP_HOST=127\.0\.0\.1\n', 'HTTP_HOST=127.0.0.1\n', value)
        # Normalize Host header with port
        value = re.sub(r'Host=127\.0\.0\.1:\d+', 'Host=127.0.0.1:PORT', value)
    return value
```

Apply this only to CGI test responses (identified by test name prefix `cgi.`).

**Tests affected**: `cgi.env_variables`, `cgi.path_info`

### Normalizer 4: Body Length Tolerance for Directory Listings

**Applies to**: `errors.directory_without_index`

**Strategy**: Directory listing body content varies because:
- File metadata (size, nlink) differs between tempdir instances
- File timestamps differ between runs
- Directory entries include `.` and `..` whose metadata depends on the filesystem

Instead of exact body_length match, verify that:
1. Status code is 200
2. Content-Type is `text/html`
3. Body contains the expected structural markers (`<PRE>`, `mode  links`, entry names)

**Implementation**: Add a `body_structure_check` that verifies the listing contains
expected entries by name, rather than byte-for-byte comparison.

**Tests affected**: `errors.directory_without_index`

## Implementation Steps

### Step 1: Add normalizer functions to `harness/diff_engine.py`

Add the 4 normalizer functions as described above. Each is a pure function that
takes an expected/actual pair and returns a normalized pair.

### Step 2: Create `compare_responses_v2()` in `harness/diff_engine.py`

New comparison function that applies normalizers before checking equality. Keeps
the original `compare_responses()` for backward compatibility and `--strict` mode.

```python
def compare_responses_v2(expected, actual, test_name=""):
    """Compare with normalization for non-deterministic fields."""
    results = []

    # Status code — exact match
    results.append(field_result("status_code", expected["status_code"], actual["status_code"]))

    # Status text — exact match
    results.append(field_result("status_text", expected["status_text"], actual["status_text"]))

    # Header count — exact match
    results.append(field_result("header_count", len(expected["headers"]), len(actual["headers"])))

    # Header order — exact match
    results.append(field_result("header_order",
        list(expected["headers"].keys()),
        list(actual["headers"].keys())))

    # Header values — normalized comparison
    exp_hdrs = normalize_headers(expected["headers"])
    act_hdrs = normalize_headers(actual["headers"])
    results.append(field_result("header_values", exp_hdrs, act_hdrs))

    # Body SHA-256 — only in strict mode
    results.append(field_result("body_sha256",
        expected["body_sha256"], actual["body_sha256"]))

    # Body length — normalized for CGI and directory listings
    exp_len = normalize_body_length(expected, test_name)
    act_len = normalize_body_length(actual, test_name)
    results.append(field_result("body_length", exp_len, act_len))

    # Connection result — exact match
    results.append(field_result("connection_result",
        expected["connection_result"], actual["connection_result"]))

    return results
```

### Step 3: Update `pipeline/run_differential.py` to use v2 comparison

Change the `compare_responses` call to `compare_responses_v2`, passing the test
name for CGI/directory-specific normalization.

### Step 4: Add `--strict` flag to disable normalization

When `--strict` is passed, use the original `compare_responses()` for
byte-exact comparison. This is useful for regression testing against a known-good
baseline captured in the same session.

### Step 5: Update golden capture to store normalization hints

In `pipeline/run_golden_capture.py`, add metadata to each baseline entry:
```json
{
    "test_name": "cgi.env_variables",
    "normalization_hints": ["timestamp", "port", "tempdir"],
    ...
}
```

This allows the diff engine to know which normalizers to apply without hardcoding
test-name prefixes.

### Step 6: Verify all tests pass with normalization

Run the differential test suite with the normalized comparison:
```bash
python3 pipeline/run_differential.py --baseline harness/golden/baseline.json
```

Expected result: 40-42/45 passing (the 3-5 remaining would be genuinely different
behaviors still needing implementation fixes).

## Acceptance Criteria

- [ ] All timestamp-bearing tests pass (Date, Last-Modified normalized)
- [ ] `cgi.env_variables` passes (SERVER_PORT, HTTP_HOST normalized)
- [ ] `cgi.path_info` passes (PATH_TRANSLATED normalized)
- [ ] `errors.directory_without_index` passes (listing structure verified)
- [ ] `--strict` mode retains exact comparison for regression testing
- [ ] Normalizer functions have unit tests in `harness/test_diff_engine.py`
- [ ] No normalizer changes the behavior of non-matching fields (identity for deterministic values)

## Risks and Mitigations

| Risk | Mitigation |
|------|-----------|
| Over-normalization hides real bugs | `--strict` flag retains exact comparison. Normalizers only apply to known non-deterministic fields. |
| Timestamp regex misses edge cases | Test against real C thttpd output. Fail closed: if format doesn't match, compare raw values. |
| Path normalization too aggressive | Only match `/tmp/thttpd_(golden\|diff)_` prefix, not arbitrary paths. |
| CGI port normalization breaks if server binds non-localhost | Normalize by pattern (`:\d+` after known IP), not by fixed value. |
