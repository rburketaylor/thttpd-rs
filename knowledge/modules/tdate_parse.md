# tdate_parse — HTTP Date Parsing

## Source
`legacy/src/tdate_parse.c` (330 lines) → `rust/crates/thttpd-tdate/src/lib.rs` (228 lines)

## Status
Migrated. 105 differential scenarios pass.

## What It Does
Parses HTTP date formats for `If-Modified-Since` conditional requests. Supports RFC 1123 (`Sun, 06 Nov 1994 08:49:37 GMT`), RFC 850 (`Sunday, 06-Nov-94 08:49:37 GMT`), and `asctime()` format (`Sun Nov  6 08:49:37 1994`).

## Key Decisions
- Preserves the original multi-format parsing approach — tries each format in sequence.
- Returns seconds-since-epoch as `i64`, matching C's `time_t`.
