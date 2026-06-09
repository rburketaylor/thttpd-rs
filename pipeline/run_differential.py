#!/usr/bin/env python3
"""Differential test runner.

Starts the Rust binary, replays baseline requests, diffs responses.

Usage:
    python3 pipeline/run_differential.py --baseline PATH [--strict] [--port PORT]
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
RUST_BINARY = os.path.join(PROJECT_ROOT, "rust", "target", "debug", "thttpd")

# Reuse helpers from run_golden_capture
sys.path.insert(0, SCRIPT_DIR)
from run_golden_capture import (
    find_free_port,
    http_request,
    parse_response,
    response_to_jsonable,
    create_www_root,
    start_server,
    stop_server,
)

# Import the diff engine
sys.path.insert(0, os.path.join(PROJECT_ROOT, "harness"))
from diff_engine import compare_responses


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    parser = argparse.ArgumentParser(description="Differential test runner")
    parser.add_argument("--baseline", required=True,
                        help="Path to baseline.json from golden capture")
    parser.add_argument("--strict", action="store_true",
                        help="Also check body_sha256 (binary-exact match)")
    parser.add_argument("--port", type=int, default=None,
                        help="Port to use (default: auto-detect)")
    parser.add_argument("--host", default="127.0.0.1",
                        help="Host to connect to")
    parser.add_argument("--binary", default=None,
                        help="Path to server binary (default: Rust debug build)")
    args = parser.parse_args()

    binary = args.binary or RUST_BINARY
    if not os.path.isfile(binary):
        print(f"ERROR: Binary not found at {binary}", file=sys.stderr)
        if binary == RUST_BINARY:
            print("Run: cargo build --manifest-path rust/Cargo.toml", file=sys.stderr)
        sys.exit(1)

    if not os.path.isfile(args.baseline):
        print(f"ERROR: Baseline not found at {args.baseline}", file=sys.stderr)
        print("Run: python3 pipeline/run_golden_capture.py", file=sys.stderr)
        sys.exit(1)

    # Load baseline
    with open(args.baseline) as f:
        baseline = json.load(f)
    print(f"Loaded {len(baseline)} test cases from {args.baseline}")

    # Create temp www root (same fixture layout as golden capture)
    tmpdir = tempfile.mkdtemp(prefix="thttpd_diff_")
    www_root = create_www_root(tmpdir)

    port = args.port or find_free_port()

    print(f"Starting server: {binary}")
    print(f"  port={port}  www_root={www_root}")

    proc = start_server(binary, port, www_root)
    try:
        passed = 0
        failed = 0
        errors = []

        for entry in baseline:
            test_name = entry["test_name"]
            req = entry["request"]
            expected = entry["response"]

            # Rebuild kwargs for http_request
            kwargs = {}
            if "raw_request" in req:
                kwargs["raw_request"] = req["raw_request"].encode("latin-1")
            else:
                kwargs["method"] = req.get("method", "GET")
                kwargs["path"] = req.get("path", "/")
                kwargs["http_version"] = req.get("http_version", "HTTP/1.0")
                # Reconstruct headers dict from request
                hdrs = {}
                for k, v in req.items():
                    if k in ("method", "path", "http_version", "body", "raw_request"):
                        continue
                    # The request dict may have stored headers with lowercase names
                    # We need to pass them through
                    hdrs[k] = v
                # Override with explicit Host if not present
                if "Host" not in hdrs and "host" not in hdrs:
                    hdrs["Host"] = f"{args.host}:{port}"
                kwargs["headers"] = hdrs
                if "body" in req:
                    kwargs["body"] = req["body"].encode("latin-1")

            raw = http_request(args.host, port, **kwargs)
            actual = response_to_jsonable(parse_response(raw))

            # Compare using diff engine
            diffs = compare_responses(expected, actual)

            # Check each field
            test_passed = True
            failures = []
            for d in diffs:
                # Skip body_sha256 unless --strict
                if d["field"] == "body_sha256" and not args.strict:
                    continue
                if not d["match"]:
                    test_passed = False
                    failures.append(d)

            if test_passed:
                passed += 1
                print(f"  PASS  {test_name}")
            else:
                failed += 1
                print(f"  FAIL  {test_name}")
                for d in failures:
                    print(f"        {d['field']}: expected={d['expected']!r}  actual={d['actual']!r}")
                errors.append({
                    "test_name": test_name,
                    "failures": failures,
                    "expected": expected,
                    "actual": actual,
                })

    finally:
        stop_server(proc)

    # Cleanup
    import shutil
    shutil.rmtree(tmpdir, ignore_errors=True)

    # Summary
    total = passed + failed
    print(f"\n{'='*60}")
    print(f"Results: {passed}/{total} passed, {failed} failed")
    if args.strict:
        print("  (strict mode: body_sha256 checked)")
    print(f"{'='*60}")

    sys.exit(0 if failed == 0 else 1)


if __name__ == "__main__":
    main()
