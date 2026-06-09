"""Header handling tests."""
import os
import socket
import time
import pytest

from conftest import http_request, parse_response


class TestHeaders:
    """Tests for HTTP header handling."""

    def test_content_type_header(self, server_process):
        """Correct Content-Type header for HTML."""
        proc, port = server_process
        raw = http_request(port, b'GET /page.html HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        ct = resp['headers'].get('content-type', '')
        assert 'text/html' in ct

    def test_content_length_header(self, server_process):
        """Content-Length header matches body size."""
        proc, port = server_process
        raw = http_request(port, b'GET /test.txt HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        cl = int(resp['headers'].get('content-length', '-1'))
        assert cl == len(resp['body'])

    def test_date_header(self, server_process):
        """Date header is present and valid."""
        proc, port = server_process
        raw = http_request(port, b'GET / HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        assert 'date' in resp['headers']
        # Date should look like RFC 1123 format
        date_val = resp['headers']['date']
        assert 'GMT' in date_val or '20' in date_val

    def test_server_header(self, server_process):
        """Server header is present."""
        proc, port = server_process
        raw = http_request(port, b'GET / HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        assert 'server' in resp['headers']
        # thttpd/sthttpd server header
        server_val = resp['headers']['server']
        assert 'thttpd' in server_val.lower()

    def test_last_modified_header(self, server_process):
        """Last-Modified header is present for files."""
        proc, port = server_process
        raw = http_request(port, b'GET /test.txt HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        assert 'last-modified' in resp['headers']
        lm = resp['headers']['last-modified']
        assert 'GMT' in lm

    def test_connection_close(self, server_process):
        """Connection: close is sent for HTTP/1.0."""
        proc, port = server_process
        raw = http_request(port, b'GET / HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        assert resp['headers'].get('connection', '').lower() == 'close'

    def test_accept_encoding_gzip(self, server_process):
        """Accept-Encoding: gzip in request does not cause errors (thttpd doesn't compress)."""
        proc, port = server_process
        raw = http_request(port, b'GET /test.txt HTTP/1.0\r\nAccept-Encoding: gzip\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        assert b'test content' in resp['body']
        # thttpd does not do content-encoding, response should be uncompressed

    def test_host_header_virtual_hosting(self, server_process):
        """Host header accepted without error."""
        proc, port = server_process
        raw = http_request(port, b'GET / HTTP/1.0\r\nHost: example.com\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        assert b'Hello World' in resp['body']

    def test_custom_headers_forwarded(self, server_process):
        """Unknown request headers accepted without error."""
        proc, port = server_process
        raw = http_request(
            port,
            b'GET /test.txt HTTP/1.0\r\n'
            b'X-Custom-Header: custom-value\r\n'
            b'X-Another-Header: another-value\r\n'
            b'\r\n'
        )
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        assert b'test content' in resp['body']

    def test_charset_header(self, server_process):
        """Charset appended to Content-Type for text files."""
        proc, port = server_process
        raw = http_request(port, b'GET /page.html HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        ct = resp['headers'].get('content-type', '')
        # Default charset should be iso-8859-1 unless overridden with -T
        assert 'charset' in ct.lower()
