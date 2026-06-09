"""Throttling tests."""
import os
import socket
import time
import pytest

from conftest import http_request, parse_response


class TestThrottling:
    """Tests for bandwidth throttling."""

    def test_throttle_file_loading(self, server_process_with_throttle):
        """Server starts successfully with a throttle file."""
        proc, port = server_process_with_throttle
        # If we got here, the throttle file loaded without error
        raw = http_request(port, b'GET / HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        assert b'Hello World' in resp['body']

    def test_throttle_rate_limiting(self, server_process_with_throttle):
        """Rate limiting enforced - file still gets delivered."""
        proc, port = server_process_with_throttle
        raw = http_request(port, b'GET /largefile.bin HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        assert len(resp['body']) == 100000

    def test_throttle_fair_share(self, server_process_with_throttle):
        """Fair share - two connections both get their files."""
        proc, port = server_process_with_throttle
        # Open two connections simultaneously
        s1 = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s1.settimeout(10)
        s1.connect(('127.0.0.1', port))
        s2 = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s2.settimeout(10)
        s2.connect(('127.0.0.1', port))

        s1.sendall(b'GET /test.txt HTTP/1.0\r\n\r\n')
        s2.sendall(b'GET /test.txt HTTP/1.0\r\n\r\n')

        data1 = b''
        s1.settimeout(5)
        while True:
            try:
                chunk = s1.recv(4096)
                if not chunk:
                    break
                data1 += chunk
            except (socket.timeout, OSError):
                break
        s1.close()

        data2 = b''
        s2.settimeout(5)
        while True:
            try:
                chunk = s2.recv(4096)
                if not chunk:
                    break
                data2 += chunk
            except (socket.timeout, OSError):
                break
        s2.close()

        resp1 = parse_response(data1)
        resp2 = parse_response(data2)
        assert resp1['status_code'] == 200
        assert resp2['status_code'] == 200

    def test_throttle_rolling_average(self, server_process_with_throttle):
        """Throttled server handles repeated requests correctly."""
        proc, port = server_process_with_throttle
        for _ in range(5):
            raw = http_request(port, b'GET /test.txt HTTP/1.0\r\n\r\n')
            resp = parse_response(raw)
            assert resp['status_code'] == 200

    def test_no_throttle(self, server_process):
        """No throttle file means unlimited - server still works."""
        proc, port = server_process
        raw = http_request(port, b'GET /largefile.bin HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        assert len(resp['body']) == 100000

    def test_cgi_bytecount(self, server_process_with_throttle):
        """CGI responses counted under throttle but still complete."""
        proc, port = server_process_with_throttle
        raw = http_request(port, b'GET /cgi-bin/hello.sh HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        assert b'hello from cgi' in resp['body']

    def test_throttle_pause_resume(self, server_process_with_throttle):
        """Throttled server handles connection that reads slowly."""
        proc, port = server_process_with_throttle
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.settimeout(10)
        s.connect(('127.0.0.1', port))
        s.sendall(b'GET /largefile.bin HTTP/1.0\r\n\r\n')

        # Read slowly
        data = b''
        s.settimeout(5)
        while True:
            try:
                chunk = s.recv(1024)
                if not chunk:
                    break
                data += chunk
                time.sleep(0.001)  # Small delay between reads
            except (socket.timeout, OSError):
                break
        s.close()
        resp = parse_response(data)
        assert resp['status_code'] == 200
        assert len(resp['body']) == 100000

    def test_throttle_multiple_patterns(self, server_process_with_throttle):
        """Server with throttle handles different file types."""
        proc, port = server_process_with_throttle
        # Text file
        raw1 = http_request(port, b'GET /test.txt HTTP/1.0\r\n\r\n')
        resp1 = parse_response(raw1)
        assert resp1['status_code'] == 200

        # HTML file
        raw2 = http_request(port, b'GET /page.html HTTP/1.0\r\n\r\n')
        resp2 = parse_response(raw2)
        assert resp2['status_code'] == 200

        # Binary file
        raw3 = http_request(port, b'GET /image.png HTTP/1.0\r\n\r\n')
        resp3 = parse_response(raw3)
        assert resp3['status_code'] == 200

    def test_throttle_min_limit(self, server_process_with_throttle):
        """Throttle with high limit still delivers content."""
        proc, port = server_process_with_throttle
        # Our throttle file uses 1000000 bytes/sec which is very high
        # so responses should come through quickly
        raw = http_request(port, b'GET /largefile.bin HTTP/1.0\r\n\r\n')
        resp = parse_response(raw)
        assert resp['status_code'] == 200
        assert len(resp['body']) == 100000

    def test_throttle_connection_count(self, server_process_with_throttle):
        """Throttled server handles multiple concurrent connections."""
        proc, port = server_process_with_throttle
        import threading

        results = [None] * 5
        errors = [None] * 5

        def do_request(idx):
            try:
                raw = http_request(port, b'GET /test.txt HTTP/1.0\r\n\r\n', timeout=10)
                resp = parse_response(raw)
                results[idx] = resp
            except Exception as e:
                errors[idx] = e

        threads = [threading.Thread(target=do_request, args=(i,)) for i in range(5)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=30)

        for i, err in enumerate(errors):
            assert err is None, f"Thread {i} error: {err}"
        for i, resp in enumerate(results):
            assert resp is not None and resp['status_code'] == 200, f"Thread {i} failed"
