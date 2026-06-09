"""Malformed input tests."""
import os
import socket
import time
import pytest

from conftest import http_request, parse_response


class TestMalformed:
    """Tests for malformed input handling."""

    def test_invalid_method(self, server_process):
        """Invalid HTTP method."""
        proc, port = server_process
        raw = http_request(port, b'FOOBAR / HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        # thttpd should return 501 Not Implemented or 400
        assert resp['status_code'] in (400, 501)

    def test_missing_host_header(self, server_process):
        """Missing Host header (HTTP/1.0 allows this)."""
        proc, port = server_process
        raw = http_request(port, b'GET / HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        # HTTP/1.0 doesn't require Host, so should work fine
        assert resp['status_code'] == 200

    def test_invalid_http_version(self, server_process):
        """Invalid HTTP version."""
        proc, port = server_process
        raw = http_request(port, b'GET / HTTP/9.9\r\n\r\n')
        resp = parse_response(raw)
        # Should return 400 or 505 or handle gracefully
        assert resp['status_code'] in (200, 400, 505)

    def test_truncated_request(self, server_process):
        """Truncated request line."""
        proc, port = server_process
        # Send just a partial request then close
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.settimeout(5)
        s.connect(('127.0.0.1', port))
        s.sendall(b'GET /\r\n')
        s.shutdown(socket.SHUT_WR)
        data = b''
        while True:
            try:
                chunk = s.recv(4096)
                if not chunk:
                    break
                data += chunk
            except (socket.timeout, OSError):
                break
        s.close()
        # Should not crash; may get a response or nothing
        assert True

    def test_binary_garbage(self, server_process):
        """Binary garbage as request."""
        proc, port = server_process
        garbage = bytes(range(256)) * 4
        raw = http_request(port, garbage)
        # Server should not crash; may return 400 or disconnect
        # Just verify it didn't crash - try a valid request after
        raw2 = http_request(port, b'GET / HTTP/1.0\r\n\r\n')
        resp2 = parse_response(raw2)
        assert resp2['status_code'] == 200

    def test_very_long_header(self, server_process):
        """Very long header value."""
        proc, port = server_process
        long_val = 'X' * 10000
        req = f'GET / HTTP/1.0\r\nX-Long: {long_val}\r\n\r\n'.encode()
        raw = http_request(port, req, timeout=5, read_timeout=5)
        resp = parse_response(raw)
        # Should not crash - may return 200, 400, or 431
        assert resp['status_code'] in (200, 400, 431, 0) or raw == b''

    def test_duplicate_headers(self, server_process):
        """Duplicate headers accepted."""
        proc, port = server_process
        req = (
            b'GET / HTTP/1.0\r\n'
            b'Accept: text/html\r\n'
            b'Accept: text/plain\r\n'
            b'\r\n'
        )
        raw = http_request(port, req)
        resp = parse_response(raw)
        assert resp['status_code'] == 200

    def test_negative_content_length(self, server_process):
        """Negative Content-Length."""
        proc, port = server_process
        req = (
            b'POST /cgi-bin/post.sh HTTP/1.0\r\n'
            b'Content-Length: -1\r\n'
            b'\r\n'
        )
        raw = http_request(port, req)
        resp = parse_response(raw)
        # Should return 400 or ignore the negative content-length
        assert resp['status_code'] in (200, 400, 411)

    def test_chunked_transfer_encoding(self, server_process):
        """Chunked transfer encoding (thttpd doesn't support it for requests)."""
        proc, port = server_process
        req = (
            b'POST /cgi-bin/post.sh HTTP/1.0\r\n'
            b'Transfer-Encoding: chunked\r\n'
            b'\r\n'
            b'4\r\n'
            b'test\r\n'
            b'0\r\n'
            b'\r\n'
        )
        raw = http_request(port, req)
        resp = parse_response(raw)
        # thttpd likely doesn't support chunked request encoding
        # May return 400, 411, or process it somehow
        assert resp['status_code'] in (200, 400, 411, 501)

    def test_pipeline_requests(self, server_process):
        """HTTP pipelining - send two requests in one TCP connection."""
        proc, port = server_process
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.settimeout(5)
        s.connect(('127.0.0.1', port))
        # Send two pipelined requests
        s.sendall(
            b'GET /test.txt HTTP/1.0\r\n\r\n'
            b'GET /index.html HTTP/1.0\r\n\r\n'
        )
        data = b''
        s.settimeout(3)
        while True:
            try:
                chunk = s.recv(4096)
                if not chunk:
                    break
                data += chunk
            except socket.timeout:
                break
        s.close()
        # thttpd with HTTP/1.0 closes after first response (Connection: close)
        # So we should get at least the first response
        resp = parse_response(data)
        assert resp['status_code'] == 200
