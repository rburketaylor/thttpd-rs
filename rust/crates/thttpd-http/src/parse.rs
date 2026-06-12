//! Request parsing for thttpd.
//! Translates `legacy/src/libhttpd.c:1769-1925` incremental FSM parser.

use crate::method::Method;
use crate::parse_state::{GotRequest, ParseState};

/// Run the request-detection FSM over new data in `read_buf`.
/// `checked_idx` is where we left off; `read_idx` is end of valid data.
/// `initial_state` is the parser state from the previous call (persists
/// across incremental reads, matching C's `hc->checked_state`).
/// Returns (result, new_checked_idx, final_state).
///
/// The transition table is byte-exact with `libhttpd.c:1773-1922`.
pub fn got_request(read_buf: &[u8], mut checked_idx: usize, read_idx: usize, initial_state: ParseState) -> (GotRequest, usize, ParseState) {
    let mut state = initial_state;

    while checked_idx < read_idx {
        let c = read_buf[checked_idx];

        state = match state {
            ParseState::FirstWord => match c {
                b' ' | b'\t' => ParseState::FirstWs,
                b'\r' | b'\n' => {
                    // CR/LF before a complete word is malformed
                    return (GotRequest::BadRequest, checked_idx, ParseState::Bogus);
                }
                _ => ParseState::FirstWord,
            },
            ParseState::FirstWs => match c {
                b' ' | b'\t' => ParseState::FirstWs,
                b'\r' | b'\n' => {
                    // Whitespace then CR/LF is malformed (libhttpd.c:1794-1797)
                    return (GotRequest::BadRequest, checked_idx, ParseState::Bogus);
                }
                _ => ParseState::SecondWord,
            },
            ParseState::SecondWord => match c {
                b' ' | b'\t' => ParseState::SecondWs,
                b'\r' | b'\n' => {
                    // HTTP/0.9: two-word request
                    return (GotRequest::GotRequest, checked_idx + 1, ParseState::GotRequest);
                }
                _ => ParseState::SecondWord,
            },
            ParseState::SecondWs => match c {
                b' ' | b'\t' => ParseState::SecondWs,
                b'\r' | b'\n' => {
                    // Whitespace then CR/LF is malformed (libhttpd.c:1818-1821)
                    return (GotRequest::BadRequest, checked_idx, ParseState::Bogus);
                }
                _ => ParseState::ThirdWord,
            },
            ParseState::ThirdWord => match c {
                b' ' | b'\t' => ParseState::ThirdWs,
                b'\r' => ParseState::Cr,
                b'\n' => ParseState::Lf,
                _ => ParseState::ThirdWord,
            },
            ParseState::ThirdWs => match c {
                b' ' | b'\t' => ParseState::ThirdWs,
                b'\r' => ParseState::Cr,
                b'\n' => ParseState::Lf,
                _ => {
                    // Non-LF/CR/WS after request line is malformed (libhttpd.c:1851-1854)
                    return (GotRequest::BadRequest, checked_idx, ParseState::Bogus);
                }
            },
            ParseState::Lf => match c {
                b'\r' => ParseState::Cr,
                b'\n' => return (GotRequest::GotRequest, checked_idx + 1, ParseState::GotRequest),
                _ => ParseState::Line,
            },
            ParseState::Cr => match c {
                b'\n' => ParseState::Crlf,
                b'\r' => {
                    // Two CRs in a row — end of request (libhttpd.c:1887-1889)
                    return (GotRequest::GotRequest, checked_idx + 1, ParseState::GotRequest);
                }
                _ => ParseState::Line,
            },
            ParseState::Crlf => match c {
                b'\r' => ParseState::Crlfcr,
                b'\n' => return (GotRequest::GotRequest, checked_idx + 1, ParseState::GotRequest),
                _ => ParseState::Line,
            },
            ParseState::Line => match c {
                b'\r' => ParseState::Cr,
                b'\n' => ParseState::Lf,
                _ => ParseState::Line,
            },
            ParseState::Crlfcr => match c {
                b'\r' | b'\n' => {
                    // Two CRLFs or two CRs in a row — end of request
                    // (libhttpd.c:1912-1914). Rust previously only accepted \n.
                    return (GotRequest::GotRequest, checked_idx + 1, ParseState::GotRequest);
                }
                _ => ParseState::Line,
            },
            ParseState::GotRequest | ParseState::Bogus => {
                return (GotRequest::NoRequest, checked_idx, state);
            }
        };

        checked_idx += 1;
    }

    (GotRequest::NoRequest, checked_idx, state)
}

/// Parse method from the first word of the request line.
pub fn parse_method(read_buf: &[u8], end: usize) -> Method {
    let word: Vec<u8> = read_buf[..end]
        .iter()
        .take_while(|&&b| b != b' ' && b != b'\t')
        .copied()
        .collect();
    Method::from_str(&String::from_utf8_lossy(&word))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_get() {
        let buf = b"GET / HTTP/1.0\r\n\r\n";
        let (result, _, _) = got_request(buf, 0, buf.len(), ParseState::FirstWord);
        assert_eq!(result, GotRequest::GotRequest);
    }

    #[test]
    fn test_incomplete_request() {
        let buf = b"GET / HTTP/1.0\r\n";
        let (result, _, _) = got_request(buf, 0, buf.len(), ParseState::FirstWord);
        assert_eq!(result, GotRequest::NoRequest);
    }

    #[test]
    fn test_http09_two_word() {
        let buf = b"GET /\r\n";
        let (result, _, _) = got_request(buf, 0, buf.len(), ParseState::FirstWord);
        assert_eq!(result, GotRequest::GotRequest);
    }

    #[test]
    fn test_bad_request() {
        let buf = b"GET / HTTP/1.0\r\n\rX";
        let (result, _, _) = got_request(buf, 0, buf.len(), ParseState::FirstWord);
        // After CRLF+CR, non-LF goes to Line state — not Bogus.
        // A truly bad request is one with CR/LF in FirstWord.
        let buf2 = b"\rGET / HTTP/1.0\r\n\r\n";
        let (result2, _, _) = got_request(buf2, 0, buf2.len(), ParseState::FirstWord);
        assert_eq!(result2, GotRequest::BadRequest);
    }

    #[test]
    fn test_headers_with_body() {
        let buf = b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n";
        let (result, _, _) = got_request(buf, 0, buf.len(), ParseState::FirstWord);
        assert_eq!(result, GotRequest::GotRequest);
    }

    #[test]
    fn test_incremental_byte_by_byte() {
        // Simulate data arriving one byte at a time — the core incremental FSM case
        let buf = b"GET / HTTP/1.0\r\n\r\n";
        let mut checked_idx = 0;
        let mut state = ParseState::FirstWord;
        for i in 0..buf.len() {
            let (result, new_checked, new_state) = got_request(buf, checked_idx, i + 1, state.clone());
            checked_idx = new_checked;
            state = new_state;
            if i < buf.len() - 1 {
                assert_eq!(result, GotRequest::NoRequest, "should not complete at byte {}", i);
            } else {
                assert_eq!(result, GotRequest::GotRequest, "should complete at last byte");
            }
        }
    }

    #[test]
    fn test_parse_method() {
        assert_eq!(parse_method(b"GET / HTTP/1.0\r\n", 14), Method::Get);
        assert_eq!(parse_method(b"POST / HTTP/1.0\r\n", 15), Method::Post);
        assert_eq!(parse_method(b"HEAD / HTTP/1.0\r\n", 15), Method::Head);
    }

    // ---- Phase 1 FSM terminator tests (libhttpd.c byte-exact parity) ----

    /// Crlfcr state: C accepts both \r and \n as terminator (libhttpd.c:1912).
    /// Previously Rust only accepted \n.
    #[test]
    fn test_crlfcr_cr_terminator() {
        // \r\n\r\r — the second \r in the Crlfcr state ends the request
        let buf = b"GET / HTTP/1.0\r\n\r\r";
        let (result, _, _) = got_request(buf, 0, buf.len(), ParseState::FirstWord);
        assert_eq!(result, GotRequest::GotRequest);
    }

    /// Cr state: two CRs in a row end the request (libhttpd.c:1887-1889).
    #[test]
    fn test_cr_cr_terminator() {
        let buf = b"GET / HTTP/1.0\r\r";
        let (result, _, _) = got_request(buf, 0, buf.len(), ParseState::FirstWord);
        assert_eq!(result, GotRequest::GotRequest);
    }

    /// Lf state: \r goes to Cr (not Crlf). libhttpd.c:1873-1875.
    /// Combined with the Cr→Crlf transition, \n\r\n is NOT end of request.
    #[test]
    fn test_lf_cr_not_end() {
        // \n\r\n — C considers this incomplete (Lf→Cr, Cr→Crlf, no \n)
        let buf = b"GET / HTTP/1.0\n\r\n";
        let (result, _, _) = got_request(buf, 0, buf.len(), ParseState::FirstWord);
        // Crlf state, not end of request
        assert_eq!(result, GotRequest::NoRequest);
    }

    /// Lf state: two \n in a row end the request (libhttpd.c:1867-1872).
    #[test]
    fn test_lf_lf_terminator() {
        let buf = b"GET / HTTP/1.0\n\n";
        let (result, _, _) = got_request(buf, 0, buf.len(), ParseState::FirstWord);
        assert_eq!(result, GotRequest::GotRequest);
    }

    /// FirstWs with \r or \n is malformed (libhttpd.c:1794-1797).
    /// Previously Rust treated \r in FirstWs as start of SecondWord.
    #[test]
    fn test_firstws_cr_is_bad() {
        // "GET \r" — first word is "GET", then whitespace, then CR. C: 400. Rust: was 200.
        let buf = b"GET \r\n\r\n";
        let (result, _, _) = got_request(buf, 0, buf.len(), ParseState::FirstWord);
        assert_eq!(result, GotRequest::BadRequest);
    }

    /// SecondWs with \r or \n is malformed (libhttpd.c:1818-1821).
    #[test]
    fn test_secondws_cr_is_bad() {
        // "GET / \r" — C: 400. Rust: was 200.
        let buf = b"GET / \r\n\r\n";
        let (result, _, _) = got_request(buf, 0, buf.len(), ParseState::FirstWord);
        assert_eq!(result, GotRequest::BadRequest);
    }

    /// ThirdWs with non-CR/LF/WS is malformed (libhttpd.c:1851-1854).
    #[test]
    fn test_thirdws_garbage_is_bad() {
        // "GET / HTTP/1.0 X" — extra non-WS char after version is bad
        let buf = b"GET / HTTP/1.0 X\r\n\r\n";
        let (result, _, _) = got_request(buf, 0, buf.len(), ParseState::FirstWord);
        assert_eq!(result, GotRequest::BadRequest);
    }
}
