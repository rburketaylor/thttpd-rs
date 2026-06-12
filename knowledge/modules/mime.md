# mime — MIME type/encoding lookup

**Source:** `legacy/src/mime_types.h`, `legacy/src/mime_encodings.h`
**Rust crate:** `rust/crates/thttpd-mime/`

Maps file extensions to MIME types and content encodings (gzip, compress, etc.).
Tables are converted from C `#define`s to Rust `match` statements. Static lookup
at request time; no I/O.
