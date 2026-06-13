#!/bin/bash
# Compile C thttpd binary from legacy/src/
# Modern GCC flags implicit function declarations as errors (sigset, etc.),
# so we build manually with relaxed warnings instead of using autotools make.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LEGACY_DIR="${SCRIPT_DIR}/../legacy/src"

cd "$LEGACY_DIR"

link_libraries=(-lresolv)
if [ "$(uname -s)" != "Darwin" ]; then
    link_libraries+=(-lcrypt -lrt)
fi

"${CC:-cc}" -DHAVE_CONFIG_H -I. -I"${SCRIPT_DIR}" -I.. \
    -include "${SCRIPT_DIR}/legacy-config.h" -Wno-implicit-function-declaration \
    -o thttpd thttpd.c libhttpd.c fdwatch.c timers.c mmc.c match.c tdate_parse.c \
    "${link_libraries[@]}"

echo "Built: ${LEGACY_DIR}/thttpd"
