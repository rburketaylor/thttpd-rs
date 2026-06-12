//! MIME type lookup for thttpd.
//! Generated tables from `mime_types.h` and `mime_encodings.h`.

mod types;

pub use types::{figure_mime, mime_encoding, mime_type, MimeInfo};
