//! URL utilities for thttpd.
//! Translates URL handling from `legacy/src/libhttpd.c:1929-2370`.

/// Percent-decode a URL-encoded string.
pub fn percent_decode(input: &str) -> String {
    let mut result = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = &input[i + 1..i + 3];
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    result.push(byte);
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
