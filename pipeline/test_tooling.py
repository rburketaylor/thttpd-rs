"""Tooling tests for pipeline scripts and legacy helpers.

Covers:
* ``generate_report.py``: a missing ``--diff-results`` file must exit nonzero
  instead of silently producing a baseline-only report.
* legacy ``htpasswd.c``: the password-file update path must use direct file
  APIs (atomic rename) rather than ``system("cp ...")``, so destination paths
  containing spaces or shell metacharacters are safe.

Run: ``python3 -m pytest pipeline/test_tooling.py -q``
"""

import json
import os
import re
import shutil
import stat
import subprocess
import sys

REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), os.pardir))


# --------------------------------------------------------------------------- #
# generate_report.py
# --------------------------------------------------------------------------- #

def _baseline_entry():
    return [
        {
            "test_name": "GET /index.html",
            "request": {"method": "GET", "path": "/index.html"},
            "response": {
                "status_code": 200,
                "status_text": "OK",
                "connection_result": "ok",
                "body_length": 5,
                "body_sha256": "abcdef0123456789" + "0" * 48,
            },
        }
    ]


def _run_report(*extra):
    return subprocess.run(
        [sys.executable, os.path.join(REPO_ROOT, "pipeline", "generate_report.py"), *extra],
        capture_output=True,
        text=True,
    )


def test_missing_diff_results_exits_nonzero(tmp_path):
    # A missing --diff-results file must NOT silently fall back to a
    # baseline-only report; it must error and exit nonzero.
    baseline = tmp_path / "baseline.json"
    baseline.write_text(json.dumps(_baseline_entry()))
    out = tmp_path / "out.html"
    missing = tmp_path / "does_not_exist.json"

    result = _run_report(
        "--baseline", str(baseline),
        "--diff-results", str(missing),
        "--output", str(out),
    )

    assert result.returncode != 0, "missing --diff-results must exit nonzero"
    assert "not found" in result.stderr.lower() or "diff results" in result.stderr.lower()
    # No report was written.
    assert not out.exists(), "no report should be written on a missing diff file"


def test_no_diff_results_still_produces_baseline_report(tmp_path):
    # Omitting --diff-results entirely is legitimate and must still succeed.
    baseline = tmp_path / "baseline.json"
    baseline.write_text(json.dumps(_baseline_entry()))
    out = tmp_path / "out.html"

    result = _run_report("--baseline", str(baseline), "--output", str(out))

    assert result.returncode == 0, result.stderr
    assert out.exists()


def test_valid_diff_results_produces_report(tmp_path):
    baseline = tmp_path / "baseline.json"
    baseline.write_text(json.dumps(_baseline_entry()))
    diff = tmp_path / "diff.json"
    # One failing test referenced by the baseline entry. Each diff entry
    # carries a `failures` list of {field, expected, actual}.
    diff.write_text(json.dumps([{
        "test_name": "GET /index.html",
        "failures": [{"field": "status", "expected": "200", "actual": "500"}],
    }]))
    out = tmp_path / "out.html"

    result = _run_report(
        "--baseline", str(baseline),
        "--diff-results", str(diff),
        "--output", str(out),
    )

    assert result.returncode == 0, result.stderr
    assert out.exists()


# --------------------------------------------------------------------------- #
# legacy htpasswd.c
# --------------------------------------------------------------------------- #

def _build_htpasswd(tmp_path):
    """Compile legacy/extras/htpasswd.c into a temporary binary."""
    cc = os.environ.get("CC", "cc")
    legacy = os.path.join(REPO_ROOT, "legacy")
    src = os.path.join(legacy, "extras", "htpasswd.c")
    binary = tmp_path / "htpasswd"
    libs = [] if sys.platform == "darwin" else ["-lcrypt"]
    cmd = [cc, "-DHAVE_CONFIG_H", "-I", legacy, "-I", os.path.join(legacy, "extras")]
    if sys.platform != "darwin":
        # legacy-config.h lives in pipeline/ and pulls in feature macros; not
        # needed for htpasswd, but harmless to keep include paths consistent.
        pass
    cmd += ["-o", str(binary), src, *libs]
    subprocess.run(cmd, check=True, capture_output=True)
    return str(binary)


def _no_shell_call_in_source():
    # Guard: the source must not invoke the shell at all. Strip C comments
    # first so documentation mentioning the old behavior doesn't trip it.
    src = os.path.join(REPO_ROOT, "legacy", "extras", "htpasswd.c")
    text = open(src).read()
    text = re.sub(r"/\*.*?\*/", "", text, flags=re.DOTALL)  # block comments
    text = re.sub(r"//[^\n]*", "", text)  # line comments
    return "system(" not in text and "popen(" not in text


def test_htpasswd_source_has_no_shell_call():
    assert _no_shell_call_in_source(), (
        "htpasswd.c must not call system()/popen() — use direct file APIs"
    )


def test_htpasswd_handles_paths_with_spaces_and_metachars(tmp_path):
    if shutil.which(os.environ.get("CC", "cc")) is None:
        import pytest
        pytest.skip("no C compiler available")

    binary = _build_htpasswd(tmp_path)

    # Destination path with spaces AND shell metacharacters. Under the old
    # system("cp %s %s") this would either break (spaces) or inject (the
    # $(...) / backticks). The literal filename must be used verbatim.
    tricky_name = "pw$(id > PWNED) `whoami`.txt"
    dest = tmp_path / "ht dir" / tricky_name
    dest.parent.mkdir()

    # Create the file (-c) with user alice.
    r = subprocess.run(
        [binary, "-c", str(dest), "alice"],
        input="secret1\n", capture_output=True, text=True,
    )
    assert r.returncode == 0, r.stderr
    assert dest.exists(), "password file must be created at the literal (tricky) path"
    assert not (tmp_path / "PWNED").exists(), "shell injection in create path"
    dest.chmod(0o644)

    # Update the file (add bob) — this was the system("cp ...") path.
    r = subprocess.run(
        [binary, str(dest), "bob"],
        input="secret2\n", capture_output=True, text=True,
    )
    assert r.returncode == 0, r.stderr
    assert dest.exists()
    assert not (tmp_path / "PWNED").exists(), "shell injection in update path"
    assert stat.S_IMODE(dest.stat().st_mode) == 0o644

    contents = dest.read_text()
    users = {line.split(":", 1)[0] for line in contents.splitlines() if ":" in line}
    assert users == {"alice", "bob"}, f"expected both users, got {users}"

    # No stray temp files left in the destination directory.
    leftovers = [p.name for p in dest.parent.iterdir()]
    assert leftovers == [tricky_name], f"temp file leaked: {leftovers}"
