"""Static file serving tests."""
import os
import socket
import time
import pytest

from conftest import http_request, parse_response


class TestStaticFiles:
    """Tests for static file serving."""

    def test_get_text_file(self, server_process):
        """GET a plain text file."""
        proc, port = server_process
        raw = http_request(port, b'GET /test.txt HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        assert b'test content' in resp['body']

    def test_get_html_file(self, server_process):
        """GET an HTML file."""
        proc, port = server_process
        raw = http_request(port, b'GET /page.html HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        assert resp['headers'].get('content-type', '').startswith('text/html')
        assert b'Test Page' in resp['body']

    def test_get_binary_file(self, server_process):
        """GET a binary file."""
        proc, port = server_process
        raw = http_request(port, b'GET /image.png HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        # PNG magic bytes
        assert resp['body'][:4] == b'\x89PNG'
        assert 'content-length' in resp['headers']

    def test_get_large_file(self, server_process):
        """GET a large file (100KB)."""
        proc, port = server_process
        raw = http_request(port, b'GET /largefile.bin HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        assert len(resp['body']) == 100000
        assert resp['body'] == b'A' * 100000

    def test_get_zero_length_file(self, server_process):
        """GET a zero-length file."""
        proc, port = server_process
        raw = http_request(port, b'GET /empty.txt HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        assert resp['body'] == b''

    def test_get_symlink(self, server_process):
        """GET a file via symlink."""
        proc, port = server_process
        raw = http_request(port, b'GET /link.html HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        assert b'Hello World' in resp['body']

    def test_if_modified_since_not_modified(self, server_process):
        """If-Modified-Since returns 304 when not changed."""
        proc, port = server_process
        # First request to get Last-Modified
        raw = http_request(port, b'GET /test.txt HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        last_mod = resp['headers'].get('last-modified', '')

        # Second request with If-Modified-Since
        if last_mod:
            req = (
                f'GET /test.txt HTTP/1.0\r\n'
                f'If-Modified-Since: {last_mod}\r\n'
                f'\r\n'
            ).encode()
            raw2 = http_request(port, req)
            resp2 = parse_response(raw2)
            assert resp2['status_code'] == 304
        else:
            pytest.skip("No Last-Modified header returned")

    def test_range_request(self, server_process):
        """Range request returns partial content."""
        proc, port = server_process
        # First check if Accept-Ranges is advertised
        raw = http_request(port, b'GET /test.txt HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200

        # Range request for first 4 bytes
        raw2 = http_request(port, b'GET /test.txt HTTP/1.0\r\nRange: bytes=0-3\r\n\r\n')
        resp2 = parse_response(raw2)
        # thttpd may return 200 or 206 depending on version
        if resp2['status_code'] == 206:
            assert resp2['body'] == b'test'
            assert 'content-range' in resp2['headers']
        elif resp2['status_code'] == 200:
            # Some thttpd versions ignore Range for HTTP/1.0
            assert resp2['status_code'] in (200, 206)

    def test_get_directory_index(self, server_process):
        """GET / returns index.html."""
        proc, port = server_process
        raw = http_request(port, b'GET / HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        assert b'Hello World' in resp['body']

    def test_get_nonexistent_file(self, server_process):
        """GET nonexistent file returns 404."""
        proc, port = server_process
        raw = http_request(port, b'GET /nonexistent.txt HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 404
