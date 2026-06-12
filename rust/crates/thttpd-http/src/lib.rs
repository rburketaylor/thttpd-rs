//! HTTP protocol library for thttpd.
//! Translates `legacy/src/libhttpd.c` and `legacy/src/libhttpd.h`.

pub mod cgi;
pub mod conn;
pub mod dirlist;
pub mod error;
pub mod auth;
pub mod method;
pub mod parse;
pub mod parse_state;
pub mod response;
pub mod url;

pub use conn::HttpConn;
pub use error::HttpError;
pub use method::Method;
pub use parse_state::ParseState;
