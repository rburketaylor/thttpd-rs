#!/bin/bash
# Compile C thttpd binary from legacy/src/
# Modern GCC flags implicit function declarations as errors (sigset, etc.),
# so we build manually with relaxed warnings instead of using autotools make.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LEGACY_DIR="${SCRIPT_DIR}/../legacy/src"

# Ensure autotools has been bootstrapped (needed for config.h)
if [ ! -f "${SCRIPT_DIR}/../legacy/config.h" ]; then
    cd "${SCRIPT_DIR}/../legacy"
    bash autogen.sh
    ./configure
fi

cd "$LEGACY_DIR"
gcc -DHAVE_CONFIG_H -I. -I.. -Wno-implicit-function-declaration \
    -o thttpd thttpd.c libhttpd.c fdwatch.c timers.c mmc.c match.c tdate_parse.c \
    -lcrypt -lrt -lresolv

echo "Built: ${LEGACY_DIR}/thttpd"
