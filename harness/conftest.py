"""Pytest fixtures for thttpd golden master testing."""
import os
import socket
import subprocess
import time
import tempfile
import pytest
import signal
import stat


def find_free_port():
    """Find a free TCP port."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(('', 0))
        return s.getsockname()[1]


def http_request(port, request_bytes, timeout=5, read_timeout=5):
    """Send raw bytes to localhost:port and return the full response bytes."""
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.settimeout(timeout)
    s.connect(('127.0.0.1', port))
    s.sendall(request_bytes)
    data = b''
    s.settimeout(read_timeout)
    while True:
        try:
            chunk = s.recv(4096)
            if not chunk:
                break
            data += chunk
        except (socket.timeout, ConnectionResetError, BrokenPipeError, OSError):
            break
    s.close()
    return data


def parse_response(raw):
    """Parse raw HTTP response bytes into a dict with status_code, status_text,
    headers (dict), body (bytes)."""
    # Split headers from body
    if b'\r\n\r\n' in raw:
        header_part, body = raw.split(b'\r\n\r\n', 1)
    elif b'\n\n' in raw:
        header_part, body = raw.split(b'\n\n', 1)
    else:
        return {
            'raw': raw,
            'status_code': 0,
            'status_text': '',
            'headers': {},
            'body': raw,
        }

    lines = header_part.decode('latin-1').split('\r\n')
    if not lines:
        return {
            'raw': raw,
            'status_code': 0,
            'status_text': '',
            'headers': {},
            'body': body,
        }

    # Parse status line
    status_line = lines[0]
    parts = status_line.split(' ', 2)
    status_code = int(parts[1]) if len(parts) >= 2 else 0
    status_text = parts[2] if len(parts) >= 3 else ''

    # Parse headers (handle duplicates by keeping last value)
    headers = {}
    for line in lines[1:]:
        if ':' in line:
            key, val = line.split(':', 1)
            headers[key.strip().lower()] = val.strip()

    return {
        'raw': raw,
        'status_line': status_line,
        'status_code': status_code,
        'status_text': status_text,
        'headers': headers,
        'body': body,
    }


@pytest.fixture
def www_root(tmp_path):
    """Create a temporary www root directory with common test fixtures."""
    www = tmp_path / "www"
    www.mkdir()

    # Basic HTML file
    (www / "index.html").write_text("<html><body>Hello World</body></html>")

    # Plain text file
    (www / "test.txt").write_text("test content")

    # A simple HTML page
    (www / "page.html").write_text("<html><body>Test Page</body></html>")

    # Binary file (a small PNG-like file)
    binary_data = bytes([0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) + b'\x00' * 100
    (www / "image.png").write_bytes(binary_data)

    # Large file (100KB)
    large_data = b'A' * 100000
    (www / "largefile.bin").write_bytes(large_data)

    # Zero-length file
    (www / "empty.txt").write_text("")

    # Symlink to another file in the root
    (www / "link.html").symlink_to(www / "index.html")

    # Create a subdirectory with no index
    subdir = www / "subdir"
    subdir.mkdir()
    (subdir / "nested.txt").write_text("nested content")

    # Create a readable directory with index
    subdir_index = www / "hasindex"
    subdir_index.mkdir()
    (subdir_index / "index.html").write_text("<html><body>Subdir Index</body></html>")

    # Create cgi-bin directory with test scripts
    cgi_bin = www / "cgi-bin"
    cgi_bin.mkdir()

    # Simple CGI script
    (cgi_bin / "hello.sh").write_text(
        '#!/bin/sh\necho "Content-Type: text/plain"\necho ""\necho "hello from cgi"\n'
    )
    (cgi_bin / "hello.sh").chmod(0o755)

    # CGI script that echoes query string
    (cgi_bin / "query.sh").write_text(
        '#!/bin/sh\necho "Content-Type: text/plain"\necho ""\necho "QUERY_STRING=$QUERY_STRING"\n'
    )
    (cgi_bin / "query.sh").chmod(0o755)

    # CGI script that echoes POST body
    (cgi_bin / "post.sh").write_text(
        '#!/bin/sh\necho "Content-Type: text/plain"\necho ""\ncat\n'
    )
    (cgi_bin / "post.sh").chmod(0o755)

    # CGI script that echoes environment variables
    (cgi_bin / "env.sh").write_text(
        '#!/bin/sh\necho "Content-Type: text/plain"\necho ""\nenv | sort\n'
    )
    (cgi_bin / "env.sh").chmod(0o755)

    # CGI script that exits with error
    (cgi_bin / "error.sh").write_text(
        '#!/bin/sh\necho "Content-Type: text/plain"\necho ""\necho "error output"\nexit 1\n'
    )
    (cgi_bin / "error.sh").chmod(0o755)

    # CGI script that outputs content with specific length
    (cgi_bin / "length.sh").write_text(
        '#!/bin/sh\necho "Content-Type: text/plain"\necho "Content-Length: 5"\necho ""\necho "12345"\n'
    )
    (cgi_bin / "length.sh").chmod(0o755)

    # NPH CGI script (nph- prefix)
    (cgi_bin / "nph-test.sh").write_text(
        '#!/bin/sh\necho "HTTP/1.0 200 OK"\necho "Content-Type: text/plain"\necho ""\necho "nph response"\n'
    )
    (cgi_bin / "nph-test.sh").chmod(0o755)

    # CGI script that uses PATH_INFO
    (cgi_bin / "pathinfo.sh").write_text(
        '#!/bin/sh\necho "Content-Type: text/plain"\necho ""\necho "PATH_INFO=$PATH_INFO"\necho "SCRIPT_NAME=$SCRIPT_NAME"\n'
    )
    (cgi_bin / "pathinfo.sh").chmod(0o755)

    return www


@pytest.fixture
def c_binary():
    """Path to the compiled C thttpd binary."""
    binary = os.path.join(os.path.dirname(__file__), "..", "legacy", "src", "thttpd")
    binary = os.path.abspath(binary)
    assert os.path.exists(binary), f"C binary not found at {binary}"
    return binary


@pytest.fixture
def rust_binary():
    """Path to the compiled Rust thttpd binary."""
    binary = os.path.join(os.path.dirname(__file__), "..", "rust", "target", "release", "thttpd")
    return os.path.abspath(binary)


@pytest.fixture
def server_process(c_binary, www_root):
    """Start the C thttpd server and yield (proc, port).

    Uses -D (don't daemonize) so subprocess can control it.
    Uses -c to enable CGI for cgi-bin directory.
    """
    port = find_free_port()
    proc = subprocess.Popen(
        [c_binary, "-p", str(port), "-D", "-d", str(www_root),
         "-c", "**cgi-bin**"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    # Wait for server to start accepting connections
    for _ in range(20):
        try:
            s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            s.settimeout(0.5)
            s.connect(('127.0.0.1', port))
            s.close()
            break
        except (ConnectionRefusedError, OSError):
            time.sleep(0.1)

    yield proc, port

    proc.send_signal(signal.SIGTERM)
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=2)


@pytest.fixture
def server_process_no_cgi(c_binary, www_root):
    """Start the C thttpd server without CGI enabled."""
    port = find_free_port()
    proc = subprocess.Popen(
        [c_binary, "-p", str(port), "-D", "-d", str(www_root)],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    for _ in range(20):
        try:
            s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            s.settimeout(0.5)
            s.connect(('127.0.0.1', port))
            s.close()
            break
        except (ConnectionRefusedError, OSError):
            time.sleep(0.1)

    yield proc, port

    proc.send_signal(signal.SIGTERM)
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=2)


@pytest.fixture
def server_process_with_throttle(c_binary, www_root, tmp_path):
    """Start the C thttpd server with a throttle file."""
    # Create throttle file - rate of 1000000 bytes/sec for all patterns
    throttle_file = tmp_path / "throttle.conf"
    throttle_file.write_text("*\t1000000\n")

    port = find_free_port()
    proc = subprocess.Popen(
        [c_binary, "-p", str(port), "-D", "-d", str(www_root),
         "-c", "**cgi-bin**",
         "-t", str(throttle_file)],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    for _ in range(20):
        try:
            s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            s.settimeout(0.5)
            s.connect(('127.0.0.1', port))
            s.close()
            break
        except (ConnectionRefusedError, OSError):
            time.sleep(0.1)

    yield proc, port

    proc.send_signal(signal.SIGTERM)
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=2)
