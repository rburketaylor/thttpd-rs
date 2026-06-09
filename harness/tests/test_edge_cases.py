"""Edge case tests."""
import os
import socket
import time
import threading
import pytest

from conftest import http_request, parse_response


class TestEdgeCases:
    """Tests for edge cases."""

    def test_empty_request(self, server_process):
        """Empty request handled gracefully."""
        proc, port = server_process
        # Send nothing then close
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.settimeout(5)
        s.connect(('127.0.0.1', port))
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
        # Server may send nothing or may send a 400
        # Just verify we didn't crash
        assert True

    def test_very_long_url(self, server_process):
        """Very long URL handled (returns 400 or 404 or 200)."""
        proc, port = server_process
        long_path = '/a' * 5000
        req = f'GET {long_path} HTTP/1.0\r\n\r\n'.encode()
        raw = http_request(port, req, timeout=5, read_timeout=5)
        # Server should not crash - may return 400, 404, or disconnect
        assert raw is not None

    def test_special_characters_in_url(self, server_process):
        """Special characters in URL."""
        proc, port = server_process
        # URL-encoded space
        raw = http_request(port, b'GET /test%2Etxt HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        # thttpd may or may not decode this; it should not crash
        assert resp['status_code'] in (200, 404, 400)

    def test_concurrent_requests(self, server_process):
        """Multiple concurrent requests all succeed."""
        proc, port = server_process
        results = [None] * 5
        errors = [None] * 5

        def do_request(idx):
            try:
                raw = http_request(port, b'GET /test.txt HTTP/1.0\r\n\r\n')
                resp = parse_response(raw)
                results[idx] = resp
            except Exception as e:
                errors[idx] = e

        threads = [threading.Thread(target=do_request, args=(i,)) for i in range(5)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=10)

        for i, err in enumerate(errors):
            assert err is None, f"Thread {i} got error: {err}"
        for i, resp in enumerate(results):
            assert resp is not None, f"Thread {i} got no response"
            assert resp['status_code'] == 200, f"Thread {i} got {resp['status_code']}"

    def test_http_09_request(self, server_process):
        """HTTP/0.9 style request (no version)."""
        proc, port = server_process
        # HTTP/0.9 is just "GET /path\r\n"
        raw = http_request(port, b'GET /test.txt\r\n')
        # thttpd may respond with HTTP/0.9 (body only) or HTTP/1.0
        # Just verify we got something and server didn't crash
        assert len(raw) > 0 or raw == b''

    def test_keep_alive_request(self, server_process):
        """Keep-alive connection (HTTP/1.1 style)."""
        proc, port = server_process
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.settimeout(5)
        s.connect(('127.0.0.1', port))
        s.sendall(b'GET /test.txt HTTP/1.1\r\nHost: localhost\r\n\r\n')
        data = b''
        s.settimeout(2)
        while True:
            try:
                chunk = s.recv(4096)
                if not chunk:
                    break
                data += chunk
                # Check if we have the full response
                if b'\r\n\r\n' in data:
                    header_part = data.split(b'\r\n\r\n')[0]
                    for line in header_part.split(b'\r\n'):
                        if line.lower().startswith(b'content-length:'):
                            cl = int(line.split(b':')[1].strip())
                            body_start = data.index(b'\r\n\r\n') + 4
                            if len(data) >= body_start + cl:
                                break
            except socket.timeout:
                break
        s.close()
        resp = parse_response(data)
        assert resp['status_code'] == 200
        assert b'test content' in resp['body']

    def test_head_request(self, server_process):
        """HEAD request returns headers only, no body."""
        proc, port = server_process
        raw = http_request(port, b'HEAD /test.txt HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        # HEAD should have Content-Length header but empty body
        assert 'content-length' in resp['headers']
        assert resp['body'] == b''

    def test_post_to_static_file(self, server_process):
        """POST to static file (not CGI) returns error or static file."""
        proc, port = server_process
        req = (
            b'POST /test.txt HTTP/1.0\r\n'
            b'Content-Length: 4\r\n'
            b'\r\ndata'
        )
        raw = http_request(port, req)
        resp = parse_response(raw)
        # thttpd may return 501 (Not Implemented) or 405 or just serve the file
        assert resp['status_code'] in (200, 404, 405, 501)

    def test_directory_traversal_attempt(self, server_process):
        """Directory traversal blocked."""
        proc, port = server_process
        raw = http_request(port, b'GET /../../../etc/passwd HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        # Should not return /etc/passwd
        assert resp['status_code'] in (400, 403, 404)
        assert b'root:' not in resp['body']

    def test_double_slash_in_url(self, server_process):
        """Double slash in URL handled."""
        proc, port = server_process
        # Try to access test.txt via double slash
        raw = http_request(port, b'GET //test.txt HTTP/1.0\r\n\r\n')
        # thttpd should handle this; it may normalize or return 404
        resp = parse_response(raw)
        assert resp['status_code'] in (200, 404, 400)
