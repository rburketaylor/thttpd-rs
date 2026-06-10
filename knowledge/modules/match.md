# match — Shell-Style Glob Matching

## Source
`legacy/src/match.c` → `rust/crates/thttpd-match/src/lib.rs`

## Status
Migrated. 80/80 differential tests pass.

## What It Does
Implements shell-style wildcard pattern matching (`*`, `?`, `[...]`) used for CGI pattern matching and URL filtering. Direct translation of the C `match()` function.

## Key Decisions
- Preserved the original backtracking algorithm exactly — thttpd depends on its specific glob semantics.
- No external dependencies.
