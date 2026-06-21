#!/usr/bin/env bash
# Scan the published thttpd-rs container image for known vulnerabilities with
# trivy. Part of the Phase 7 release security sign-off. Requires trivy on PATH
# (https://aquasecurity.github.io/trivy/).
#
# This is the container half of the security picture; the binary / dependency
# half is `make security` (cargo audit + deny + geiger). The full container
# hardening work lives in the separate "Container + k8s deployment artifacts"
# plan.
set -euo pipefail

IMAGE="${1:-thttpd-rs:latest}"
SEVERITY="${THHTTPD_SCAN_SEVERITY:-HIGH,CRITICAL}"

command -v trivy >/dev/null 2>/dev/null || {
  echo "trivy is required: see https://aquasecurity.github.io/trivy/latest/getting-started/installation/" >&2
  exit 2
}

echo "Scanning $IMAGE for $SEVERITY findings..."
# --exit-code 1 makes the scan fail on any HIGH/CRITICAL finding.
trivy image --severity "$SEVERITY" --exit-code 1 "$IMAGE"
echo "PASS: no $SEVERITY findings in $IMAGE."
