#!/usr/bin/env bash
# Thin wrapper so devs run the same supply-chain checks CI does.
# Equivalent to `make security` (cargo audit + cargo deny + audit_unsafe.sh);
# kept as an explicit `pipeline/` entry point alongside the other scripts.
set -euo pipefail
exec make security
