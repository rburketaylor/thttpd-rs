//! Static MIME type and encoding tables.
//! Translates `legacy/src/mime_types.h` and `legacy/src/mime_encodings.h`.

use std::ffi::OsStr;
use std::path::Path;

/// Returns the MIME type for a file based on its extension.
pub fn mime_type(filename: &str) -> &'static str {
    let ext = Path::new(filename)
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or("");
    match ext {
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" => "application/javascript",
        "txt" => "text/plain",
        "json" => "application/json",
        "xml" => "application/xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "tar" => "application/x-tar",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "doc" => "application/msword",
        "xls" => "application/vnd.ms-excel",
        "ppt" => "application/vnd.ms-powerpoint",
        "swf" => "application/x-shockwave-flash",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// Returns the content-encoding for compressed file extensions.
pub fn mime_encoding(filename: &str) -> Option<&'static str> {
    let ext = Path::new(filename)
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or("");
    match ext {
        "gz" => Some("x-gzip"),
        "bz2" => Some("x-bzip2"),
        "Z" => Some("x-compress"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_html() {
        assert_eq!(mime_type("index.html"), "text/html");
        assert_eq!(mime_type("index.htm"), "text/html");
    }

    #[test]
    fn test_image() {
        assert_eq!(mime_type("image.png"), "image/png");
        assert_eq!(mime_type("photo.jpg"), "image/jpeg");
        assert_eq!(mime_type("photo.jpeg"), "image/jpeg");
        assert_eq!(mime_type("anim.gif"), "image/gif");
    }

    #[test]
    fn test_unknown() {
        assert_eq!(mime_type("file.xyz"), "application/octet-stream");
    }

    #[test]
    fn test_encoding_gz() {
        assert_eq!(mime_encoding("archive.tar.gz"), Some("x-gzip"));
    }

    #[test]
    fn test_no_encoding() {
        assert_eq!(mime_encoding("index.html"), None);
    }
}
