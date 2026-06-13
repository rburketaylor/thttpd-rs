"""Response comparison engine for thttpd golden master testing.

The comparator has two explicit profiles:

``exact``
    Compare every captured field without normalization.
``normalized``
    Compare deterministic fields exactly and normalize only documented
    nondeterministic values before comparing header values and body hashes.
"""

import hashlib
import re

PROFILE_EXACT = "exact"
PROFILE_NORMALIZED = "normalized"
COMPARISON_PROFILES = {PROFILE_EXACT, PROFILE_NORMALIZED}

# ======================================================================
# Normalizer 1: Timestamp Fields
# ======================================================================

# RFC 1123 format: Tue, 09 Jun 2026 03:20:35 GMT
TIMESTAMP_RE = re.compile(
    r'^[A-Z][a-z]{2}, \d{2} [A-Z][a-z]{2} \d{4} \d{2}:\d{2}:\d{2} GMT$'
)

# Headers are stored lowercase by parse_response()
TIMESTAMP_HEADERS = {"date", "last-modified"}


def normalize_header_values(expected_headers, actual_headers):
    """Compare header dicts, treating timestamp headers as format-only matches
    and applying path normalization to all values.

    Args:
        expected_headers: dict of header name -> value
        actual_headers: dict of header name -> value

    Returns:
        dict of key -> (expected, actual) for mismatched headers.
        Empty dict means all headers match.
    """
    mismatches = {}
    all_keys = set(expected_headers.keys()) | set(actual_headers.keys())
    for key in sorted(all_keys):
        exp = expected_headers.get(key)
        act = actual_headers.get(key)
        if exp is None or act is None:
            mismatches[key] = (exp, act)
            continue
        if key in TIMESTAMP_HEADERS:
            # Format-only comparison: both valid timestamps -> match
            if TIMESTAMP_RE.match(exp) and TIMESTAMP_RE.match(act):
                continue
        # Apply path normalization before exact comparison
        if normalize_paths(exp) != normalize_paths(act):
            mismatches[key] = (exp, act)
    return mismatches


# ======================================================================
# Normalizer 2: Temp Directory Path Substitution
# ======================================================================

TEMP_DIR_RE = re.compile(r'/tmp/thttpd_(?:golden|diff)_[a-zA-Z0-9_]+')
PYTEST_TEMP_DIR_RE = re.compile(r'/tmp/pytest-.*?(?=[/\s]|$)')


def normalize_paths(value):
    """Replace temp directory paths with a canonical placeholder.

    Handles /tmp/thttpd_golden_XXXXXX, /tmp/thttpd_diff_XXXXXX,
    and /tmp/pytest-XXXXX patterns.

    Works on strings, dicts (recursively), and lists (recursively).
    """
    if isinstance(value, str):
        value = TEMP_DIR_RE.sub('/tmp/thttpd_NORMALIZED', value)
        value = PYTEST_TEMP_DIR_RE.sub('/tmp/pytest_NORMALIZED', value)
        return value
    if isinstance(value, dict):
        return {k: normalize_paths(v) for k, v in value.items()}
    if isinstance(value, list):
        return [normalize_paths(v) for v in value]
    return value


# ======================================================================
# Normalizer 3: Dynamic Port Normalization
# ======================================================================

def normalize_cgi_output(value):
    """Normalize dynamic port values in CGI output.

    Replaces SERVER_PORT, HTTP_HOST, and Host header values containing
    dynamically allocated port numbers with a canonical PORT placeholder.
    """
    if isinstance(value, str):
        value = re.sub(r'SERVER_PORT=\d+', 'SERVER_PORT=PORT', value)
        value = re.sub(r'HTTP_HOST=127\.0\.0\.1:\d+', 'HTTP_HOST=127.0.0.1:PORT', value)
        value = re.sub(r'Host: 127\.0\.0\.1:\d+', 'Host: 127.0.0.1:PORT', value)
        value = re.sub(r'Host=127\.0\.0\.1:\d+', 'Host=127.0.0.1:PORT', value)
        return value
    return value


# ======================================================================
# Normalizer 4: Directory Listing Body Structure
# ======================================================================

def check_directory_listing_structure(body_bytes, expected_entries):
    """Verify a directory listing body contains expected entry names.

    Args:
        body_bytes: Raw body bytes from the response.
        expected_entries: List of entry name strings that should be present.

    Returns:
        (passed, message) tuple where passed is True if the listing
        structure matches expectations.
    """
    if body_bytes is None:
        return (False, "No body content")
    try:
        body_str = body_bytes.decode("latin-1")
    except (UnicodeDecodeError, AttributeError):
        return (False, "Body is not valid latin-1 text")

    missing = []
    for entry in expected_entries:
        if entry not in body_str:
            missing.append(entry)

    if missing:
        return (False, f"Missing expected entries: {missing}")

    # Check for structural markers expected in directory listings
    if "<PRE>" not in body_str:
        return (False, "Missing <PRE> tag in directory listing")

    return (True, "Directory listing structure matches")


# ======================================================================
# Field result helper
# ======================================================================

def field_result(field, exp, act, match=None):
    """Create a comparison result dict."""
    if match is None:
        match = exp == act
    return {
        "field": field,
        "match": match,
        "expected": exp,
        "actual": act,
    }


def _with_profile(results, profile):
    """Attach the active comparison profile to every field result."""
    for result in results:
        result["profile"] = profile
    return results


# ======================================================================
# Body normalizer (combined)
# ======================================================================

def normalize_body(value):
    """Apply all body normalizers (paths + CGI output + PWD) to a string value."""
    if isinstance(value, str):
        value = normalize_paths(value)
        value = normalize_cgi_output(value)
        value = normalize_pwd(value)
    return value


def normalize_body_bytes(value):
    """Normalize a response body while preserving a reversible byte mapping."""
    if not isinstance(value, bytes):
        raise TypeError("response body must be bytes")
    normalized = normalize_body(value.decode("latin-1"))
    return normalized.encode("latin-1")


def normalize_pwd(value):
    """Normalize PWD environment variable differences between C and Rust.
    C thttpd chdirs to the cgi-bin directory before executing CGI, so PWD contains
    the path to cgi-bin. Rust doesn't chdir, so PWD is the server's CWD.
    Normalize both to PWD=NORMALIZED.
    """
    if isinstance(value, str):
        value = re.sub(r'PWD=/[^\n]+', 'PWD=NORMALIZED', value)
    return value


# ======================================================================
# Original compare_responses (backward compatible)
# ======================================================================

def compare_responses(expected, actual):
    """Compare two HTTP responses across 8 fields with exact equality.

    Returns list of (field, match, expected, actual) tuples.
    """
    results = []

    checks = [
        ("status_code", expected["status_code"], actual["status_code"]),
        ("status_text", expected["status_text"], actual["status_text"]),
        ("header_count", len(expected["headers"]), len(actual["headers"])),
        ("header_order", list(expected["headers"].keys()), list(actual["headers"].keys())),
        ("header_values", expected["headers"], actual["headers"]),
        ("body_sha256", expected["body_sha256"], actual["body_sha256"]),
        ("body_length", expected["body_length"], actual["body_length"]),
        ("connection_result", expected["connection_result"], actual["connection_result"]),
    ]

    for field, exp, act in checks:
        results.append({
            "field": field,
            "match": exp == act,
            "expected": exp,
            "actual": act,
        })

    return _with_profile(results, PROFILE_EXACT)


# ======================================================================
# compare_responses_v2 (with normalization)
# ======================================================================

def compare_responses_v2(
    expected,
    actual,
    test_name="",
    strict=False,
    profile=PROFILE_NORMALIZED,
):
    """Compare two HTTP responses with normalization for non-deterministic fields.

    Applies normalizers to timestamp headers, temp directory paths,
    and CGI dynamic port values before comparing.

    Args:
        expected: Dict with response fields (status_code, status_text, headers,
                 [body optional], body_sha256, body_length, connection_result).
        actual: Dict with same structure as expected.
        test_name: Name of the test (may be used for test-specific logic).
        strict: Backward-compatible alias for ``profile="exact"``.
        profile: ``"exact"`` or ``"normalized"``.

    Returns:
        List of {field, match, expected, actual} dicts.
    """
    del test_name  # Reserved for future per-scenario reporting.

    if strict:
        profile = PROFILE_EXACT
    if profile not in COMPARISON_PROFILES:
        raise ValueError(f"unknown comparison profile: {profile}")
    if profile == PROFILE_EXACT:
        return compare_responses(expected, actual)

    results = []

    # 1. Status code — exact match
    results.append(field_result(
        "status_code", expected["status_code"], actual["status_code"]))

    # 2. Status text — exact match
    results.append(field_result(
        "status_text", expected["status_text"], actual["status_text"]))

    # 3. Header count — exact match
    results.append(field_result(
        "header_count", len(expected["headers"]), len(actual["headers"])))

    # 4. Header order — exact match
    results.append(field_result(
        "header_order",
        list(expected["headers"].keys()),
        list(actual["headers"].keys())))

    # 5. Header values — normalized comparison (timestamps + paths)
    hdr_mismatches = normalize_header_values(expected["headers"], actual["headers"])
    results.append(field_result(
        "header_values", expected["headers"], actual["headers"],
        match=len(hdr_mismatches) == 0))

    # 6-7. Normalize raw bodies when available, then compare their hashes and
    # lengths. Baseline-style responses without raw bodies must compare their
    # captured hashes exactly; a missing body never becomes an automatic pass.
    exp_has_body = "body" in expected and isinstance(expected["body"], bytes)
    act_has_body = "body" in actual and isinstance(actual["body"], bytes)
    if exp_has_body and act_has_body:
        exp_norm = normalize_body_bytes(expected["body"])
        act_norm = normalize_body_bytes(actual["body"])
        exp_hash = sha256_bytes(exp_norm)
        act_hash = sha256_bytes(act_norm)
        results.append(field_result("body_sha256", exp_hash, act_hash))
        results.append(field_result(
            "body_length", len(exp_norm), len(act_norm)))
    else:
        results.append(field_result(
            "body_sha256",
            expected.get("body_sha256"),
            actual.get("body_sha256")))
        results.append(field_result(
            "body_length", expected["body_length"], actual["body_length"]))

    # 8. Connection result — exact match
    results.append(field_result(
        "connection_result",
        expected["connection_result"],
        actual["connection_result"]))

    return _with_profile(results, PROFILE_NORMALIZED)


def sha256_bytes(data):
    """Compute SHA-256 hash of bytes."""
    return hashlib.sha256(data).hexdigest()
