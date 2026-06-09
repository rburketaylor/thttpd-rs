"""Error response tests."""
import os
import stat
import socket
import time
import pytest

from conftest import http_request, parse_response


class TestErrors:
    """Tests for error responses."""

    def test_404_not_found(self, server_process):
        """404 for nonexistent file."""
        proc, port = server_process
        raw = http_request(port, b'GET /nonexistent.html HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 404

    def test_403_forbidden(self, server_process, www_root):
        """403 for file with no read permission."""
        proc, port = server_process
        # Create a file with no read permission for others
        no_read = www_root / "noperm.txt"
        no_read.write_text("secret")
        # Remove all permissions
        no_read.chmod(0o000)

        raw = http_request(port, b'GET /noperm.txt HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] in (403, 500)

        # Cleanup
        no_read.chmod(0o644)

    def test_400_bad_request(self, server_process):
        """400 for malformed request."""
        proc, port = server_process
        # Send an invalid HTTP request
        raw = http_request(port, b'BADREQUEST\r\n\r\n')
        resp = parse_response(raw)
        # Should get 400 or the connection may just close
        assert resp['status_code'] in (400, 0) or raw == b''

    def test_501_not_implemented(self, server_process):
        """501 for unsupported method."""
        proc, port = server_process
        raw = http_request(port, b'DELETE /test.txt HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] in (501, 400, 405)

    def test_error_page_html(self, server_process):
        """Error page contains HTML content."""
        proc, port = server_process
        raw = http_request(port, b'GET /nonexistent.html HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 404
        # Error pages should be HTML
        assert b'<html' in resp['body'].lower() or b'not found' in resp['body'].lower()

    def test_error_content_type(self, server_process):
        """Error response Content-Type is text/html."""
        proc, port = server_process
        raw = http_request(port, b'GET /nonexistent.html HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 404
        ct = resp['headers'].get('content-type', '')
        assert 'text/html' in ct

    def test_directory_without_index(self, server_process):
        """Directory without index returns 404 or generates listing."""
        proc, port = server_process
        raw = http_request(port, b'GET /subdir/ HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        # thttpd may auto-generate directory listing or return 404
        # depending on configuration (default is no listing)
        assert resp['status_code'] in (200, 404, 403)

    def test_permission_denied(self, server_process, www_root):
        """File with no read permission returns 403."""
        proc, port = server_process
        # Create a file with no read permission
        no_read = www_root / "noperm.txt"
        no_read.write_text("secret")
        no_read.chmod(0o000)

        raw = http_request(port, b'GET /noperm.txt HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] in (403, 500)

        # Cleanup
        no_read.chmod(0o644)

    def test_symlink_outside_root(self, server_process, www_root):
        """Symlink pointing outside root returns 403."""
        proc, port = server_process
        # Create a symlink to /etc/passwd
        outside_link = www_root / "outside_link"
        try:
            outside_link.symlink_to("/etc/passwd")
        except OSError:
            pytest.skip("Cannot create symlink")

        raw = http_request(port, b'GET /outside_link HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] in (403, 404)
        assert b'root:' not in resp['body']

        # Cleanup
        outside_link.unlink()

    def test_cgi_not_found(self, server_process):
        """CGI script not found returns 404."""
        proc, port = server_process
        raw = http_request(port, b'GET /cgi-bin/nonexistent.sh HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] in (404, 500)
