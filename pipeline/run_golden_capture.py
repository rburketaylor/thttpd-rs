#!/usr/bin/env python3
"""Golden master capture runner.

Starts the C binary, runs all test cases, captures baseline.json.

Usage:
    python3 pipeline/run_golden_capture.py [--output PATH] [--port PORT]
"""

import argparse
import hashlib
import json
import os
import signal
import socket
import subprocess
import sys
import tempfile
import time

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------
SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
PROJECT_ROOT = os.path.join(SCRIPT_DIR, "..")
C_BINARY = os.path.join(PROJECT_ROOT, "legacy", "src", "thttpd")
DEFAULT_OUTPUT = os.path.join(PROJECT_ROOT, "harness", "golden", "baseline.json")

# ---------------------------------------------------------------------------
# HTTP helpers (no external deps)
# ---------------------------------------------------------------------------

def find_free_port():
    """Find a free TCP port."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("", 0))
        return s.getsockname()[1]


def http_request(host, port, method=None, path=None, headers=None, body=None,
                 timeout=3, http_version="HTTP/1.0", raw_request=None):
    """Send an HTTP request over a raw socket and return the full raw response bytes."""
    if headers is None:
        headers = {}
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    sock.settimeout(timeout)
    try:
        sock.connect((host, port))
        if raw_request is not None:
            sock.sendall(raw_request)
        else:
            # Build request
            req_line = f"{method} {path} {http_version}\r\n"
            # Default Host header
            if "Host" not in headers:
                headers["Host"] = f"{host}:{port}"
            hdr_lines = "".join(f"{k}: {v}\r\n" for k, v in headers.items())
            payload = req_line + hdr_lines + "\r\n"
            if body:
                payload = payload.encode() + body
            else:
                payload = payload.encode()
            sock.sendall(payload)

        # Read response – try to detect end of response efficiently.
        # Strategy: read until we have full headers, then if Content-Length
        # is present read exactly that many body bytes.  Otherwise read until
        # connection close or timeout.
        data = b""
        # Set a short per-recv timeout for the initial header read
        sock.settimeout(2)
        while True:
            try:
                chunk = sock.recv(4096)
                if not chunk:
                    break
                data += chunk
                # Check if we have complete headers
                if b"\r\n\r\n" in data:
                    hdr_end = data.index(b"\r\n\r\n")
                    header_block = data[:hdr_end].decode("latin-1", errors="replace")
                    body_so_far = data[hdr_end + 4:]
                    # Extract Content-Length from headers
                    cl = None
                    for line in header_block.split("\r\n")[1:]:
                        if line.lower().startswith("content-length:"):
                            try:
                                cl = int(line.split(":", 1)[1].strip())
                            except ValueError:
                                pass
                            break
                    if cl is not None and len(body_so_far) >= cl:
                        # We have all the body
                        break
                    elif cl is not None:
                        # Read remaining body bytes
                        remaining = cl - len(body_so_far)
                        while remaining > 0:
                            try:
                                chunk = sock.recv(min(remaining, 4096))
                                if not chunk:
                                    break
                                data += chunk
                                remaining -= len(chunk)
                            except socket.timeout:
                                break
                        break
                    # No Content-Length – keep reading until close/timeout
            except socket.timeout:
                break
        return data
    except (ConnectionRefusedError, ConnectionResetError, OSError) as exc:
        return None
    finally:
        try:
            sock.close()
        except OSError:
            pass


def parse_response(raw):
    """Parse raw HTTP response bytes into structured dict.

    Returns dict with keys:
        status_code, status_text, headers (ordered dict), body,
        body_sha256, body_length, connection_result
    If raw is None (connection failed), returns a failure sentinel.
    """
    if raw is None:
        return {
            "status_code": 0,
            "status_text": "",
            "headers": {},
            "body": b"",
            "body_sha256": hashlib.sha256(b"").hexdigest(),
            "body_length": 0,
            "connection_result": "refused",
        }

    # Split headers from body
    try:
        hdr_end = raw.index(b"\r\n\r\n")
        header_block = raw[:hdr_end].decode("latin-1")
        body = raw[hdr_end + 4:]
    except ValueError:
        # No header/body separator – treat everything as body
        header_block = raw.decode("latin-1", errors="replace")
        body = b""

    lines = header_block.split("\r\n")
    status_line = lines[0] if lines else ""

    # Parse status line: HTTP/x.x CODE TEXT
    # HTTP/0.9 responses have no status line – treat as 200 with body = everything
    parts = status_line.split(" ", 2)
    if parts[0].startswith("HTTP/"):
        status_code = int(parts[1]) if len(parts) >= 2 else 0
        status_text = parts[2] if len(parts) >= 3 else ""
    else:
        # HTTP/0.9 – entire raw response is body
        return {
            "status_code": 200,
            "status_text": "OK",
            "headers": {},
            "body": raw,
            "body_sha256": hashlib.sha256(raw).hexdigest(),
            "body_length": len(raw),
            "connection_result": "ok",
        }

    # Parse headers (ordered)
    headers = {}
    for line in lines[1:]:
        if ":" in line:
            key, val = line.split(":", 1)
            headers[key.strip()] = val.strip()

    return {
        "status_code": status_code,
        "status_text": status_text,
        "headers": headers,
        "body": body,
        "body_sha256": hashlib.sha256(body).hexdigest(),
        "body_length": len(body),
        "connection_result": "ok",
    }


def response_to_jsonable(resp):
    """Convert a parsed response to a JSON-serializable dict (body as hex)."""
    return {
        "status_code": resp["status_code"],
        "status_text": resp["status_text"],
        "headers": resp["headers"],
        "body_sha256": resp["body_sha256"],
        "body_length": resp["body_length"],
        "connection_result": resp["connection_result"],
    }

# ---------------------------------------------------------------------------
# WWW root fixture builder
# ---------------------------------------------------------------------------

def create_www_root(base_dir):
    """Create a temp www root with test fixture files.

    Returns the path to the www root directory.
    """
    www = os.path.join(base_dir, "www")
    os.makedirs(www, exist_ok=True)

    # Static files
    with open(os.path.join(www, "index.html"), "w") as f:
        f.write("<html><head><title>Test</title></head><body>Hello World</body></html>")

    with open(os.path.join(www, "test.txt"), "w") as f:
        f.write("test content\n")

    # Binary file (256 bytes of 0x00..0xFF)
    with open(os.path.join(www, "binary.bin"), "wb") as f:
        f.write(bytes(range(256)))

    # Large file (128 KB)
    with open(os.path.join(www, "largefile.dat"), "wb") as f:
        f.write(b"A" * (128 * 1024))

    # Zero-length file
    with open(os.path.join(www, "empty.txt"), "w") as f:
        pass

    # Symlink to index.html
    os.symlink("index.html", os.path.join(www, "link.html"))

    # Symlink pointing outside the www root (for 403 tests)
    os.symlink("/etc/passwd", os.path.join(www, "escaped_link.html"))

    # Forbidden file (no read permission)
    forbidden = os.path.join(www, "forbidden.txt")
    with open(forbidden, "w") as f:
        f.write("secret")
    os.chmod(forbidden, 0o000)

    # Subdirectory without index
    subdir = os.path.join(www, "subdir")
    os.makedirs(subdir, exist_ok=True)
    with open(os.path.join(subdir, "nested.txt"), "w") as f:
        f.write("nested content")

    # CGI directory with scripts
    cgi_dir = os.path.join(www, "cgi-bin")
    os.makedirs(cgi_dir, exist_ok=True)

    # Simple CGI that returns hello
    simple_cgi = os.path.join(cgi_dir, "hello.sh")
    with open(simple_cgi, "w") as f:
        f.write("#!/bin/sh\n")
        f.write('echo "Content-Type: text/plain"\n')
        f.write('echo ""\n')
        f.write('echo "Hello from CGI"\n')
    os.chmod(simple_cgi, 0o755)

    # CGI that echoes QUERY_STRING
    query_cgi = os.path.join(cgi_dir, "query.sh")
    with open(query_cgi, "w") as f:
        f.write("#!/bin/sh\n")
        f.write('echo "Content-Type: text/plain"\n')
        f.write('echo ""\n')
        f.write('echo "QUERY_STRING=$QUERY_STRING"\n')
    os.chmod(query_cgi, 0o755)

    # CGI that reads POST body from stdin
    post_cgi = os.path.join(cgi_dir, "post.sh")
    with open(post_cgi, "w") as f:
        f.write("#!/bin/sh\n")
        f.write('echo "Content-Type: text/plain"\n')
        f.write('echo ""\n')
        f.write('cat\n')
    os.chmod(post_cgi, 0o755)

    # CGI that prints environment variables
    env_cgi = os.path.join(cgi_dir, "env.sh")
    with open(env_cgi, "w") as f:
        f.write("#!/bin/sh\n")
        f.write('echo "Content-Type: text/plain"\n')
        f.write('echo ""\n')
        f.write('env | grep -E "^(SERVER|GATEWAY|REQUEST|QUERY|CONTENT|PATH_INFO|REMOTE|HTTP)" | sort\n')
    os.chmod(env_cgi, 0o755)

    # CGI that exits with error
    fail_cgi = os.path.join(cgi_dir, "fail.sh")
    with open(fail_cgi, "w") as f:
        f.write("#!/bin/sh\n")
        f.write('echo "Content-Type: text/plain" 1>&2\n')
        f.write('echo "" 1>&2\n')
        f.write('echo "CGI error" 1>&2\n')
        f.write('exit 1\n')
    os.chmod(fail_cgi, 0o755)

    # NPH CGI (sends raw HTTP response)
    nph_cgi = os.path.join(cgi_dir, "nph-test.sh")
    with open(nph_cgi, "w") as f:
        f.write("#!/bin/sh\n")
        f.write('echo "HTTP/1.0 200 OK"\n')
        f.write('echo "Content-Type: text/plain"\n')
        f.write('echo ""\n')
        f.write('echo "NPH response"\n')
    os.chmod(nph_cgi, 0o755)

    # CGI that prints CONTENT_LENGTH
    content_len_cgi = os.path.join(cgi_dir, "contentlen.sh")
    with open(content_len_cgi, "w") as f:
        f.write("#!/bin/sh\n")
        f.write('echo "Content-Type: text/plain"\n')
        f.write('echo ""\n')
        f.write('echo "CONTENT_LENGTH=$CONTENT_LENGTH"\n')
    os.chmod(content_len_cgi, 0o755)

    # CGI that prints PATH_INFO
    pathinfo_cgi = os.path.join(cgi_dir, "pathinfo.sh")
    with open(pathinfo_cgi, "w") as f:
        f.write("#!/bin/sh\n")
        f.write('echo "Content-Type: text/plain"\n')
        f.write('echo ""\n')
        f.write('echo "PATH_INFO=$PATH_INFO"\n')
        f.write('echo "PATH_TRANSLATED=$PATH_TRANSLATED"\n')
    os.chmod(pathinfo_cgi, 0o755)

    return www


# ---------------------------------------------------------------------------
# Test case definitions
# ---------------------------------------------------------------------------

def build_test_cases():
    """Return a list of (test_name, kwargs) tuples.

    Each kwargs dict is passed to http_request().
    """
    cases = []

    # --- Static Files ---
    cases.append(("static.get_index", {
        "method": "GET", "path": "/"
    }))
    cases.append(("static.get_index_html", {
        "method": "GET", "path": "/index.html"
    }))
    cases.append(("static.get_text_file", {
        "method": "GET", "path": "/test.txt"
    }))
    cases.append(("static.get_binary_file", {
        "method": "GET", "path": "/binary.bin"
    }))
    cases.append(("static.get_large_file", {
        "method": "GET", "path": "/largefile.dat"
    }))
    cases.append(("static.get_zero_length_file", {
        "method": "GET", "path": "/empty.txt"
    }))
    cases.append(("static.get_symlink", {
        "method": "GET", "path": "/link.html"
    }))
    cases.append(("static.head_text_file", {
        "method": "HEAD", "path": "/test.txt"
    }))
    cases.append(("static.if_modified_since_304", {
        "method": "GET", "path": "/test.txt",
        "headers": {"If-Modified-Since": "Sun, 08 Jun 2030 00:00:00 GMT"}
    }))
    cases.append(("static.if_modified_since_200", {
        "method": "GET", "path": "/test.txt",
        "headers": {"If-Modified-Since": "Mon, 01 Jan 2000 00:00:00 GMT"}
    }))
    cases.append(("static.range_request", {
        "method": "GET", "path": "/test.txt",
        "headers": {"Range": "bytes=0-4"}
    }))

    # --- Errors ---
    cases.append(("errors.404_not_found", {
        "method": "GET", "path": "/nonexistent.html"
    }))
    cases.append(("errors.403_forbidden", {
        "method": "GET", "path": "/forbidden.txt"
    }))
    cases.append(("errors.403_symlink_escape", {
        "method": "GET", "path": "/escaped_link.html"
    }))
    cases.append(("errors.501_not_implemented", {
        "method": "DELETE", "path": "/test.txt"
    }))
    cases.append(("errors.directory_without_index", {
        "method": "GET", "path": "/subdir/"
    }))

    # --- Headers ---
    cases.append(("headers.content_type_html", {
        "method": "GET", "path": "/index.html"
    }))
    cases.append(("headers.content_type_txt", {
        "method": "GET", "path": "/test.txt"
    }))
    cases.append(("headers.server_header_present", {
        "method": "GET", "path": "/test.txt"
    }))
    cases.append(("headers.date_header_present", {
        "method": "GET", "path": "/test.txt"
    }))
    cases.append(("headers.last_modified_present", {
        "method": "GET", "path": "/test.txt"
    }))

    # --- Connection ---
    cases.append(("connection.basic_get", {
        "method": "GET", "path": "/test.txt", "http_version": "HTTP/1.0"
    }))
    cases.append(("connection.http11_get", {
        "method": "GET", "path": "/test.txt", "http_version": "HTTP/1.1"
    }))

    # --- Edge Cases ---
    cases.append(("edge.very_long_url", {
        "method": "GET", "path": "/" + "a" * 500 + ".html"
    }))
    cases.append(("edge.special_chars_url", {
        "method": "GET", "path": "/test%2Etxt"
    }))
    cases.append(("edge.double_slash", {
        "method": "GET", "path": "//test.txt"
    }))
    cases.append(("edge.directory_traversal", {
        "method": "GET", "path": "/../etc/passwd"
    }))
    cases.append(("edge.post_to_static", {
        "method": "POST", "path": "/test.txt",
        "body": b"payload"
    }))
    cases.append(("edge.head_request", {
        "method": "HEAD", "path": "/index.html"
    }))
    cases.append(("edge.http09_simple", {
        "raw_request": b"GET /test.txt\r\n"
    }))

    # --- Malformed ---
    cases.append(("malformed.invalid_method", {
        "method": "FOOBAR", "path": "/test.txt"
    }))
    cases.append(("malformed.invalid_version", {
        "raw_request": b"GET /test.txt HTTP/9.9\r\nHost: localhost\r\n\r\n"
    }))
    cases.append(("malformed.truncated_request", {
        "raw_request": b"GET /test.tx"
    }))
    cases.append(("malformed.binary_garbage", {
        "raw_request": b"\x00\x01\x02\x03\x04\x05"
    }))
    cases.append(("malformed.very_long_header", {
        "method": "GET", "path": "/test.txt",
        "headers": {"X-Long": "A" * 4000}
    }))
    cases.append(("malformed.negative_content_length", {
        "method": "POST", "path": "/test.txt",
        "headers": {"Content-Length": "-1"},
        "body": b""
    }))

    # --- CGI ---
    cases.append(("cgi.simple_cgi", {
        "method": "GET", "path": "/cgi-bin/hello.sh"
    }))
    cases.append(("cgi.query_string", {
        "method": "GET", "path": "/cgi-bin/query.sh?foo=bar&baz=1"
    }))
    cases.append(("cgi.post_body", {
        "method": "POST", "path": "/cgi-bin/post.sh",
        "headers": {"Content-Length": "11"},
        "body": b"hello world"
    }))
    cases.append(("cgi.env_variables", {
        "method": "GET", "path": "/cgi-bin/env.sh"
    }))
    cases.append(("cgi.fail_script", {
        "method": "GET", "path": "/cgi-bin/fail.sh"
    }))
    cases.append(("cgi.nph_cgi", {
        "method": "GET", "path": "/cgi-bin/nph-test.sh"
    }))
    cases.append(("cgi.content_length", {
        "method": "POST", "path": "/cgi-bin/contentlen.sh",
        "headers": {"Content-Length": "5"},
        "body": b"abcde"
    }))
    cases.append(("cgi.path_info", {
        "method": "GET", "path": "/cgi-bin/pathinfo.sh/extra/path"
    }))
    cases.append(("cgi.cgi_not_found", {
        "method": "GET", "path": "/cgi-bin/nonexistent.sh"
    }))

    return cases


# ---------------------------------------------------------------------------
# Server management
# ---------------------------------------------------------------------------

def start_server(binary, port, www_root):
    """Start thttpd binary, return subprocess.Popen object."""
    proc = subprocess.Popen(
        [binary, "-p", str(port), "-D", "-d", www_root, "-c", "**/*.sh"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    # Wait for server to bind
    time.sleep(0.5)
    # Verify it's alive
    if proc.poll() is not None:
        stdout = proc.stdout.read().decode("latin-1", errors="replace")
        stderr = proc.stderr.read().decode("latin-1", errors="replace")
        raise RuntimeError(
            f"Server process exited immediately (rc={proc.returncode}).\n"
            f"stdout: {stdout}\nstderr: {stderr}"
        )
    return proc


def stop_server(proc, timeout=5):
    """Stop the server process cleanly."""
    if proc.poll() is not None:
        return
    try:
        proc.send_signal(signal.SIGTERM)
    except OSError:
        return
    try:
        proc.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=2)


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    parser = argparse.ArgumentParser(description="Golden master capture runner")
    parser.add_argument("--output", default=DEFAULT_OUTPUT,
                        help="Path to write baseline.json")
    parser.add_argument("--port", type=int, default=None,
                        help="Port to use (default: auto-detect)")
    parser.add_argument("--host", default="127.0.0.1",
                        help="Host to connect to")
    args = parser.parse_args()

    if not os.path.isfile(C_BINARY):
        print(f"ERROR: C binary not found at {C_BINARY}", file=sys.stderr)
        print("Run: bash pipeline/build_legacy.sh", file=sys.stderr)
        sys.exit(1)

    # Create temp www root
    tmpdir = tempfile.mkdtemp(prefix="thttpd_golden_")
    www_root = create_www_root(tmpdir)

    port = args.port or find_free_port()

    print(f"Starting C binary: {C_BINARY}")
    print(f"  port={port}  www_root={www_root}")

    proc = start_server(C_BINARY, port, www_root)
    try:
        cases = build_test_cases()
        print(f"Running {len(cases)} test cases...")

        baseline = []
        for test_name, kwargs in cases:
            raw = http_request(args.host, port, **kwargs)
            parsed = parse_response(raw)
            entry = {
                "test_name": test_name,
                "request": {k: (v.decode("latin-1") if isinstance(v, bytes) else v)
                            for k, v in kwargs.items()},
                "response": response_to_jsonable(parsed),
            }
            baseline.append(entry)
            code = parsed["status_code"]
            print(f"  {test_name}: {code} ({parsed['connection_result']})")

    finally:
        stop_server(proc)

    # Write baseline
    os.makedirs(os.path.dirname(os.path.abspath(args.output)), exist_ok=True)
    with open(args.output, "w") as f:
        json.dump(baseline, f, indent=2)

    print(f"\nCaptured {len(baseline)} responses → {args.output}")

    # Cleanup temp dir
    import shutil
    shutil.rmtree(tmpdir, ignore_errors=True)


if __name__ == "__main__":
    main()
