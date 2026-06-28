//! URL utilities for thttpd.
//! Translates URL handling from `legacy/src/libhttpd.c:1929-2370`.

/// Percent-decode a URL-encoded string.
///
/// Operates purely on bytes so malformed `%` escapes and multibyte UTF-8
/// never panic. Mirrors C's `strdecode()` (legacy/src/libhttpd.c), which
/// treats strings as byte arrays: a `%XX` escape is two hex bytes, anything
/// else is copied verbatim, and a stray `%` with no two hex digits is left
/// literal. `+` decodes to space (form-encoding).
pub fn percent_decode(input: &str) -> String {
    let mut result = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                // Read the two hex nibbles from the byte slice rather than
                // slicing the &str, which would panic on a non-char-boundary.
                if let (Some(hi), Some(lo)) = (from_hex(bytes[i + 1]), from_hex(bytes[i + 2])) {
                    result.push(hi * 16 + lo);
                    i += 3;
                } else {
                    result.push(bytes[i]);
                    i += 1;
                }
            }
            b'+' => {
                result.push(b' ');
                i += 1;
            }
            _ => {
                result.push(bytes[i]);
                i += 1;
            }
        }
    }

    String::from_utf8_lossy(&result).to_string()
}

/// Decode a single ASCII hex nibble to its 0..=15 value, or `None` if not hex.
fn from_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Normalize a URL path: resolve `.` and `..` components, reject traversal above root.
/// Returns None if the path attempts to escape root.
pub fn normalize_path(path: &str) -> Option<String> {
    // Reject paths containing double-slash (//) — matches C behavior
    if path.contains("//") {
        return None;
    }

    let mut components: Vec<&str> = Vec::new();

    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                components.pop()?; // traversal above root
            }
            _ => {
                components.push(part);
            }
        }
    }

    let normalized = if components.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", components.join("/"))
    };

    Some(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_percent_decode_simple() {
        assert_eq!(percent_decode("hello%20world"), "hello world");
    }

    #[test]
    fn test_percent_decode_plus() {
        assert_eq!(percent_decode("a+b"), "a b");
    }

    #[test]
    fn test_percent_decode_hex() {
        assert_eq!(percent_decode("%41%42%43"), "ABC");
    }

    #[test]
    fn test_percent_decode_malformed_no_panic() {
        // Stray percent and incomplete escapes must not panic and must be
        // copied literally (matches C's strdecode).  The old code sliced the
        // &str at byte offsets which panicked on these inputs.
        assert_eq!(percent_decode("foo%"), "foo%");
        assert_eq!(percent_decode("foo%4"), "foo%4");
        assert_eq!(percent_decode("%ZZ"), "%ZZ");
        assert_eq!(percent_decode("%4Z"), "%4Z");
        assert_eq!(percent_decode("100%done"), "100%done");
    }

    #[test]
    fn test_percent_decode_multibyte_no_panic() {
        // A multibyte UTF-8 char following a percent: the bytes after `%` are
        // continuation bytes, not ASCII hex.  Must not panic and must leave
        // the `%` literal since the following two bytes are not hex.
        let input = "%é"; // '%', then 0xC3 0xA9
        assert_eq!(percent_decode(input), "%é");
    }

    #[test]
    fn test_normalize_simple() {
        assert_eq!(normalize_path("/foo/bar"), Some("/foo/bar".to_string()));
    }

    #[test]
    fn test_normalize_dotdot() {
        assert_eq!(normalize_path("/foo/../bar"), Some("/bar".to_string()));
    }

    #[test]
    fn test_normalize_traversal() {
        assert_eq!(normalize_path("/../../etc/passwd"), None);
    }

    #[test]
    fn test_normalize_double_slash() {
        assert_eq!(normalize_path("//test.txt"), None);
        assert_eq!(normalize_path("/foo//bar"), None);
    }
}
