//! HTTP protocol library for thttpd.
//! Translates `legacy/src/libhttpd.c` and `legacy/src/libhttpd.h`.

// Basic-Auth lives in the dedicated `thttpd-auth` crate (the crypt(3) FFI is
// one of the three audited OS-boundary sites — see docs/SECURITY_NOTES.md).
// It is re-exported here as `auth` so the 8 call sites in thttpd-core's
// eventloop.rs keep compiling with `thttpd_http::auth::*` unchanged, while
// cargo-geiger honestly reports this crate as free of raw memory operations
// (enforced by the security-CI gate in pipeline/).
pub use thttpd_auth as auth;

pub mod cgi;
pub mod conn;
pub mod dirlist;
pub mod error;
pub mod method;
pub mod parse;
pub mod parse_state;
pub mod response;
pub mod url;

pub use conn::HttpConn;
pub use error::HttpError;
pub use method::Method;
pub use parse_state::ParseState;
