//! MIME type lookup for thttpd.
//! Generated tables from `mime_types.h` and `mime_encodings.h`.

mod types;

pub use types::{MimeInfo, figure_mime, mime_encoding, mime_type};
