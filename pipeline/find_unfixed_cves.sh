#!/usr/bin/env bash
# Scan legacy/src/*.c for C patterns that historically hosted thttpd CVEs.
#
# Emits a candidate list (file:line + matched pattern) grouped by the CWE class
# the pattern belongs to. In Phase 1 this only BUILDS the candidate list; the
# "returns 0 uncovered" gate lives in Phase 2/3, where every candidate must be
# mapped to a CWE row in docs/security/C_PATTERNS.md and a Rust mitigation in
# docs/security/RUST_MITIGATIONS.md.
#
# Exit codes:
#   0  — ran and emitted a candidate list (possibly empty). NOT a pass/fail gate.
#   2  — legacy/src not found (nothing to scan).
set -euo pipefail

SRC="${1:-legacy/src}"
if [ ! -d "$SRC" ]; then
  echo "ERROR: $SRC not found — run from repo root." >&2
  exit 2
fi

count=0
emit() {  # <CWE label> <pattern regex>
  local label="$1" pat="$2"
  # Skip self-references in this file's own comments. Match .c/.h only.
  local hits
  hits=$(grep -rnE --include='*.c' --include='*.h' "$pat" "$SRC" 2>/dev/null || true)
  if [ -n "$hits" ]; then
    echo "## $label"
    echo "$hits"
    echo
    count=$((count + $(printf '%s\n' "$hits" | wc -l)))
  fi
}

echo "# Candidate risky C patterns in $SRC"
echo "(Grouped by CWE class. Each candidate must be mapped to a Rust mitigation"
echo " in docs/security/RUST_MITIGATIONS.md — see Phase 2/3 of the security plan.)"
echo

# CWE-120/787 — classic / out-of-bounds buffer write: fixed-size buffer + unbounded copy
emit "CWE-120/787 (Classic Buffer Overflow / OOB Write)" \
  '(strcat|strcpy|sprintf|vsprintf|gets)\('

# CWE-125 — out-of-bounds read: memcpy/strncpy with attacker-influenced length
emit "CWE-125 (Out-of-bounds Read)" \
  'memcpy\(|memmove\(|strncpy\(|strncat\('

# CWE-476 — NULL pointer deref: bare derefs of malloc/strdup/etc. returns
emit "CWE-476 (NULL Pointer Dereference)" \
  '(malloc|calloc|realloc|strdup)\([^)]*\)\s*;[^=]*(->|\.)'

# CWE-20 — improper input validation: atoi/atol returning 0 indistinguishable from error
emit "CWE-20 (Improper Input Validation — atoi/atol sentinels)" \
  '\b(atoi|atol|atoll)\('

# CWE-22 — path traversal: de_dotdot / filename manipulation
emit "CWE-22 (Path Traversal — de_dotdot / backslash variants)" \
  'de_dotdot|%5[cC]|\\\\\\.\\.'

# CWE-78 — OS command injection: system/popen with concatenated input
emit "CWE-78 (OS Command Injection)" \
  '\bsystem\(|\bpopen\('

# CWE-59/377 — symlink race / insecure temp file
emit "CWE-59/377 (Insecure Temp File / Symlink Race)" \
  'tmpfile|tmpnam|mktemp\b|/tmp/'

# CWE-79 — reflected XSS (error pages echoing unescaped input)
emit "CWE-79 (Reflected XSS — error page echoing)" \
  'snprintf\([^)]*%(s|.*s)'

# CWE-668/732 — resource exposed to wrong sphere / wrong permissions
emit "CWE-668/732 (Resource Exposure / Wrong Permissions)" \
  'open\([^)]*O_CREAT|chmod\(|fchmod\(|umask\('

echo "----"
echo "Total candidate hits: $count (classify each in docs/security/C_PATTERNS.md)"
