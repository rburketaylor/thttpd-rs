//! Shell-style glob matching for thttpd.
//! Translates `legacy/src/match.c` (91 lines).
//! Pattern syntax: `*` (no-slash any), `**` (any), `?` (single char), `|` (alternation).

/// Match a shell-style glob pattern against a filename.
///
/// Supports: `*` (any chars except `/`), `**` (any chars including `/`),
/// `?` (single char), `|` (alternation — OR of sub-patterns).
pub fn match_pattern(pattern: &str, filename: &str) -> bool {
    // Handle alternation: split on '|' and match any sub-pattern.
    for sub in pattern.split('|') {
        if match_single(sub, filename) {
            return true;
        }
    }
    false
}

fn match_single(pattern: &str, filename: &str) -> bool {
    let mut pi = 0;
    let mut fi = 0;
    let pbytes = pattern.as_bytes();
    let fbytes = filename.as_bytes();

    while pi < pbytes.len() {
        match pbytes[pi] {
            b'?' => {
                if fi >= fbytes.len() {
                    return false;
                }
                pi += 1;
                fi += 1;
            }
            b'*' => {
                // Check for double-star (globstar)
                if pi + 1 < pbytes.len() && pbytes[pi + 1] == b'*' {
                    // `**` matches anything including slashes
                    pi += 2;
                    if pi >= pbytes.len() {
                        return true; // trailing ** matches everything
                    }
                    // Try matching remaining pattern at every position
                    for try_fi in fi..=fbytes.len() {
                        if match_single(&pattern[pi..], &filename[try_fi..]) {
                            return true;
                        }
                    }
                    return false;
                } else {
                    // Single `*` matches any chars except `/`
                    pi += 1;
                    if pi >= pbytes.len() {
                        // Trailing * matches remaining non-slash chars
                        return !fbytes[fi..].contains(&b'/');
                    }
                    // Try matching 0..N non-slash chars
                    for try_fi in fi..=fbytes.len() {
                        if try_fi > fi && fbytes[try_fi - 1] == b'/' {
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
                if fi >= fbytes.len() || pbytes[pi] != fbytes[fi] {
                    return false;
                }
                pi += 1;
                fi += 1;
            }
        }
    }

    fi == fbytes.len()
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
}
