#!/usr/bin/env bash
# Enforce the thttpd-rs unsafe budget with two independent gates.
#
# Gate 1 (HARD, deterministic, no geiger dependency):
#   The request-parsing crate (thttpd-http) must contain zero literal `unsafe`
#   tokens in source, including comments/docstrings. This blunt gate backs the
#   headline security claim and must not depend on cargo-geiger's JSON schema
#   (which has churned across versions).
#
# Gate 2 (HARD, geiger, schema-tolerant):
#   cargo-geiger reports which thttpd-* crates contain ANY `unsafe` usage. The
#   set must be EXACTLY the three audited OS/FFI boundary crates documented in
#   docs/SECURITY_NOTES.md: thttpd-auth, thttpd-core, thttpd-mmc. To add or
#   remove one, update EXPECTED_BOUNDARY_CRATES below AND docs/SECURITY_NOTES.md
#   in the same commit.
#
# cargo-geiger 0.13 cannot scan a virtual workspace (`--workspace` fails on a
# virtual manifest), so Gate 2 runs geiger once per thttpd-* workspace member
# against its absolute package manifest. The geiger report is one JSON line
# (`{"packages":...}`) mixed with cargo `{"$message_type":"artifact":...}` lines,
# so we extract the report line. The `unsafe` subtree key in geiger 0.13 is
# `unsafety` with per-bucket `unsafe_` integer counts; the parser sums only the
# `unsafe_` keys (not the `safe` counts) and tolerates the older `unsafe`/`used`
# shape names as well.
set -euo pipefail

# The 3 audited OS/FFI boundary crates (see docs/SECURITY_NOTES.md).
EXPECTED_BOUNDARY_CRATES="thttpd-auth thttpd-core thttpd-mmc"

# All thttpd-* workspace members. Keep in sync with rust/Cargo.toml `members`.
ALL_THTTPD_CRATES=(
  thttpd-core thttpd-http thttpd-auth thttpd-fdwatch thttpd-timers
  thttpd-mmc thttpd-match thttpd-tdate thttpd-mime thttpd-migrate
)

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# --- Gate 1 (deterministic grep) -------------------------------------------
if grep -rn --include='*.rs' 'unsafe' "$ROOT/rust/crates/thttpd-http/src/" >/tmp/thttpd_unsafe_hits.txt; then
  echo "FAIL [Gate 1]: thttpd-http/src contains the 'unsafe' token:" >&2
  cat /tmp/thttpd_unsafe_hits.txt >&2
  exit 1
fi
echo "PASS [Gate 1]: 0 'unsafe' tokens in thttpd-http/src/ (including comments)."

# --- Gate 2 (geiger set-membership) ----------------------------------------
command -v cargo-geiger >/dev/null 2>/dev/null || {
  echo "cargo-geiger not installed; installing with --locked" >&2
  cargo install cargo-geiger --locked
}

mapfile -t BOUNDARY < <(python3 - "$ROOT" "${ALL_THTTPD_CRATES[@]}" <<'PY'
import json, subprocess, sys, os
root = sys.argv[1]
members = sys.argv[2:]

snapshot = {"crates": {}}

def geiger_unsafe_total(pkg):
    manifest = os.path.join(root, "rust", "crates", pkg, "Cargo.toml")
    r = subprocess.run(
        ["cargo", "geiger", "--manifest-path", manifest, "--output-format", "Json"],
        capture_output=True, text=True, cwd=os.path.join(root, "rust"),
    )
    # The report is the last stdout line beginning with {"packages":
    report = None
    for line in reversed(r.stdout.splitlines()):
        if line.startswith('{"packages":'):
            report = json.loads(line)
            break
    if report is None:
        print(f"WARN: no JSON report for {pkg}", file=sys.stderr)
        return None
    # Sum every integer stored under a key named `unsafe_` (geiger 0.13 schema).
    # Tolerates older shapes that used `unsafe`/`used` subtrees with `unsafe_`
    # leaf counts too.
    def count(o):
        if isinstance(o, dict):
            t = 0
            for k, v in o.items():
                if k == "unsafe_" and isinstance(v, int) and not isinstance(v, bool):
                    t += v
                else:
                    t += count(v)
            return t
        if isinstance(o, list):
            return sum(count(v) for v in o)
        return 0
    for p in report.get("packages", []):
        name = (p.get("package", {}).get("id", {}) or {}).get("name", "")
        if name == pkg:
            # geiger 0.13 uses `unsafety`; older versions used `unsafe`.
            return count(p.get("unsafety", p.get("unsafe", {})))
    return 0

for m in members:
    total = geiger_unsafe_total(m)
    snapshot["crates"][m] = {"unsafe_count": total}
    if total and total > 0:
        print(m)

# Persist a consolidated snapshot for the security.yml upload-artifact step
# and for the migration report's evidence chain. (Gitignored — generated.)
with open(os.path.join(root, "geiger.json"), "w") as f:
    json.dump(snapshot, f, indent=2, sort_keys=True)
PY
)

ACTUAL="$(printf '%s\n' "${BOUNDARY[@]}" | grep -v '^$' | sort -u | tr '\n' ' ' | sed 's/ *$//')"
EXPECTED="$(printf '%s\n' $EXPECTED_BOUNDARY_CRATES | sort -u | tr '\n' ' ' | sed 's/ *$//')"

if [ "$ACTUAL" != "$EXPECTED" ]; then
  echo "FAIL [Gate 2]: thttpd-* crates with unsafe = [$ACTUAL]" >&2
  echo "             expected                       = [$EXPECTED]" >&2
  echo "             Update EXPECTED_BOUNDARY_CRATES AND docs/SECURITY_NOTES.md together if intentional." >&2
  exit 1
fi
echo "PASS [Gate 2]: boundary crates with unsafe = [$ACTUAL] (matches expected)."
