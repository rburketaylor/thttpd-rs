#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

printf '\n1. Verification layers\n'
python3 -m pytest harness/test_diff_engine.py --collect-only -q | tail -1
python3 -m pytest harness/tests/ --ignore=harness/tests/test_differential.py --ignore=harness/tests/test_proxy.py --collect-only -q | tail -1
python3 -m pytest harness/tests/test_differential.py --collect-only -q | tail -1
python3 -m pytest harness/tests/test_proxy.py --collect-only -q | tail -1

printf '\n2. Representative differential tests\n'
python3 -m pytest harness/tests/test_differential.py \
  -q -k 'get_text_file or negative_content_length or auth_correct_password'

printf '\n3. Full proof\n'
printf 'Run make verify for format, lint, unit, security, legacy, parity, and proxy gates.\n'
