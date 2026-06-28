//! Shell-style glob matching for thttpd.
//! Translates `legacy/src/match.c` (91 lines).
//! Pattern syntax: `*` (no-slash any), `**` (any), `?` (single char), `|` (alternation).

/// Match a shell-style glob pattern against a filename.
///
/// Supports: `*` (any chars except `/`), `**` (any chars including `/`),
/// `?` (single char), `|` (alternation — OR of sub-patterns).
pub fn match_pattern(pattern: &str, filename: &str) -> bool {
    // Handle alternation: split on '|' and match any sub-pattern.
    // `|` is ASCII so byte-splitting is safe.
    for sub in pattern.split('|') {
        if match_single(sub.as_bytes(), filename.as_bytes()) {
            return true;
        }
    }
    false
}

/// Byte-oriented matcher — matches C's `match_one()`, which treats strings as
/// raw byte arrays (`char*`). `?` consumes one byte, `*` consumes a run of
/// non-`/` bytes, `**` consumes any run of bytes. Operating on `&[u8]`
/// instead of slicing `&str` keeps non-ASCII (multibyte UTF-8) paths from
/// panicking on a non-char-boundary slice.
fn match_single(pattern: &[u8], filename: &[u8]) -> bool {
    let mut pi = 0;
    let mut fi = 0;

    while pi < pattern.len() {
        match pattern[pi] {
            b'?' => {
                if fi >= filename.len() {
                    return false;
                }
                pi += 1;
                fi += 1;
            }
            b'*' => {
                // Check for double-star (globstar)
                if pi + 1 < pattern.len() && pattern[pi + 1] == b'*' {
                    // `**` matches anything including slashes
                    pi += 2;
                    if pi >= pattern.len() {
                        return true; // trailing ** matches everything
                    }
                    // Try matching remaining pattern at every byte position
                    for try_fi in fi..=filename.len() {
                        if match_single(&pattern[pi..], &filename[try_fi..]) {
                            return true;
                        }
                    }
                    return false;
                } else {
                    // Single `*` matches any bytes except `/`
                    pi += 1;
                    if pi >= pattern.len() {
                        // Trailing * matches remaining non-slash bytes
                        return !filename[fi..].contains(&b'/');
                    }
                    // Try matching 0..N non-slash bytes
                    for try_fi in fi..=filename.len() {
                        if try_fi > fi && filename[try_fi - 1] == b'/' {
                            break; // * doesn't cross /
                        }
                        if match_single(&pattern[pi..], &filename[try_fi..]) {
                            return true;
                        }
                    }
                    return false;
                }
            }
            _ => {
                if fi >= filename.len() || pattern[pi] != filename[fi] {
                    return false;
                }
                pi += 1;
                fi += 1;
            }
        }
    }

    fi == filename.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_star_match() {
        assert!(match_pattern("*.html", "index.html"));
        assert!(!match_pattern("*.html", "image.png"));
    }

    #[test]
    fn test_double_star() {
        assert!(match_pattern("**.cgi", "/cgi-bin/test.cgi"));
    }

    #[test]
    fn test_alternation() {
        assert!(match_pattern("*.cgi|*.sh", "test.cgi"));
        assert!(match_pattern("*.cgi|*.sh", "test.sh"));
        assert!(!match_pattern("*.cgi|*.sh", "test.html"));
    }

    #[test]
    fn test_question_mark() {
        assert!(match_pattern("test?.cgi", "test1.cgi"));
        assert!(!match_pattern("test?.cgi", "test.cgi"));
    }

    #[test]
    fn test_star_no_cross_slash() {
        assert!(match_pattern("*.cgi", "test.cgi"));
        assert!(!match_pattern("*.cgi", "sub/test.cgi"));
    }

    #[test]
    fn test_empty_pattern() {
        assert!(match_pattern("", ""));
        assert!(!match_pattern("", "file"));
    }

    #[test]
    fn test_cgi_pattern() {
        assert!(match_pattern("/cgi-bin/*|/jef/**", "/cgi-bin/hello"));
        assert!(match_pattern("/cgi-bin/*|/jef/**", "/jef/sub/deep/file"));
    }

    #[test]
    fn test_exact_match() {
        assert!(match_pattern("index.html", "index.html"));
        assert!(!match_pattern("index.html", "other.html"));
    }

    #[test]
    fn test_non_ascii_wildcards_no_panic() {
        // Non-ASCII (multibyte UTF-8) filenames matched against `*`/`**` must
        // not panic.  The old code sliced the &str at arbitrary byte offsets,
        // which panicked when a wildcard landed mid-character.
        // 'café' = c a f 0xC3 0xA9 ; trailing '*' tries every byte offset.
        assert!(match_pattern("caf*", "café"));
        assert!(match_pattern("*é", "café"));
        assert!(match_pattern("**", "café/naïve/file"));
        assert!(match_pattern("*.html", "résumé.html"));
        assert!(!match_pattern("café", "caff"));
        // Single '*' still does not cross '/' even with multibyte bytes around.
        assert!(!match_pattern("*é", "café/naïve"));
    }
}
