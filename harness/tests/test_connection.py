"""Connection handling tests."""
import os
import socket
import time
import threading
import pytest

from conftest import http_request, parse_response


class TestConnection:
    """Tests for connection handling."""

    def test_tcp_connection(self, server_process):
        """Basic TCP connection and response."""
        proc, port = server_process
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.settimeout(5)
        s.connect(('127.0.0.1', port))
        s.sendall(b'GET / HTTP/1.0\r\n\r\n')
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
        assert len(data) > 0
        resp = parse_response(data)
        assert resp['status_code'] == 200

    def test_connection_timeout(self, server_process):
        """Connection that sends nothing eventually gets cleaned up."""
        proc, port = server_process
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.settimeout(30)  # long timeout
        s.connect(('127.0.0.1', port))
        # Don't send anything, just wait a bit
        s.settimeout(5)
        data = b''
        try:
            while True:
                chunk = s.recv(4096)
                if not chunk:
                    break
                data += chunk
        except (socket.timeout, OSError):
            pass
        s.close()
        # Server may close the connection or we just time out
        # Either way the server should still work
        raw = http_request(port, b'GET / HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200

    def test_multiple_connections(self, server_process):
        """Multiple sequential connections all succeed."""
        proc, port = server_process
        for _ in range(10):
            raw = http_request(port, b'GET /test.txt HTTP/1.0\r\n\r\n')
            resp = parse_response(raw)
            assert resp['status_code'] == 200

    def test_connection_reset(self, server_process):
        """Connection reset during request doesn't crash server."""
        proc, port = server_process
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.settimeout(5)
        s.connect(('127.0.0.1', port))
        s.sendall(b'GET / HT')
        # Reset the connection abruptly
        s.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, b'\x01\x00\x00\x00\x00\x00\x00\x00')
        s.close()
        # Server should still work
        time.sleep(0.2)
        raw = http_request(port, b'GET / HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200

    def test_slow_loris(self, server_process):
        """Slow loris - send request very slowly."""
        proc, port = server_process
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.settimeout(10)
        s.connect(('127.0.0.1', port))
        # Send request byte by byte with small delays
        req = b'GET / HTTP/1.0\r\n\r\n'
        for i in range(len(req)):
            s.send(req[i:i+1])
            time.sleep(0.01)
        data = b''
        s.settimeout(5)
        while True:
            try:
                chunk = s.recv(4096)
                if not chunk:
                    break
                data += chunk
            except (socket.timeout, OSError):
                break
        s.close()
        # Should eventually get a response
        resp = parse_response(data)
        assert resp['status_code'] == 200

    def test_partial_read(self, server_process):
        """Partial read - read only part of the response then close."""
        proc, port = server_process
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.settimeout(5)
        s.connect(('127.0.0.1', port))
        s.sendall(b'GET /largefile.bin HTTP/1.0\r\n\r\n')
        # Read only a small portion
        data = s.recv(100)
        s.close()
        # Server should still work
        time.sleep(0.2)
        raw = http_request(port, b'GET / HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200

    def test_large_response(self, server_process):
        """Large response (100KB) received completely."""
        proc, port = server_process
        raw = http_request(port, b'GET /largefile.bin HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        assert len(resp['body']) == 100000

    def test_connection_close_after_response(self, server_process):
        """Connection closed after HTTP/1.0 response."""
        proc, port = server_process
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.settimeout(5)
        s.connect(('127.0.0.1', port))
        s.sendall(b'GET / HTTP/1.0\r\n\r\n')
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
        resp = parse_response(data)
        assert resp['status_code'] == 200
        # After response, the connection should be closed (HTTP/1.0 default)
        assert resp['headers'].get('connection', '').lower() == 'close'

    def test_idle_connection_cleanup(self, server_process):
        """Idle connections get cleaned up without affecting new connections."""
        proc, port = server_process
        # Open a connection and leave it idle
        idle_sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        idle_sock.settimeout(30)
        idle_sock.connect(('127.0.0.1', port))
        # Don't send anything

        # New connection should still work
        raw = http_request(port, b'GET / HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        idle_sock.close()

    def test_max_connections(self, server_process):
        """Many connections don't crash the server."""
        proc, port = server_process
        socks = []
        # Open a bunch of connections
        for _ in range(20):
            try:
                s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
                s.settimeout(5)
                s.connect(('127.0.0.1', port))
                socks.append(s)
            except (ConnectionRefusedError, OSError):
                break

        # Try a normal request while many connections are open
        try:
            raw = http_request(port, b'GET / HTTP/1.0\r\n\r\n', timeout=10)
            resp = parse_response(raw)
            assert resp['status_code'] == 200
        finally:
            for s in socks:
                try:
                    s.close()
                except OSError:
                    pass
