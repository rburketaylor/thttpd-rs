"""CGI execution tests."""
import os
import socket
import time
import pytest

from conftest import http_request, parse_response


class TestCgi:
    """Tests for CGI execution."""

    def test_simple_cgi(self, server_process):
        """Execute a simple CGI script."""
        proc, port = server_process
        raw = http_request(port, b'GET /cgi-bin/hello.sh HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        assert b'hello from cgi' in resp['body']

    def test_cgi_with_query_string(self, server_process):
        """CGI script receives QUERY_STRING."""
        proc, port = server_process
        raw = http_request(port, b'GET /cgi-bin/query.sh?foo=bar&baz=qux HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        assert b'QUERY_STRING=foo=bar&baz=qux' in resp['body']

    def test_cgi_with_post(self, server_process):
        """POST data passed to CGI via stdin."""
        proc, port = server_process
        body = b'this is post data'
        req = (
            b'POST /cgi-bin/post.sh HTTP/1.0\r\n'
            b'Content-Length: ' + str(len(body)).encode() + b'\r\n'
            b'\r\n' + body
        )
        raw = http_request(port, req)
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        assert b'this is post data' in resp['body']

    def test_nph_cgi(self, server_process):
        """NPH CGI script sends raw HTTP response."""
        proc, port = server_process
        raw = http_request(port, b'GET /cgi-bin/nph-test.sh HTTP/1.0\r\n\r\n')
        # NPH scripts send the full HTTP response including status line
        assert b'HTTP/1.0 200 OK' in raw
        assert b'nph response' in raw

    def test_cgi_environment_variables(self, server_process):
        """CGI receives correct environment variables."""
        proc, port = server_process
        raw = http_request(port, b'GET /cgi-bin/env.sh HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        body_str = resp['body'].decode('latin-1')
        # Check essential CGI variables are present
        assert 'REQUEST_METHOD=GET' in body_str
        assert 'SERVER_PROTOCOL=HTTP/1.0' in body_str or 'SERVER_PROTOCOL=http/1.0' in body_str.lower()
        assert 'GATEWAY_INTERFACE' in body_str

    def test_cgi_pattern_matching(self, server_process):
        """CGI pattern matching works correctly - scripts in cgi-bin match the **cgi-bin** pattern."""
        proc, port = server_process
        raw = http_request(port, b'GET /cgi-bin/hello.sh HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        assert b'hello from cgi' in resp['body']

    def test_cgi_error(self, server_process):
        """CGI script that exits with error."""
        proc, port = server_process
        raw = http_request(port, b'GET /cgi-bin/error.sh HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        # CGI scripts that exit with error may still send their output
        # thttpd returns whatever the CGI produced before exit
        assert resp['status_code'] in (200, 500, 502)
        # The output should still contain the CGI's response
        assert b'error output' in resp['body'] or resp['status_code'] in (500, 502)

    def test_post_post_garbage_hack(self, server_process):
        """POST body followed by trailing CR/LF is consumed without error."""
        proc, port = server_process
        body = b'data'
        # Extra CRLF after the body - thttpd should handle this gracefully
        req = (
            b'POST /cgi-bin/post.sh HTTP/1.0\r\n'
            b'Content-Length: 4\r\n'
            b'\r\n' + body + b'\r\n'
        )
        raw = http_request(port, req)
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        assert b'data' in resp['body']

    def test_cgi_content_length(self, server_process):
        """CGI receives correct CONTENT_LENGTH."""
        proc, port = server_process
        body = b'1234567890'
        req = (
            b'POST /cgi-bin/env.sh HTTP/1.0\r\n'
            b'Content-Length: ' + str(len(body)).encode() + b'\r\n'
            b'\r\n' + body
        )
        raw = http_request(port, req)
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        body_str = resp['body'].decode('latin-1')
        assert 'CONTENT_LENGTH=10' in body_str

    def test_cgi_path_info(self, server_process):
        """CGI receives correct PATH_INFO."""
        proc, port = server_process
        raw = http_request(port, b'GET /cgi-bin/pathinfo.sh/extra/path/info HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        body_str = resp['body'].decode('latin-1')
        assert 'PATH_INFO=/extra/path/info' in body_str
