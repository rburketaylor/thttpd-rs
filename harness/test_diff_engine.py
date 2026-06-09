"""Unit tests for diff_engine normalizers and comparison functions."""

import hashlib

from harness.diff_engine import (
    TIMESTAMP_RE,
    TIMESTAMP_HEADERS,
    normalize_header_values,
    TEMP_DIR_RE,
    PYTEST_TEMP_DIR_RE,
    normalize_paths,
    normalize_cgi_output,
    check_directory_listing_structure,
    normalize_body,
    compare_responses,
    compare_responses_v2,
    field_result,
)


# ======================================================================
# Helper
# ======================================================================

def make_response_dict(headers=None, body=None, status_code=200,
                       status_text="OK", connection_result="ok",
                       **overrides):
    """Build a response dict matching the structure from parse_response()."""
    if headers is None:
        headers = {}
    body_bytes = body if isinstance(body, bytes) else (body.encode("latin-1") if body else b"")
    resp = {
        "status_code": status_code,
        "status_text": status_text,
        "headers": headers,
        "body": body_bytes,
        "body_sha256": hashlib.sha256(body_bytes).hexdigest(),
        "body_length": len(body_bytes),
        "connection_result": connection_result,
    }
    resp.update(overrides)
    return resp


# ======================================================================
# Tests: TIMESTAMP_RE
# ======================================================================

class TestTimestampRe:
    """Verify RFC 1123 timestamp format matching."""

    def test_valid_rfc1123(self):
        assert TIMESTAMP_RE.match("Tue, 09 Jun 2026 03:20:35 GMT")

    def test_valid_rfc1123_feb(self):
        assert TIMESTAMP_RE.match("Fri, 02 Feb 2024 12:00:00 GMT")

    def test_valid_rfc1123_dec(self):
        assert TIMESTAMP_RE.match("Mon, 25 Dec 2023 00:00:00 GMT")

    def test_invalid_format(self):
        assert not TIMESTAMP_RE.match("Tuesday, 09-Jun-2026 03:20:35 GMT")

    def test_invalid_extra_chars(self):
        assert not TIMESTAMP_RE.match("Tue, 09 Jun 2026 03:20:35 GMT extra")

    def test_invalid_empty(self):
        assert not TIMESTAMP_RE.match("")

    def test_not_timestamp(self):
        assert not TIMESTAMP_RE.match("text/html")


# ======================================================================
# Tests: TIMESTAMP_HEADERS
# ======================================================================

class TestTimestampHeadersSet:
    """Verify TIMESTAMP_HEADERS contains expected header names (lowercase)."""

    def test_contains_date(self):
        assert "date" in TIMESTAMP_HEADERS

    def test_contains_last_modified(self):
        assert "last-modified" in TIMESTAMP_HEADERS

    def test_case_sensitivity(self):
        # Headers stored lowercase by parse_response()
        assert "Date" not in TIMESTAMP_HEADERS


# ======================================================================
# Tests: normalize_header_values
# ======================================================================

class TestNormalizeHeaderValues:
    """Test header value comparison with timestamp and path normalization."""

    def test_timestamp_match(self):
        """Both valid RFC 1123 timestamps -> match."""
        exp = {"date": "Tue, 09 Jun 2026 03:20:35 GMT"}
        act = {"date": "Fri, 09 Jun 2026 15:05:07 GMT"}
        assert normalize_header_values(exp, act) == {}

    def test_timestamp_mismatch(self):
        """One invalid timestamp -> real mismatch."""
        exp = {"date": "Tue, 09 Jun 2026 03:20:35 GMT"}
        act = {"date": "not-a-timestamp"}
        mismatches = normalize_header_values(exp, act)
        assert "date" in mismatches

    def test_timestamp_both_invalid_same(self):
        """Both invalid but same value -> match."""
        exp = {"date": "INVALID_DATE"}
        act = {"date": "INVALID_DATE"}
        assert normalize_header_values(exp, act) == {}

    def test_timestamp_both_invalid_different(self):
        """Both invalid and different -> mismatch."""
        exp = {"date": "INVALID_DATE_A"}
        act = {"date": "INVALID_DATE_B"}
        mismatches = normalize_header_values(exp, act)
        assert "date" in mismatches

    def test_last_modified_match(self):
        """Both valid Last-Modified timestamps -> match."""
        exp = {"last-modified": "Tue, 09 Jun 2026 03:20:34 GMT"}
        act = {"last-modified": "Tue, 09 Jun 2026 15:04:55 GMT"}
        assert normalize_header_values(exp, act) == {}

    def test_regular_header_mismatch(self):
        """Non-timestamp header with different values -> mismatch."""
        exp = {"content-type": "text/html"}
        act = {"content-type": "text/plain"}
        mismatches = normalize_header_values(exp, act)
        assert "content-type" in mismatches

    def test_regular_header_match(self):
        """Non-timestamp header with same values -> match."""
        exp = {"content-type": "text/html"}
        act = {"content-type": "text/html"}
        assert normalize_header_values(exp, act) == {}

    def test_path_normalized_in_header(self):
        """Headers with temp directory paths -> match after normalization."""
        exp = {"location": "/tmp/thttpd_golden_qz73fauz/file.txt"}
        act = {"location": "/tmp/thttpd_diff_abc12345/file.txt"}
        assert normalize_header_values(exp, act) == {}

    def test_missing_key_in_expected(self):
        """Key present in actual but not expected -> mismatch."""
        exp = {"content-type": "text/html"}
        act = {"content-type": "text/html", "date": "Tue, 09 Jun 2026 03:20:35 GMT"}
        mismatches = normalize_header_values(exp, act)
        assert "date" in mismatches

    def test_missing_key_in_actual(self):
        """Key present in expected but not actual -> mismatch."""
        exp = {"content-type": "text/html", "date": "Tue, 09 Jun 2026 03:20:35 GMT"}
        act = {"content-type": "text/html"}
        mismatches = normalize_header_values(exp, act)
        assert "date" in mismatches


# ======================================================================
# Tests: TEMP_DIR_RE and normalize_paths
# ======================================================================

class TestNormalizePaths:
    """Test temp directory path substitution."""

    def test_golden_dir_replaced(self):
        result = normalize_paths("/tmp/thttpd_golden_qz73fauz/index.html")
        assert result == "/tmp/thttpd_NORMALIZED/index.html"

    def test_diff_dir_replaced(self):
        result = normalize_paths("/tmp/thttpd_diff_abc12345/file.txt")
        assert result == "/tmp/thttpd_NORMALIZED/file.txt"

    def test_pytest_dir_replaced(self):
        result = normalize_paths("/tmp/pytest-abc123/test.txt")
        assert result == "/tmp/pytest_NORMALIZED/test.txt"

    def test_no_match(self):
        result = normalize_paths("/var/www/index.html")
        assert result == "/var/www/index.html"

    def test_multiple_paths(self):
        result = normalize_paths(
            "/tmp/thttpd_golden_a/www /tmp/thttpd_diff_b/www"
        )
        assert result == "/tmp/thttpd_NORMALIZED/www /tmp/thttpd_NORMALIZED/www"

    def test_dict_values(self):
        result = normalize_paths({
            "path": "/tmp/thttpd_golden_abc/file.txt",
            "other": "hello",
        })
        assert result == {
            "path": "/tmp/thttpd_NORMALIZED/file.txt",
            "other": "hello",
        }

    def test_list_values(self):
        result = normalize_paths([
            "/tmp/thttpd_diff_123/path",
            "no_match",
        ])
        assert result == [
            "/tmp/thttpd_NORMALIZED/path",
            "no_match",
        ]

    def test_non_string_non_container(self):
        assert normalize_paths(42) == 42
        assert normalize_paths(None) is None


# ======================================================================
# Tests: normalize_cgi_output
# ======================================================================

class TestNormalizeCgiOutput:
    """Test CGI output port normalization."""

    def test_server_port_replaced(self):
        result = normalize_cgi_output("SERVER_PORT=49543\n")
        assert result == "SERVER_PORT=PORT\n"

    def test_http_host_replaced(self):
        result = normalize_cgi_output("HTTP_HOST=127.0.0.1:49543\n")
        assert result == "HTTP_HOST=127.0.0.1:PORT\n"

    def test_host_header_replaced(self):
        result = normalize_cgi_output("Host: 127.0.0.1:49543\n")
        assert result == "Host: 127.0.0.1:PORT\n"

    def test_host_env_replaced(self):
        result = normalize_cgi_output("Host=127.0.0.1:49543\n")
        assert result == "Host=127.0.0.1:PORT\n"

    def test_no_port(self):
        result = normalize_cgi_output("SERVER_PORT=\n")
        # \d+ requires at least one digit, so this shouldn't match
        assert result == "SERVER_PORT=\n"

    def test_non_string(self):
        assert normalize_cgi_output(42) == 42
        assert normalize_cgi_output(None) is None

    def test_multiple_replacements(self):
        result = normalize_cgi_output(
            "SERVER_PORT=49543\nHTTP_HOST=127.0.0.1:49543\n"
        )
        assert "SERVER_PORT=PORT" in result
        assert "HTTP_HOST=127.0.0.1:PORT" in result
        assert "49543" not in result

    def test_unchanged_content(self):
        result = normalize_cgi_output("CONTENT_TYPE=text/html\n")
        assert result == "CONTENT_TYPE=text/html\n"


# ======================================================================
# Tests: check_directory_listing_structure
# ======================================================================

class TestDirectoryListingStructure:
    """Test directory listing structure verification."""

    def test_valid_listing(self):
        body = (
            b"<html><body><PRE>\n"
            b"drwxr-xr-x  2 root root 4096 Jun  9 03:20 .\n"
            b"drwxr-xr-x  3 root root 4096 Jun  9 03:20 ..\n"
            b"-rw-r--r--  1 root root   15 Jun  9 03:20 test.txt\n"
            b"</PRE></body></html>"
        )
        passed, msg = check_directory_listing_structure(body, ["test.txt"])
        assert passed, msg

    def test_missing_entry(self):
        body = (
            b"<html><body><PRE>\n"
            b"drwxr-xr-x  2 root root 4096 Jun  9 03:20 .\n"
            b"-rw-r--r--  1 root root   15 Jun  9 03:20 test.txt\n"
            b"</PRE></body></html>"
        )
        passed, msg = check_directory_listing_structure(
            body, ["test.txt", "missing_file.txt"]
        )
        assert not passed
        assert "missing_file.txt" in msg

    def test_no_body(self):
        passed, msg = check_directory_listing_structure(None, [])
        assert not passed
        assert "No body content" in msg

    def test_no_pre_tag(self):
        body = b"<html><body>test.txt</body></html>"
        passed, msg = check_directory_listing_structure(body, ["test.txt"])
        assert not passed
        assert "<PRE>" in msg

    def test_empty_expected_entries(self):
        body = b"<html><body><PRE>\ncontents\n</PRE></body></html>"
        passed, msg = check_directory_listing_structure(body, [])
        assert passed, msg


# ======================================================================
# Tests: normalize_body
# ======================================================================

class TestNormalizeBody:
    """Test combined body normalizer."""

    def test_path_and_port_normalized(self):
        input_str = (
            "PATH_TRANSLATED=/tmp/thttpd_golden_abc/cgi-bin/test\n"
            "SERVER_PORT=49543\n"
            "HTTP_HOST=127.0.0.1:49543\n"
        )
        result = normalize_body(input_str)
        assert "/tmp/thttpd_NORMALIZED/cgi-bin/test" in result
        assert "SERVER_PORT=PORT" in result
        assert "HTTP_HOST=127.0.0.1:PORT" in result

    def test_non_string(self):
        assert normalize_body(42) == 42

    def test_pwd_normalized(self):
        input_str = "PWD=/tmp/pytest-10/thttpd_shared0/www/cgi-bin\nOTHER=foo\n"
        result = normalize_body(input_str)
        assert "PWD=NORMALIZED" in result
        assert "OTHER=foo" in result

    def test_pwd_different_dirs(self):
        """Both PWD=/some/cgi-bin and PWD=/home/user should normalize the same."""
        r1 = normalize_body("PWD=/tmp/pytest-10/www/cgi-bin\n")
        r2 = normalize_body("PWD=/home/burket/Git/thttpd-rs\n")
        assert r1 == r2
        assert "PWD=NORMALIZED" in r1


# ======================================================================
# Tests: field_result helper
# ======================================================================

class TestFieldResult:
    """Test the field_result helper function."""

    def test_match(self):
        r = field_result("status_code", 200, 200)
        assert r["field"] == "status_code"
        assert r["match"] is True
        assert r["expected"] == 200
        assert r["actual"] == 200

    def test_mismatch(self):
        r = field_result("status_code", 200, 404)
        assert r["match"] is False

    def test_explicit_match(self):
        r = field_result("sha", "a", "b", match=True)
        assert r["match"] is True


# ======================================================================
# Tests: compare_responses_v2
# ======================================================================

class TestCompareResponsesV2:
    """Test the normalized comparison function."""

    def test_compare_v2_exact_match(self):
        """Identical responses -> all match."""
        headers = {
            "date": "Tue, 09 Jun 2026 03:20:35 GMT",
            "content-type": "text/html",
            "content-length": "15",
        }
        body = b"Hello, World!\n"
        exp = make_response_dict(headers=dict(headers), body=body)
        act = make_response_dict(headers=dict(headers), body=body)
        results = compare_responses_v2(exp, act)
        for r in results:
            assert r["match"], f"Field {r['field']} did not match: {r}"

    def test_compare_v2_timestamp_diff(self):
        """Different timestamps -> still match."""
        exp_headers = {
            "date": "Tue, 09 Jun 2026 03:20:35 GMT",
            "content-type": "text/html",
        }
        act_headers = {
            "date": "Fri, 09 Jun 2026 15:05:07 GMT",
            "content-type": "text/html",
        }
        body = b"Hello, World!\n"
        exp = make_response_dict(headers=exp_headers, body=body)
        act = make_response_dict(headers=act_headers, body=body)
        results = compare_responses_v2(exp, act)
        for r in results:
            assert r["match"], f"Field {r['field']} did not match: {r}"

    def test_compare_v2_port_diff(self):
        """Different ports in CGI body -> still match."""
        exp_body = (
            "REQUEST_METHOD=GET\n"
            "SERVER_PORT=49543\n"
            "HTTP_HOST=127.0.0.1:49543\n"
        ).encode("latin-1")
        act_body = (
            "REQUEST_METHOD=GET\n"
            "SERVER_PORT=36777\n"
            "HTTP_HOST=127.0.0.1:36777\n"
        ).encode("latin-1")
        exp_headers = {"content-type": "text/plain"}
        act_headers = {"content-type": "text/plain"}
        exp = make_response_dict(headers=exp_headers, body=exp_body)
        act = make_response_dict(headers=act_headers, body=act_body)
        results = compare_responses_v2(exp, act)
        for r in results:
            assert r["match"], f"Field {r['field']} did not match: {r}"

    def test_compare_v2_status_code_mismatch(self):
        """Different status codes -> fail."""
        exp = make_response_dict(status_code=200)
        act = make_response_dict(status_code=404)
        results = compare_responses_v2(exp, act)
        status_results = [r for r in results if r["field"] == "status_code"]
        assert len(status_results) == 1
        assert not status_results[0]["match"]

    def test_compare_v2_strict_mode(self):
        """Strict mode falls back to exact comparison."""
        exp_headers = {
            "date": "Tue, 09 Jun 2026 03:20:35 GMT",
        }
        act_headers = {
            "date": "Fri, 09 Jun 2026 15:05:07 GMT",
        }
        exp = make_response_dict(headers=exp_headers)
        act = make_response_dict(headers=act_headers)
        results = compare_responses_v2(exp, act, strict=True)
        date_results = [r for r in results if r["field"] == "header_values"]
        assert len(date_results) == 1
        assert not date_results[0]["match"]

    def test_compare_v2_body_without_raw_body(self):
        """Response without raw body field (baseline-style) compares length directly."""
        headers = {"content-type": "text/html"}
        exp = {
            "status_code": 200,
            "status_text": "OK",
            "headers": dict(headers),
            "body_sha256": "a" * 64,
            "body_length": 100,
            "connection_result": "ok",
        }
        act = {
            "status_code": 200,
            "status_text": "OK",
            "headers": dict(headers),
            "body_sha256": "b" * 64,
            "body_length": 100,
            "connection_result": "ok",
        }
        results = compare_responses_v2(exp, act)
        for r in results:
            assert r["match"], f"Field {r['field']} did not match: {r}"

    def test_compare_v2_header_path_normalized(self):
        """Path normalization in headers -> match."""
        exp_headers = {
            "content-location": "/tmp/thttpd_golden_abc/file.txt",
        }
        act_headers = {
            "content-location": "/tmp/thttpd_diff_xyz/file.txt",
        }
        exp = make_response_dict(headers=exp_headers)
        act = make_response_dict(headers=act_headers)
        results = compare_responses_v2(exp, act)
        hdr_results = [r for r in results if r["field"] == "header_values"]
        assert len(hdr_results) == 1
        assert hdr_results[0]["match"], "Headers with paths should match"

    def test_compare_v2_all_fields_present(self):
        """Verify all 8 fields are present in results."""
        exp = make_response_dict()
        act = make_response_dict()
        results = compare_responses_v2(exp, act)
        fields = {r["field"] for r in results}
        expected_fields = {
            "status_code", "status_text", "header_count", "header_order",
            "header_values", "body_sha256", "body_length", "connection_result",
        }
        assert expected_fields.issubset(fields), f"Missing fields: {expected_fields - fields}"


class TestCompareResponsesOriginal:
    """Verify original compare_responses still works."""

    def test_original_backward_compatible(self):
        exp = make_response_dict(status_code=200)
        act = make_response_dict(status_code=200)
        results = compare_responses(exp, act)
        assert all(r["match"] for r in results)

    def test_original_detects_mismatch(self):
        exp = make_response_dict(status_code=200)
        act = make_response_dict(status_code=404)
        results = compare_responses(exp, act)
        sc = [r for r in results if r["field"] == "status_code"][0]
        assert not sc["match"]
