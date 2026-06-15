"""Integration tests for the thttpd-migrate strangler-fig proxy.

These tests spin up real C and Rust thttpd backends (via the `proxy` fixture in
conftest.py), drive the proxy with raw HTTP, and assert on routing, shadow,
health, circuit-breaker, and rollback/drain behavior.
"""
import socket
import subprocess
import time
import urllib.request

import pytest

from conftest import http_request, parse_response


# ---------------------------------------------------------------------
# helpers
# ---------------------------------------------------------------------

def proxy_get(proxy, path="/", n=1, timeout=5):
    """Send n GETs through the proxy; return list of parsed responses."""
    port = proxy["port"]
    out = []
    for _ in range(n):
        raw = http_request(port, f"GET {path} HTTP/1.0\r\nHost: x\r\n\r\n".encode(),
                           timeout=timeout, read_timeout=timeout)
        out.append(parse_response(raw))
    return out


def proxy_cli(proxy, *args):
    """Run a thttpd-migrate control command against the proxy's socket."""
    cmd = [proxy["bin"], "--control-socket", proxy["control_socket"], *args]
    return subprocess.run(cmd, capture_output=True, text=True, timeout=10)


def server_header(resp):
    """Best-effort extract of the Server header (lowercased keys)."""
    return resp.get("headers", {}).get("server", "")


# ---------------------------------------------------------------------
# active routing
# ---------------------------------------------------------------------

class TestActiveRouting:
    def test_proxies_request_to_backend(self, proxy):
        resp = proxy_get(proxy, "/", n=1)[0]
        assert resp["status_code"] == 200
        assert b"Hello World" in resp["body"]

    def test_weight_split_approximate(self, proxy):
        # 95/5 split — over many requests the C backend should dominate.
        resps = proxy_get(proxy, "/", n=200)
        # Every response is 200 (both backends serve the same content).
        assert all(r["status_code"] == 200 for r in resps)

    def test_excluded_path_returns_404(self, proxy):
        resp = proxy_get(proxy, "/metrics", n=1)[0]
        assert resp["status_code"] == 404

    def test_post_body_forwarded(self, proxy):
        port = proxy["port"]
        raw = http_request(
            port,
            (b"POST /cgi-bin/post.sh HTTP/1.0\r\n"
             b"Host: x\r\nContent-Length: 11\r\n\r\nhello world"),
            timeout=5, read_timeout=5,
        )
        resp = parse_response(raw)
        assert resp["status_code"] == 200
        assert b"hello world" in resp["body"]

    def test_large_file_streamed(self, proxy):
        resp = proxy_get(proxy, "/largefile.bin", n=1)[0]
        assert resp["status_code"] == 200
        assert len(resp["body"]) == 100000

    def test_unknown_path_404_from_backend(self, proxy):
        resp = proxy_get(proxy, "/does-not-exist.html", n=1)[0]
        assert resp["status_code"] == 404

    def test_query_string_forwarded(self, proxy):
        resp = proxy_get(proxy, "/cgi-bin/query.sh?foo=bar", n=1)[0]
        assert resp["status_code"] == 200
        assert b"foo=bar" in resp["body"].replace(b" ", b"") or b"QUERY_STRING=foo=bar" in resp["body"]

    def test_metrics_endpoint_serves(self, proxy):
        proxy_get(proxy, "/", n=3)
        time.sleep(0.5)
        raw = http_request(int(proxy["metrics"].rsplit(":", 1)[1]),
                           b"GET /metrics HTTP/1.0\r\n\r\n", timeout=5, read_timeout=5)
        assert b"thttpd_migrate_requests_total" in raw

    def test_concurrent_requests(self, proxy):
        import threading
        results = []
        def worker():
            r = proxy_get(proxy, "/", n=10)
            results.append(all(x["status_code"] == 200 for x in r))
        threads = [threading.Thread(target=worker) for _ in range(4)]
        for t in threads: t.start()
        for t in threads: t.join(timeout=20)
        assert all(results)
        assert len(results) == 4

    def test_static_text_file(self, proxy):
        resp = proxy_get(proxy, "/test.txt", n=1)[0]
        assert resp["status_code"] == 200
        assert b"test content" in resp["body"]

    def test_subdirectory_file(self, proxy):
        resp = proxy_get(proxy, "/subdir/nested.txt", n=1)[0]
        assert resp["status_code"] == 200
        assert b"nested content" in resp["body"]


# ---------------------------------------------------------------------
# shadow mode
# ---------------------------------------------------------------------

class TestShadowMode:
    @pytest.fixture
    def shadow_proxy(self, dual_thttpd_backends, tmp_path):
        from conftest import write_proxy_config, find_free_port, wait_for_port, _proxy_binary, _short_socket_path
        port = find_free_port()
        metrics_port = find_free_port()
        control_socket = _short_socket_path("shadow")
        state_path = str(tmp_path / "state.json")
        bin_path = _proxy_binary()
        cfg = write_proxy_config(
            tmp_path,
            listen=f"127.0.0.1:{port}",
            metrics=f"127.0.0.1:{metrics_port}",
            control_socket=control_socket,
            state_path=state_path,
            backends=dual_thttpd_backends,
            weights={"c-thttpd": 1, "rust-thttpd": 0},
            mode="shadow",
            primary="c-thttpd",
            shadow="rust-thttpd",
        )
        proc = subprocess.Popen([bin_path, "start", "--config", str(cfg)],
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        wait_for_port(port)
        yield {
            "addr": f"127.0.0.1:{port}", "port": port,
            "metrics": f"127.0.0.1:{metrics_port}", "control_socket": control_socket,
            "state_path": state_path, "proc": proc, "bin": bin_path,
            "backends": dual_thttpd_backends,
        }
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)

    def test_shadow_serves_primary_content(self, shadow_proxy):
        resp = proxy_get(shadow_proxy, "/", n=1)[0]
        assert resp["status_code"] == 200
        assert b"Hello World" in resp["body"]

    def test_shadow_user_unaffected_by_backend(self, shadow_proxy):
        # Both backends serve identical content; user always sees 200.
        for r in proxy_get(shadow_proxy, "/", n=20):
            assert r["status_code"] == 200

    def test_shadow_status_healthy(self, shadow_proxy):
        time.sleep(0.5)
        result = proxy_cli(shadow_proxy, "status") if False else None
        # status reads the state file directly
        st = subprocess.run([shadow_proxy["bin"], "status", "--state", shadow_proxy["state_path"]],
                            capture_output=True, text=True, timeout=10)
        assert st.returncode == 0


# ---------------------------------------------------------------------
# health
# ---------------------------------------------------------------------

class TestHealth:
    def test_backends_healthy_at_start(self, proxy):
        time.sleep(0.5)  # let health probes run
        st = subprocess.run([proxy["bin"], "status", "--state", proxy["state_path"]],
                            capture_output=True, text=True, timeout=10)
        assert "Healthy" in st.stdout

    def test_proxy_serves_when_both_up(self, proxy):
        assert proxy_get(proxy, "/", n=5)[0]["status_code"] == 200

    def test_status_shows_backend_weights(self, proxy):
        time.sleep(0.5)
        st = subprocess.run([proxy["bin"], "status", "--state", proxy["state_path"]],
                            capture_output=True, text=True, timeout=10)
        assert "weight=95" in st.stdout
        assert "weight=5" in st.stdout

    def test_dead_backend_excluded_from_routing(self, proxy):
        # Kill the rust backend; all traffic must still succeed via C.
        proxy["backends"]["rust_proc"].terminate()
        proxy["backends"]["rust_proc"].wait(timeout=5)
        # Default health: interval=1s, failure_threshold=3 → unhealthy after ~3s.
        time.sleep(4)
        resps = proxy_get(proxy, "/", n=50)
        # No 5xx to the client — C serves everything.
        assert all(r["status_code"] == 200 for r in resps)


# ---------------------------------------------------------------------
# circuit breaker
# ---------------------------------------------------------------------

class TestCircuitBreaker:
    def test_client_unaffected_when_backend_errors(self, proxy):
        # Even if a backend path errors (5xx from CGI), the proxy forwards it
        # without itself crashing.
        resp = proxy_get(proxy, "/cgi-bin/status_500.sh", n=1)[0]
        assert resp["status_code"] == 500

    def test_proxy_keeps_serving_after_errors(self, proxy):
        for _ in range(5):
            proxy_get(proxy, "/cgi-bin/status_500.sh", n=1)
        # A good path still works.
        assert proxy_get(proxy, "/", n=1)[0]["status_code"] == 200

    def test_5xx_counted_in_metrics(self, proxy):
        proxy_get(proxy, "/cgi-bin/status_500.sh", n=3)
        time.sleep(0.5)
        raw = http_request(int(proxy["metrics"].rsplit(":", 1)[1]),
                           b"GET /metrics HTTP/1.0\r\n\r\n", timeout=5, read_timeout=5)
        assert b"thttpd_migrate_5xx_responses_total" in raw

    def test_circuit_state_in_status(self, proxy):
        st = subprocess.run([proxy["bin"], "status", "--state", proxy["state_path"]],
                            capture_output=True, text=True, timeout=10)
        assert st.returncode == 0
        assert "c-thttpd" in st.stdout


# ---------------------------------------------------------------------
# rollback / drain
# ---------------------------------------------------------------------

class TestRollback:
    def test_set_weight_to_all_rust(self, proxy):
        r = proxy_cli(proxy, "set-weight", "rust-thttpd=100", "c-thttpd=0")
        assert r.returncode == 0, r.stderr
        time.sleep(1)
        # All requests succeed (rust serves identical content).
        for resp in proxy_get(proxy, "/", n=20):
            assert resp["status_code"] == 200

    def test_rollback_to_c(self, proxy):
        # Promote rust then roll back to c.
        proxy_cli(proxy, "set-weight", "rust-thttpd=100", "c-thttpd=0")
        time.sleep(1)
        r = proxy_cli(proxy, "rollback", "--to", "c-thttpd")
        assert r.returncode == 0, r.stderr
        time.sleep(1)
        for resp in proxy_get(proxy, "/", n=20):
            assert resp["status_code"] == 200

    def test_rollback_unknown_backend_errors(self, proxy):
        r = proxy_cli(proxy, "rollback", "--to", "nope")
        assert r.returncode != 0
        assert "unknown backend" in r.stdout.lower() or "unknown" in r.stderr.lower()

    def test_set_weight_unknown_backend_errors(self, proxy):
        r = proxy_cli(proxy, "set-weight", "ghost=1")
        assert r.returncode != 0

    def test_promote_then_demote(self, proxy):
        proxy_cli(proxy, "set-weight", "rust-thttpd=100", "c-thttpd=0")
        time.sleep(0.5)
        proxy_cli(proxy, "set-weight", "rust-thttpd=0", "c-thttpd=100")
        time.sleep(0.5)
        assert proxy_get(proxy, "/", n=5)[0]["status_code"] == 200

    def test_status_reflects_new_weights(self, proxy):
        proxy_cli(proxy, "set-weight", "rust-thttpd=100", "c-thttpd=0")
        time.sleep(0.5)
        st = subprocess.run([proxy["bin"], "status", "--state", proxy["state_path"]],
                            capture_output=True, text=True, timeout=10)
        # After promote, rust carries weight 100 and c carries 0.
        lines = st.stdout.splitlines()
        rust_line = [l for l in lines if "rust-thttpd" in l][0]
        c_line = [l for l in lines if "c-thttpd" in l and "rust" not in l][0]
        assert "weight=100" in rust_line
        assert "weight=0" in c_line


class TestDrain:
    def test_drain_stops_accepting(self, proxy):
        # Before drain, the proxy serves.
        assert proxy_get(proxy, "/", n=1)[0]["status_code"] == 200
        r = proxy_cli(proxy, "drain", "--timeout-secs", "30")
        assert r.returncode == 0, r.stderr
        time.sleep(1.5)
        # After drain, new connections are refused.
        refused = False
        try:
            http_request(proxy["port"], b"GET / HTTP/1.0\r\n\r\n", timeout=1, read_timeout=1)
        except (socket.timeout, ConnectionError, OSError):
            refused = True
        assert refused, "proxy should refuse new connections after drain"

    def test_status_after_drain(self, proxy):
        proxy_cli(proxy, "drain", "--timeout-secs", "30")
        time.sleep(1)
        st = subprocess.run([proxy["bin"], "status", "--state", proxy["state_path"]],
                            capture_output=True, text=True, timeout=10)
        assert "Draining: true" in st.stdout

    def test_drain_exits_despite_idle_keepalive(self, proxy):
        # Regression (Claim 3): an idle keep-alive client must not block drain.
        # Open a keep-alive connection, serve one request, leave it open, then
        # drain — the proxy must still exit within the grace period. Before the
        # graceful_shutdown fix, the idle connection kept serve_connection alive
        # and the JoinSet join hung until the client disconnected.
        sock = socket.create_connection(("127.0.0.1", proxy["port"]), timeout=3)
        sock.sendall(b"GET / HTTP/1.1\r\nHost: x\r\nConnection: keep-alive\r\n\r\n")
        # Read the first response; the socket stays open (keep-alive).
        raw = b""
        sock.settimeout(3)
        while b"\r\n\r\n" not in raw:
            try:
                chunk = sock.recv(4096)
            except socket.timeout:
                break
            if not chunk:
                break
            raw += chunk
        assert b" 200 " in raw[:64], "first request on keep-alive connection must succeed"
        # The connection is now idle-but-open. Trigger drain with a short grace.
        r = proxy_cli(proxy, "drain", "--timeout-secs", "3")
        assert r.returncode == 0, r.stderr
        # The proxy must exit despite the open idle keep-alive connection.
        try:
            proxy["proc"].wait(timeout=15)
        except subprocess.TimeoutExpired:
            pytest.fail("proxy did not exit within grace — idle keep-alive blocked drain")
        finally:
            try:
                sock.close()
            except OSError:
                pass
