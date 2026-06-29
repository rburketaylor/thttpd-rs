//! Static MIME type and encoding tables.
//! Translates `legacy/src/mime_types.h` and `legacy/src/mime_encodings.h`,
//! implementing figure_mime() from `legacy/src/libhttpd.c:2538-2621`.

use std::ffi::OsStr;
use std::path::Path;

/// MIME type lookup result. Mirrors C's `figure_mime()`:
/// - `type` is the Content-Type (e.g. "text/html", "application/x-tar")
/// - `encoding` is the Content-Encoding (e.g. "x-gzip", "")
///
/// For chained encodings like .tar.gz, the encoding includes the inner extension:
/// .tar.gz -> Content-Encoding: x-gzip, Content-Type: application/x-tar
pub struct MimeInfo {
    pub mime_type: &'static str,
    pub encoding: Option<String>,
}

/// Compute MIME type and encoding for a filename.
/// Walks the filename's extensions right-to-left, peeling off encoding
/// extensions (gz, bz2, Z) until a type extension is found (or the
/// default `application/octet-stream` is used).
pub fn figure_mime(filename: &str) -> MimeInfo {
    let default_type = "application/octet-stream";
    let mut mime_type = default_type;
    let mut encoding: Option<String> = None;

    // Walk extensions from right to left, like C's figure_mime.
    // For each extension: check encodings table, then types table.
    // Encoding matches are accumulated (so file.tar.gz.bz2 → "x-bzip2,x-gzip").
    // Type match breaks the loop.
    let path = Path::new(filename);
    let mut components: Vec<&str> = path
        .file_name()
        .and_then(OsStr::to_str)
        .map(|n| n.split('.').collect())
        .unwrap_or_default();

    if components.is_empty() {
        return MimeInfo {
            mime_type,
            encoding,
        };
    }

    // The first element before any dot is the name, not an extension.
    // Extensions are everything after.
    let extensions: Vec<&str> = if components.len() > 1 {
        components.split_off(1)
    } else {
        // No extension at all
        return MimeInfo {
            mime_type,
            encoding,
        };
    };

    // Walk extensions in REVERSE order (rightmost first).
    let mut encoding_parts: Vec<&'static str> = Vec::new();
    let mut found_type = false;

    for ext in extensions.iter().rev() {
        // First check the encodings table
        if let Some(enc) = lookup_encoding(ext) {
            encoding_parts.push(enc);
        }
        // Then check the types table — if found, set type and break
        if let Some(t) = lookup_type(ext) {
            mime_type = t;
            found_type = true;
            break;
        }
    }

    let _ = found_type;

    // Build the encoding string by joining in REVERSE order
    // (rightmost encoding first, since we collected them right-to-left).
    // Wait — C's loop at libhttpd.c:2607 goes from n-1 to 0, so it
    // outputs encodings[last_pushed] first. Since we pushed right-to-left
    // in encoding_parts, we need to iterate encoding_parts in REVERSE
    // (which is the original order).
    if !encoding_parts.is_empty() {
        // encoding_parts is [rightmost_enc, ..., leftmost_enc]
        // C outputs them in order: [last_pushed, ..., first_pushed]
        // which is [rightmost, ..., leftmost] in our case.
        // So we keep encoding_parts as-is and join.
        encoding = Some(encoding_parts.join(","));
    }

    MimeInfo {
        mime_type,
        encoding,
    }
}

/// Look up an extension in the encodings table.
/// Returns Some("gzip") etc. for matching extensions, None otherwise.
fn lookup_encoding(ext: &str) -> Option<&'static str> {
    match ext {
        "gz" => Some("gzip"),
        "bz2" => Some("x-bzip2"),
        "Z" => Some("compress"),
        _ => None,
    }
}

/// Look up an extension in the MIME types table.
/// Returns Some("text/html") etc. for matching extensions, None otherwise.
/// If not found, the caller uses the default "application/octet-stream".
fn lookup_type(ext: &str) -> Option<&'static str> {
    match ext {
        "html" | "htm" => Some("text/html"),
        "css" => Some("text/css"),
        "js" => Some("application/javascript"),
        "txt" => Some("text/plain"),
        "json" => Some("application/json"),
        "xml" => Some("application/xml"),
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "svg" => Some("image/svg+xml"),
        "ico" => Some("image/x-icon"),
        "pdf" => Some("application/pdf"),
        "zip" => Some("application/zip"),
        "tar" => Some("application/x-tar"),
        "mp3" => Some("audio/mpeg"),
        "mp4" => Some("video/mp4"),
        "webm" => Some("video/webm"),
        "wav" => Some("audio/wav"),
        "ogg" => Some("audio/ogg"),
        "doc" => Some("application/msword"),
        "xls" => Some("application/vnd.ms-excel"),
        "ppt" => Some("application/vnd.ms-powerpoint"),
        "swf" => Some("application/x-shockwave-flash"),
        "wasm" => Some("application/wasm"),
        _ => None,
    }
}

/// Old API: returns just the MIME type. Use figure_mime() for both type and encoding.
pub fn mime_type(filename: &str) -> &'static str {
    figure_mime(filename).mime_type
}

/// Old API: returns just the encoding. Use figure_mime() for both type and encoding.
pub fn mime_encoding(filename: &str) -> Option<String> {
    figure_mime(filename).encoding
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_html() {
        let info = figure_mime("index.html");
        assert_eq!(info.mime_type, "text/html");
        assert_eq!(info.encoding, None);
    }

    #[test]
    fn test_image() {
        assert_eq!(figure_mime("image.png").mime_type, "image/png");
        assert_eq!(figure_mime("photo.jpg").mime_type, "image/jpeg");
        assert_eq!(figure_mime("photo.jpeg").mime_type, "image/jpeg");
        assert_eq!(figure_mime("anim.gif").mime_type, "image/gif");
    }

    #[test]
    fn test_unknown_extension() {
        // Unknown extension → application/octet-stream (C default)
        let info = figure_mime("file.xyz");
        assert_eq!(info.mime_type, "application/octet-stream");
        assert_eq!(info.encoding, None);
    }

    #[test]
    fn test_no_extension() {
        // No extension at all → default
        let info = figure_mime("Makefile");
        assert_eq!(info.mime_type, "application/octet-stream");
        assert_eq!(info.encoding, None);
    }

    #[test]
    fn test_gz_encoding() {
        // .gz alone → no type match (gz is only an encoding), so default
        // octet-stream + Content-Encoding: gzip
        let info = figure_mime("file.gz");
        assert_eq!(info.mime_type, "application/octet-stream");
        assert_eq!(info.encoding, Some("gzip".to_string()));
    }

    #[test]
    fn test_tar_gz_chained() {
        // .tar.gz → type is tar, encoding is gzip
        let info = figure_mime("archive.tar.gz");
        assert_eq!(info.mime_type, "application/x-tar");
        assert_eq!(info.encoding, Some("gzip".to_string()));
    }

    #[test]
    fn test_tar_bz2() {
        let info = figure_mime("archive.tar.bz2");
        assert_eq!(info.mime_type, "application/x-tar");
        assert_eq!(info.encoding, Some("x-bzip2".to_string()));
    }

    #[test]
    fn test_tar_gz_bz2_chained() {
        let info = figure_mime("archive.tar.gz.bz2");
        assert_eq!(info.mime_type, "application/x-tar");
        assert_eq!(info.encoding, Some("x-bzip2,gzip".to_string()));
    }

    #[test]
    fn test_encoded_files_in_loop_no_leak() {
        // figure_mime must not leak on every call for encoded files.  The old
        // implementation used Box::leak to turn the combined encoding into a
        // &'static str, so each call for a .tar.gz file permanently leaked a
        // String.  Now the encoding is owned and dropped with the MimeInfo.
        // We assert correctness over many iterations; the leak itself is
        // structural (no Box::leak in the source) — this test guards against
        // regressions and confirms stable behavior across a loop.
        for _ in 0..10000 {
            let info = figure_mime("archive.tar.gz");
            assert_eq!(info.mime_type, "application/x-tar");
            assert_eq!(info.encoding.as_deref(), Some("gzip"));
            // Encoded-only file with no type extension.
            let info2 = figure_mime("data.gz");
            assert_eq!(info2.encoding.as_deref(), Some("gzip"));
            let info3 = figure_mime("big.tar.gz.bz2");
            assert_eq!(info3.encoding.as_deref(), Some("x-bzip2,gzip"));
        }
    }
}
