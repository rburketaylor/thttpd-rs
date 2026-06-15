"""Pytest fixtures for thttpd golden master testing."""
import os
import socket
import subprocess
import time
import tempfile
import pytest
import signal
import stat
import hashlib


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


@pytest.fixture(scope="session")
def www_root_session(tmp_path_factory):
    """Session-scoped www root for differential tests - created once, shared across all tests."""
    tmp_path = tmp_path_factory.mktemp("thttpd_shared")
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

    # CGI script that returns Status: 418 (unknown status → "Something")
    (cgi_bin / "status_418.sh").write_text(
        '#!/bin/sh\necho "Status: 418 I am a teapot"\necho "Content-Type: text/plain"\necho ""\necho "teapot"\n'
    )
    (cgi_bin / "status_418.sh").chmod(0o755)

    # CGI script that returns only Location: header (treated as 302)
    (cgi_bin / "location_only.sh").write_text(
        '#!/bin/sh\necho "Location: /elsewhere"\necho "Content-Type: text/plain"\necho ""\n'
    )
    (cgi_bin / "location_only.sh").chmod(0o755)

    # CGI script that returns Status: 302 + Location:
    (cgi_bin / "status_302.sh").write_text(
        '#!/bin/sh\necho "Status: 302 Found"\necho "Location: /elsewhere"\necho "Content-Type: text/plain"\necho ""\n'
    )
    (cgi_bin / "status_302.sh").chmod(0o755)

    # CGI script that returns Status: 500
    (cgi_bin / "status_500.sh").write_text(
        '#!/bin/sh\necho "Status: 500 Server Error"\necho "Content-Type: text/plain"\necho ""\necho "oops"\n'
    )
    (cgi_bin / "status_500.sh").chmod(0o755)

    # CGI script that echoes all expected env vars
    (cgi_bin / "env_full.sh").write_text(
        '#!/bin/sh\necho "Content-Type: text/plain"\necho ""\n'
        'echo "REDIRECT_STATUS=200"\n'
        'echo "GATEWAY_INTERFACE=$GATEWAY_INTERFACE"\n'
        'echo "HTTP_REFERER=$HTTP_REFERER"\n'
        'echo "HTTP_USER_AGENT=$HTTP_USER_AGENT"\n'
        'echo "HTTP_ACCEPT=$HTTP_ACCEPT"\n'
        'echo "HTTP_ACCEPT_LANGUAGE=$HTTP_ACCEPT_LANGUAGE"\n'
        'echo "HTTP_ACCEPT_ENCODING=$HTTP_ACCEPT_ENCODING"\n'
        'echo "HTTP_COOKIE=$HTTP_COOKIE"\n'
        'echo "HTTP_HOST=$HTTP_HOST"\n'
    )
    (cgi_bin / "env_full.sh").chmod(0o755)

    # Auth-protected directory with .htpasswd
    secret = www / "secret"
    secret.mkdir()
    (secret / "data.txt").write_text("secret content")
    # MD5 crypt hash of "secret" with salt "abcd" — generated via:
    #   openssl passwd -1 -salt abcd secret
    # Traditional DES crypt is supported by both macOS and Linux libc:
    # crypt("secret", "ab") => abNANd1rDfiNc
    (secret / ".htpasswd").write_text("alice:abNANd1rDfiNc\n")
    (secret / ".htpasswd").chmod(0o644)

    # Non-CGI executable file (should be 403 per libhttpd.c:3790-3799)
    (www / "executable.txt").write_text("I'm executable but not CGI")
    (www / "executable.txt").chmod(0o755)

    # .tar.gz — tests figure_mime chained encoding (libhttpd.c:2607-2618)
    (www / "archive.tar.gz").write_bytes(b"fake-tar-gz-content")
    # .zzz — tests application/octet-stream default (xyz is in C's table as chemical/x-xyz)
    (www / "data.zzz").write_text("unknown extension data")

    # Circular symlink (libhttpd.c:1599 — too many symlinks)
    (www / "loop_a").symlink_to("loop_b")
    (www / "loop_b").symlink_to("loop_a")

    # Absolute-target symlink (libhttpd.c:1631 — absolute symlink)
    (www / "abs_link").symlink_to(str(www / "test.txt"))

    return www


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


@pytest.fixture(scope="session")
def c_binary():
    """Path to the compiled C thttpd binary."""
    binary = os.path.join(os.path.dirname(__file__), "..", "legacy", "src", "thttpd")
    binary = os.path.abspath(binary)
    assert os.path.exists(binary), f"C binary not found at {binary}"
    return binary


@pytest.fixture(scope="session")
def dual_server_process_vhost(c_binary, rust_binary, www_root_session):
    """Session-scoped: both servers with vhost enabled."""
    www = www_root_session
    # Set up vhost subdirectories
    vhost1 = www / "vhost1.example.com"
    vhost1.mkdir(exist_ok=True)
    (vhost1 / "index.html").write_text("<html>vhost1</html>")
    vhost2 = www / "vhost2.example.com"
    vhost2.mkdir(exist_ok=True)
    (vhost2 / "data.txt").write_text("vhost2 data")

    c_port = find_free_port()
    c_proc = subprocess.Popen(
        [c_binary, "-p", str(c_port), "-D", "-d", str(www),
         "-c", "**cgi-bin**", "-v"],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    rust_port = find_free_port()
    rust_proc = subprocess.Popen(
        [rust_binary, "-p", str(rust_port), "-D", "-d", str(www),
         "-c", "**cgi-bin**", "--vhost"],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    # Wait for both to be ready
    for port in [c_port, rust_port]:
        for _ in range(50):
            try:
                s = socket.socket()
                s.settimeout(0.2)
                s.connect(('127.0.0.1', port))
                s.close()
                break
            except (ConnectionRefusedError, OSError):
                time.sleep(0.1)
    yield c_proc, c_port, rust_proc, rust_port
    c_proc.terminate(); c_proc.wait(timeout=5)
    rust_proc.terminate(); rust_proc.wait(timeout=5)


@pytest.fixture(scope="session")
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


@pytest.fixture
def rust_server_process(rust_binary, www_root):
    """Start the Rust thttpd server and yield (proc, port).

    Uses -D (debug/don't daemonize) so subprocess can control it.
    Uses -c to enable CGI for cgi-bin directory.
    """
    port = find_free_port()
    proc = subprocess.Popen(
        [rust_binary, "-p", str(port), "-D", "-d", str(www_root),
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
def rust_server_process_no_cgi(rust_binary, www_root):
    """Start the Rust thttpd server without CGI enabled."""
    port = find_free_port()
    proc = subprocess.Popen(
        [rust_binary, "-p", str(port), "-D", "-d", str(www_root)],
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
def rust_server_process_with_throttle(rust_binary, www_root, tmp_path):
    """Start the Rust thttpd server with a throttle file."""
    # Create throttle file - rate of 1000000 bytes/sec for all patterns
    throttle_file = tmp_path / "throttle.conf"
    throttle_file.write_text("*\t1000000\n")

    port = find_free_port()
    proc = subprocess.Popen(
        [rust_binary, "-p", str(port), "-D", "-d", str(www_root),
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


def dual_compare(c_port, rust_port, request_bytes, test_name=""):
    """Send same request to both C and Rust servers, compare responses."""
    c_raw = http_request(c_port, request_bytes, timeout=2, read_timeout=2)
    rust_raw = http_request(rust_port, request_bytes, timeout=2, read_timeout=2)
    c_resp = parse_response(c_raw)
    rust_resp = parse_response(rust_raw)

    # Add fields needed by compare_responses_v2
    for resp in [c_resp, rust_resp]:
        resp['connection_result'] = 'closed'
        resp['body_sha256'] = hashlib.sha256(resp.get('body', b'')).hexdigest()
        resp['body_length'] = len(resp.get('body', b''))

    from diff_engine import compare_responses_v2
    results = compare_responses_v2(c_resp, rust_resp, test_name=test_name)

    return c_resp, rust_resp, results


@pytest.fixture(scope="session")
def dual_server_process(c_binary, rust_binary, www_root_session):
    """Session-scoped: start both C and Rust thttpd servers once, reuse across all tests."""
    www = www_root_session
    c_port = find_free_port()
    c_proc = subprocess.Popen(
        [c_binary, "-p", str(c_port), "-D", "-d", str(www), "-c", "**cgi-bin**"],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    rust_port = find_free_port()
    rust_proc = subprocess.Popen(
        [rust_binary, "-p", str(rust_port), "-D", "-d", str(www), "-c", "**cgi-bin**"],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    # Wait for both servers
    for port, proc in [(c_port, c_proc), (rust_port, rust_proc)]:
        for _ in range(20):
            try:
                s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
                s.settimeout(0.5)
                s.connect(('127.0.0.1', port))
                s.close()
                break
            except (ConnectionRefusedError, OSError):
                time.sleep(0.1)
    yield c_proc, c_port, rust_proc, rust_port
    # Cleanup both
    for proc in [c_proc, rust_proc]:
        proc.send_signal(signal.SIGTERM)
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=2)


@pytest.fixture(scope="session")
def dual_server_process_no_cgi(c_binary, rust_binary, www_root_session):
    """Session-scoped: both servers without CGI."""
    www = www_root_session
    c_port = find_free_port()
    c_proc = subprocess.Popen(
        [c_binary, "-p", str(c_port), "-D", "-d", str(www)],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    rust_port = find_free_port()
    rust_proc = subprocess.Popen(
        [rust_binary, "-p", str(rust_port), "-D", "-d", str(www)],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    for port, proc in [(c_port, c_proc), (rust_port, rust_proc)]:
        for _ in range(20):
            try:
                s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
                s.settimeout(0.5)
                s.connect(('127.0.0.1', port))
                s.close()
                break
            except (ConnectionRefusedError, OSError):
                time.sleep(0.1)
    yield c_proc, c_port, rust_proc, rust_port
    for proc in [c_proc, rust_proc]:
        proc.send_signal(signal.SIGTERM)
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=2)


@pytest.fixture(scope="session")
def dual_server_process_with_throttle(c_binary, rust_binary, www_root_session, tmp_path_factory):
    """Session-scoped: both servers with throttle file."""
    www = www_root_session
    tmp_path = tmp_path_factory.mktemp("thttpd_throttle")
    throttle_file = tmp_path / "throttle.conf"
    throttle_file.write_text("*\t1000000\n")

    c_port = find_free_port()
    c_proc = subprocess.Popen(
        [c_binary, "-p", str(c_port), "-D", "-d", str(www),
         "-c", "**cgi-bin**", "-t", str(throttle_file)],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    rust_port = find_free_port()
    rust_proc = subprocess.Popen(
        [rust_binary, "-p", str(rust_port), "-D", "-d", str(www),
         "-c", "**cgi-bin**", "-t", str(throttle_file)],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    for port, proc in [(c_port, c_proc), (rust_port, rust_proc)]:
        for _ in range(20):
            try:
                s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
                s.settimeout(0.5)
                s.connect(('127.0.0.1', port))
                s.close()
                break
            except (ConnectionRefusedError, OSError):
                time.sleep(0.1)
    yield c_proc, c_port, rust_proc, rust_port
    for proc in [c_proc, rust_proc]:
        proc.send_signal(signal.SIGTERM)
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=2)


# ======================================================================
# thttpd-migrate proxy fixtures (Phase 8 integration tests)
# ======================================================================

def wait_for_port(port, timeout=5.0):
    """Block until a TCP port on 127.0.0.1 accepts connections."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                return
        except OSError:
            time.sleep(0.05)
    raise RuntimeError(f"port {port} did not open")


@pytest.fixture
def dual_thttpd_backends(c_binary, rust_binary, www_root_session):
    """Start both C and Rust thttpd on ephemeral ports serving the same www root.

    Both servers serve identical content, so shadow diffing should report zero
    divergences. Yields a dict with process handles and addresses.
    """
    c_port = find_free_port()
    rust_port = find_free_port()
    c_proc = subprocess.Popen(
        [c_binary, "-p", str(c_port), "-D", "-d", str(www_root_session), "-c", "**cgi-bin**"],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    rust_proc = subprocess.Popen(
        [rust_binary, "-p", str(rust_port), "-D", "-d", str(www_root_session), "-c", "**cgi-bin**"],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    wait_for_port(c_port)
    wait_for_port(rust_port)
    yield {
        "c_proc": c_proc, "rust_proc": rust_proc,
        "c_addr": f"127.0.0.1:{c_port}", "rust_addr": f"127.0.0.1:{rust_port}",
    }
    for proc in (c_proc, rust_proc):
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=2)


def _proxy_binary():
    """Locate the thttpd-migrate binary (release, falling back to debug)."""
    here = os.path.dirname(__file__)
    for candidate in (
        os.path.join(here, "..", "rust", "target", "release", "thttpd-migrate"),
        os.path.join(here, "..", "rust", "target", "debug", "thttpd-migrate"),
    ):
        path = os.path.abspath(candidate)
        if os.path.exists(path):
            return path
    pytest.skip("thttpd-migrate binary not built; run `make build`")


def _short_socket_path(label="control"):
    """Return a short Unix-socket path under /tmp.

    macOS limits Unix socket paths to SUN_LEN (104) bytes; pytest's tmp_path
    is deeper than that, so the control socket must live under /tmp.
    """
    import random
    return f"/tmp/thttpd-migrate-{label}-{os.getpid()}-{random.randint(0, 1<<30)}.sock"


def write_proxy_config(tmp_path, listen, metrics, control_socket, state_path,
                       backends, weights, mode="active-active",
                       primary=None, shadow=None):
    """Render a thttpd-migrate TOML config to tmp_path and return its path."""
    c_addr = backends["c_addr"]
    rust_addr = backends["rust_addr"]
    c_w, rust_w = weights["c-thttpd"], weights["rust-thttpd"]
    lines = [
        f'listen = "{listen}"',
        'log_level = "info"',
        f'state_path = "{state_path}"',
        f'control_socket = "{control_socket}"',
        "",
        "[metrics]",
        f'listen = "{metrics}"',
        'path = "/metrics"',
        "",
        "[shadow]",
        "max_body_bytes = 1048576",
        "",
        "[backends.c-thttpd]",
        f'address = "{c_addr}"',
        f"weight = {c_w}",
        'health_path = "/"',
        "",
        "[backends.rust-thttpd]",
        f'address = "{rust_addr}"',
        f"weight = {rust_w}",
        'health_path = "/"',
        "",
        "[routing]",
        f'mode = "{mode}"',
    ]
    if primary:
        lines.append(f'primary_backend = "{primary}"')
    if shadow:
        lines.append(f'shadow_backend = "{shadow}"')
    lines.append('exclude_paths = ["/metrics"]')
    cfg_path = tmp_path / "thttpd-migrate.toml"
    cfg_path.write_text("\n".join(lines) + "\n")
    return cfg_path


@pytest.fixture
def proxy(dual_thttpd_backends, tmp_path):
    """Start the migration proxy on an ephemeral port and yield its handles.

    Writes a temp config, spawns the proxy, waits for it to listen, and tears
    down on exit. Yields a dict with addr, metrics, control_socket, state_path,
    proc, and config_path.
    """
    port = find_free_port()
    metrics_port = find_free_port()
    control_socket = _short_socket_path()
    state_path = str(tmp_path / "state.json")
    bin_path = _proxy_binary()
    cfg_path = write_proxy_config(
        tmp_path,
        listen=f"127.0.0.1:{port}",
        metrics=f"127.0.0.1:{metrics_port}",
        control_socket=control_socket,
        state_path=state_path,
        backends=dual_thttpd_backends,
        weights={"c-thttpd": 95, "rust-thttpd": 5},
    )
    proc = subprocess.Popen(
        [bin_path, "start", "--config", str(cfg_path)],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    try:
        wait_for_port(port)
    except Exception:
        out, err = proc.communicate(timeout=2)
        pytest.fail(f"proxy did not start: stdout={out!r} stderr={err!r}")
    yield {
        "addr": f"127.0.0.1:{port}",
        "port": port,
        "metrics": f"127.0.0.1:{metrics_port}",
        "control_socket": control_socket,
        "state_path": state_path,
        "proc": proc,
        "config_path": str(cfg_path),
        "bin": bin_path,
        "backends": dual_thttpd_backends,
    }
    proc.terminate()
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=5)
    try:
        os.unlink(control_socket)
    except OSError:
        pass
