//! Response diffing for shadow mode.
//!
//! Ports the comparison logic from `harness/diff_engine.py` (`compare_responses_v2`
//! with the *normalized* profile): timestamp headers are matched format-only,
//! temp-directory paths and CGI dynamic ports are normalized before comparison,
//! and bodies are compared by normalized SHA-256 hash and length.
//!
//! The proxy never blocks the user on the shadow backend, and divergences are
//! logged/metered — never propagated to the client.

use bytes::Bytes;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Status,
    Headers,
    Body,
    ContentLength,
    ConnectionLifecycle,
}

impl Field {
    pub fn as_str(self) -> &'static str {
        match self {
            Field::Status => "status",
            Field::Headers => "headers",
            Field::Body => "body",
            Field::ContentLength => "content_length",
            Field::ConnectionLifecycle => "connection_lifecycle",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    pub field: Field,
    pub expected: String,
    pub actual: String,
    pub path: String,
    pub method: String,
    pub truncated: bool,
}

impl Divergence {
    fn new(
        field: Field,
        expected: impl Into<String>,
        actual: impl Into<String>,
        ctx: &RequestContext,
    ) -> Self {
        Self {
            field,
            expected: expected.into(),
            actual: actual.into(),
            path: ctx.path.clone(),
            method: ctx.method.clone(),
            truncated: false,
        }
    }
}

/// Context from the inbound request needed by the diff engine.
#[derive(Debug, Clone)]
pub struct RequestContext {
    pub path: String,
    pub method: String,
    pub request_id: String,
}

/// Compare a primary and shadow response under the normalized profile.
///
/// `*_truncated` indicates the respective body was truncated at the cap; the
/// comparison then records a `Body` divergence with `truncated: true` so the
/// operator knows the comparison was partial rather than silently masking it.
#[allow(clippy::too_many_arguments)]
pub async fn diff_responses(
    primary_status: u16,
    primary_headers: &[(String, String)],
    primary_body: &Bytes,
    primary_truncated: bool,
    shadow_status: u16,
    shadow_headers: &[(String, String)],
    shadow_body: &Bytes,
    shadow_truncated: bool,
    ctx: &RequestContext,
    _max_body_bytes: usize,
) -> Vec<Divergence> {
    let mut out = Vec::new();

    // 1. Status code — exact match.
    if primary_status != shadow_status {
        out.push(Divergence::new(
            Field::Status,
            primary_status.to_string(),
            shadow_status.to_string(),
            ctx,
        ));
    }

    // 2. Truncation divergence (partial comparison). Recorded before the body
    //    hash check so the operator sees *why* a body comparison is absent.
    if primary_truncated || shadow_truncated {
        let mut d = Divergence::new(Field::Body, "full body", "truncated at cap", ctx);
        d.truncated = true;
        out.push(d);
    }

    // 3. Headers — normalized comparison (timestamp format-only + path/port
    //    normalization). Header *order* is ignored because shadow responses
    //    come from an independent backend whose header ordering is not
    //    expected to match; duplicate count and normalized values still must
    //    match for each header name.
    let hdr_mismatches = normalize_header_values(primary_headers, shadow_headers);
    if !hdr_mismatches.is_empty() {
        // Summarize the mismatched header names rather than one divergence per
        // header, to keep shadow logs readable.
        let names: Vec<String> = hdr_mismatches.iter().map(|(k, _)| k.clone()).collect();
        out.push(Divergence::new(
            Field::Headers,
            "matching normalized headers",
            names.join(", "),
            ctx,
        ));
    }

    // 4. Body — normalized SHA-256 hash, only when neither side was truncated
    //    (a truncated body can't be meaningfully hashed against a full one).
    if !primary_truncated && !shadow_truncated {
        let exp_hash = sha256_hex(&normalize_body_bytes(primary_body));
        let act_hash = sha256_hex(&normalize_body_bytes(shadow_body));
        if exp_hash != act_hash {
            out.push(Divergence::new(Field::Body, exp_hash, act_hash, ctx));
        }
    }

    out
}

// ===== normalizers (ports of harness/diff_engine.py) =====

/// RFC 1123 timestamp, e.g. `Tue, 09 Jun 2026 03:20:35 GMT`.
fn is_timestamp(value: &str) -> bool {
    let b = value.as_bytes();
    // Fixed-width 29-byte format. Validate delimiters and digit runs at known
    // offsets rather than pulling a regex dependency.
    value.len() == 29
        && b[3] == b','
        && b[4] == b' '
        && b[7] == b' '
        && b[11] == b' '
        && b[16] == b' '
        && b[25] == b' '
        && b[19] == b':'
        && b[22] == b':'
        && &b[26..29] == b"GMT"
        && b[5..7].iter().all(|c| c.is_ascii_digit())      // day
        && b[12..16].iter().all(|c| c.is_ascii_digit())    // year
        && b[17..19].iter().all(|c| c.is_ascii_digit())    // hh
        && b[20..22].iter().all(|c| c.is_ascii_digit())    // mm
        && b[23..25].iter().all(|c| c.is_ascii_digit())    // ss
        && b[0].is_ascii_uppercase()                       // day-of-week
        && b[1..3].iter().all(|c| c.is_ascii_lowercase())
        && b[8].is_ascii_uppercase()                       // month
        && b[9].is_ascii_lowercase()
        && b[10].is_ascii_lowercase()
}

const TIMESTAMP_HEADERS: &[&str] = &["date", "last-modified"];

type HeaderMismatch = (String, (Option<Vec<String>>, Option<Vec<String>>));

fn normalize_header_values(
    expected: &[(String, String)],
    actual: &[(String, String)],
) -> Vec<HeaderMismatch> {
    let mut exp = grouped_normalized_headers(expected);
    let act = grouped_normalized_headers(actual);

    let mut mismatches = Vec::new();
    let keys: std::collections::BTreeSet<String> = exp.keys().chain(act.keys()).cloned().collect();
    for key in keys {
        let e = exp.remove(&key);
        let a = act.get(&key).cloned();
        let (Some(e), Some(a)) = (&e, &a) else {
            mismatches.push((key.clone(), (e.clone(), a.clone())));
            continue;
        };
        if e != a {
            mismatches.push((key.clone(), (Some(e.clone()), Some(a.clone()))));
        }
    }
    mismatches
}

fn grouped_normalized_headers(headers: &[(String, String)]) -> BTreeMap<String, Vec<String>> {
    let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, value) in headers {
        let key = name.to_ascii_lowercase();
        grouped
            .entry(key.clone())
            .or_default()
            .push(normalize_header_value(&key, value));
    }
    for values in grouped.values_mut() {
        values.sort();
    }
    grouped
}

fn normalize_header_value(name: &str, value: &str) -> String {
    if TIMESTAMP_HEADERS.contains(&name) && is_timestamp(value) {
        "<timestamp>".to_string()
    } else {
        normalize_value(value)
    }
}

/// Compose every header-value normalizer into a single pipeline (same order
/// as [`normalize_body_bytes`]). Comparing the fully-normalized values —
/// rather than OR-ing independently-normalized ones — avoids false divergences
/// where one normalizer erases a difference that another would still see.
fn normalize_value(value: &str) -> String {
    normalize_pwd(&normalize_cgi_output(&normalize_paths(value)))
}

/// Replace temp-directory paths with a canonical placeholder.
fn normalize_paths(value: &str) -> String {
    // /tmp/thttpd_golden_XXX, /tmp/thttpd_diff_XXX, /tmp/pytest-XXX
    let mut out = String::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if value[i..].starts_with("/tmp/thttpd_golden_")
            || value[i..].starts_with("/tmp/thttpd_diff_")
        {
            out.push_str("/tmp/thttpd_NORMALIZED");
            i += "/tmp/thttpd_golden_".len();
            // consume the trailing identifier run
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
        } else if value[i..].starts_with("/tmp/pytest-") {
            out.push_str("/tmp/pytest_NORMALIZED");
            i += "/tmp/pytest-".len();
            while i < bytes.len() && !(bytes[i] == b'/' || bytes[i].is_ascii_whitespace()) {
                i += 1;
            }
        } else {
            // Advance one full UTF-8 character so `i` stays on a char
            // boundary; slicing `value[i..]` after a partial multibyte
            // increment (e.g. inside a CJK character) would otherwise panic.
            let ch = value[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// Normalize dynamic SERVER_PORT / HTTP_HOST / Host values in CGI output.
fn normalize_cgi_output(value: &str) -> String {
    let value = regex_replace(value, "SERVER_PORT=\\d+", "SERVER_PORT=PORT");
    let value = regex_replace(
        &value,
        "HTTP_HOST=127\\.0\\.0\\.1:\\d+",
        "HTTP_HOST=127.0.0.1:PORT",
    );
    let value = regex_replace(&value, "Host: 127\\.0\\.0\\.1:\\d+", "Host: 127.0.0.1:PORT");
    regex_replace(&value, "Host=127\\.0\\.0\\.1:\\d+", "Host=127.0.0.1:PORT")
}

/// Normalize PWD=... differences (C chdirs into cgi-bin, Rust doesn't).
fn normalize_pwd(value: &str) -> String {
    regex_replace(value, "PWD=/[^\n]*", "PWD=NORMALIZED")
}

/// Minimal literal-prefix regex replacement for the fixed patterns above.
fn regex_replace(haystack: &str, pattern: &str, replacement: &str) -> String {
    // The patterns used here are simple: a literal prefix (which may contain
    // regex metachars) followed by `\d+`, `.*`, `[^\n]*`, or a char class.
    // Rather than pull a regex crate, implement the small set we need by
    // matching the literal head then a greedy tail class.
    // Split pattern into head (up to first regex metachar) + rest.
    // We only ever call this with patterns ending in one of: `\d+`, `.*`, `[^\n]*`.
    let (head, tail_class): (&str, &str) = if let Some(idx) = pattern.find("\\d+") {
        (&pattern[..idx], "digit")
    } else if let Some(idx) = pattern.find(".*") {
        (&pattern[..idx], "any")
    } else if let Some(idx) = pattern.find("[^\\n]*") {
        (&pattern[..idx], "nonl")
    } else {
        // No dynamic tail: treat the whole pattern as literal.
        return haystack.replace(pattern, replacement);
    };

    let head = unescape_regex_literal(head);
    let mut out = String::with_capacity(haystack.len());
    let mut i = 0;
    let hb = head.as_bytes();
    while i < haystack.len() {
        if haystack[i..].starts_with(&head) {
            // consume head + tail class
            let mut j = i + hb.len();
            match tail_class {
                "digit" => {
                    while j < haystack.len() && haystack.as_bytes()[j].is_ascii_digit() {
                        j += 1;
                    }
                }
                "any" => {
                    while j < haystack.len() {
                        j += 1;
                    }
                }
                "nonl" => {
                    while j < haystack.len() && haystack.as_bytes()[j] != b'\n' {
                        j += 1;
                    }
                }
                _ => {}
            }
            // For a `\d+` (one-or-more) tail, a zero-length match is NOT a
            // real match — emit one literal char and continue so we don't
            // wrongly rewrite an already-normalized value (e.g. "SERVER_PORT=").
            if tail_class == "digit" && j == i + hb.len() {
                let ch = haystack[i..].chars().next().unwrap();
                out.push(ch);
                i += ch.len_utf8();
                continue;
            }
            out.push_str(replacement);
            i = j;
        } else {
            // Advance one full UTF-8 char so `i` stays on a char boundary;
            // byte-at-a-time slicing panics on multibyte content.
            let ch = haystack[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

fn unescape_regex_literal(s: &str) -> String {
    s.replace("\\.", ".")
}

/// Apply all body normalizers, preserving distinct byte values losslessly.
///
/// Bodies are mapped through a reversible Latin-1 transform (`byte -> char`)
/// so the ASCII-oriented normalizers (path/CGI/PWD) can run over a valid
/// UTF-8 string while NEVER collapsing distinct invalid byte sequences onto
/// the same replacement character the way [`String::from_utf8_lossy`] does.
/// Two different binary files therefore keep distinct hashes and stay visible
/// as shadow divergences.
fn normalize_body_bytes(body: &Bytes) -> Bytes {
    let text = bytes_to_latin1(body);
    let normalized = normalize_pwd(&normalize_cgi_output(&normalize_paths(&text)));
    latin1_to_bytes(&normalized)
}

/// Map every byte to its `char` (0x00..=0xff → U+0000..=U+00FF). All code
/// points in that range are valid, so the result is always well-formed UTF-8
/// and the existing char-aware normalizers process it safely.
fn bytes_to_latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// Inverse of [`bytes_to_latin1`]. The normalizers only emit ASCII
/// replacements plus the original Latin-1 chars, so every char stays `<= 0xff`;
/// the high-byte fallback guards a theoretical bug rather than panicking.
fn latin1_to_bytes(s: &str) -> Bytes {
    let mut out = Vec::with_capacity(s.len());
    for c in s.chars() {
        let cp = c as u32;
        debug_assert!(
            cp <= 0xff,
            "normalizer produced a non-Latin-1 char: U+{cp:04X}"
        );
        out.push(if cp <= 0xff {
            cp as u8
        } else {
            // Defensive fallback: take the low byte to stay byte-aligned.
            cp as u8
        });
    }
    Bytes::from(out)
}

fn sha256_hex(data: &Bytes) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex_encode(&hasher.finalize())
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> RequestContext {
        RequestContext {
            path: "/x".into(),
            method: "GET".into(),
            request_id: "r1".into(),
        }
    }

    fn hdrs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[tokio::test]
    async fn timestamp_headers_match_format_only() {
        let ctx = ctx();
        let d = diff_responses(
            200,
            &hdrs(&[("date", "Tue, 09 Jun 2026 03:20:35 GMT")]),
            &Bytes::new(),
            false,
            200,
            &hdrs(&[("date", "Wed, 10 Jun 2026 04:00:00 GMT")]),
            &Bytes::new(),
            false,
            &ctx,
            1024,
        )
        .await;
        assert!(
            d.is_empty(),
            "format-matching timestamps must not diverge: {d:?}"
        );
    }

    #[tokio::test]
    async fn status_mismatch_caught() {
        let ctx = ctx();
        let d = diff_responses(
            200,
            &[],
            &Bytes::new(),
            false,
            500,
            &[],
            &Bytes::new(),
            false,
            &ctx,
            1024,
        )
        .await;
        assert!(d.iter().any(|x| x.field == Field::Status));
    }

    #[tokio::test]
    async fn headers_mismatch_caught() {
        let ctx = ctx();
        let d = diff_responses(
            200,
            &hdrs(&[("content-type", "text/html")]),
            &Bytes::new(),
            false,
            200,
            &hdrs(&[("content-type", "text/plain")]),
            &Bytes::new(),
            false,
            &ctx,
            1024,
        )
        .await;
        assert!(d.iter().any(|x| x.field == Field::Headers));
    }

    #[tokio::test]
    async fn duplicate_header_mismatch_caught_even_when_last_value_matches() {
        // Regression: collecting headers into BTreeMap<String, String>
        // collapsed duplicates, so only the last set-cookie survived. These
        // responses both end in "theme=light", but the earlier cookie differs.
        let ctx = ctx();
        let d = diff_responses(
            200,
            &hdrs(&[
                ("set-cookie", "session=abc; Path=/"),
                ("set-cookie", "theme=light; Path=/"),
            ]),
            &Bytes::new(),
            false,
            200,
            &hdrs(&[
                ("set-cookie", "session=def; Path=/"),
                ("set-cookie", "theme=light; Path=/"),
            ]),
            &Bytes::new(),
            false,
            &ctx,
            1024,
        )
        .await;
        assert!(
            d.iter().any(|x| x.field == Field::Headers),
            "duplicate set-cookie content/count must diverge: {d:?}"
        );
    }

    #[tokio::test]
    async fn repeated_timestamp_headers_match_format_only() {
        let ctx = ctx();
        let d = diff_responses(
            200,
            &hdrs(&[
                ("date", "Tue, 09 Jun 2026 03:20:35 GMT"),
                ("date", "Wed, 10 Jun 2026 04:00:00 GMT"),
            ]),
            &Bytes::new(),
            false,
            200,
            &hdrs(&[
                ("date", "Thu, 11 Jun 2026 05:00:00 GMT"),
                ("date", "Fri, 12 Jun 2026 06:00:00 GMT"),
            ]),
            &Bytes::new(),
            false,
            &ctx,
            1024,
        )
        .await;
        assert!(
            d.is_empty(),
            "repeated timestamp values should normalize format-only: {d:?}"
        );
    }

    #[tokio::test]
    async fn identical_duplicate_headers_ignore_order() {
        let ctx = ctx();
        let d = diff_responses(
            200,
            &hdrs(&[
                ("set-cookie", "session=abc; Path=/"),
                ("set-cookie", "theme=light; Path=/"),
            ]),
            &Bytes::new(),
            false,
            200,
            &hdrs(&[
                ("set-cookie", "theme=light; Path=/"),
                ("set-cookie", "session=abc; Path=/"),
            ]),
            &Bytes::new(),
            false,
            &ctx,
            1024,
        )
        .await;
        assert!(
            d.is_empty(),
            "identical duplicate values should not diverge solely due to order: {d:?}"
        );
    }

    #[tokio::test]
    async fn body_mismatch_caught() {
        let ctx = ctx();
        let d = diff_responses(
            200,
            &[],
            &Bytes::from_static(b"hello"),
            false,
            200,
            &[],
            &Bytes::from_static(b"world"),
            false,
            &ctx,
            1024,
        )
        .await;
        assert!(d.iter().any(|x| x.field == Field::Body && !x.truncated));
    }

    #[tokio::test]
    async fn normalizers_composed_no_false_divergence() {
        // Regression (Claim 9): header values that differ ONLY in a dynamic CGI
        // port must match once all normalizers are composed. The old `||`
        // composition saw them differ under normalize_paths and reported a
        // false divergence.
        let ctx = ctx();
        let d = diff_responses(
            200,
            &hdrs(&[("x-cgi", "SERVER_PORT=8081")]),
            &Bytes::new(),
            false,
            200,
            &hdrs(&[("x-cgi", "SERVER_PORT=8082")]),
            &Bytes::new(),
            false,
            &ctx,
            1024,
        )
        .await;
        assert!(
            d.is_empty(),
            "CGI-port-only difference must normalize away: {d:?}"
        );
    }

    #[tokio::test]
    async fn normalize_paths_is_utf8_safe() {
        // Regression (Claim 6): normalizers used to increment one byte at a time
        // and then slice `value[i..]`, panicking when `i` landed inside a
        // multibyte character. Feed multibyte content and assert no panic.
        let multibyte = "café — /tmp/thttpd_golden_abc/日本語.txt";
        let n1 = normalize_value(multibyte);
        let n2 = normalize_value(multibyte);
        assert_eq!(n1, n2, "identical multibyte values must normalize equal");
        // Also exercise regex_replace (cgi/pwd) paths with multibyte content.
        let _ = normalize_value("PWD=/var/日本語 SERVER_PORT=8081");
    }

    #[tokio::test]
    async fn identical_responses_no_divergence() {
        let ctx = ctx();
        let d = diff_responses(
            200,
            &hdrs(&[("content-type", "text/plain")]),
            &Bytes::from_static(b"same"),
            false,
            200,
            &hdrs(&[("content-type", "text/plain")]),
            &Bytes::from_static(b"same"),
            false,
            &ctx,
            1024,
        )
        .await;
        assert!(d.is_empty(), "{d:?}");
    }

    #[tokio::test]
    async fn temp_path_substitution() {
        let ctx = ctx();
        let d = diff_responses(
            200,
            &[],
            &Bytes::from("/tmp/pytest-abc123/index.html"),
            false,
            200,
            &[],
            &Bytes::from("/tmp/pytest_NORMALIZED/index.html"),
            false,
            &ctx,
            1024,
        )
        .await;
        assert!(d.is_empty(), "normalized temp paths must match: {d:?}");
    }

    #[tokio::test]
    async fn cgi_port_substitution() {
        let ctx = ctx();
        let d = diff_responses(
            200,
            &[],
            &Bytes::from("SERVER_PORT=8081\nHTTP_HOST=127.0.0.1:8081"),
            false,
            200,
            &[],
            &Bytes::from("SERVER_PORT=PORT\nHTTP_HOST=127.0.0.1:PORT"),
            false,
            &ctx,
            1024,
        )
        .await;
        assert!(d.is_empty(), "normalized CGI ports must match: {d:?}");
    }

    #[tokio::test]
    async fn truncation_records_divergence() {
        let ctx = ctx();
        let d = diff_responses(
            200,
            &[],
            &Bytes::from_static(b"partial"),
            true, // truncated
            200,
            &[],
            &Bytes::from_static(b"partial"),
            false,
            &ctx,
            1024,
        )
        .await;
        assert!(
            d.iter().any(|x| x.field == Field::Body && x.truncated),
            "truncation must be recorded: {d:?}"
        );
    }

    #[test]
    fn timestamp_recognition() {
        assert!(is_timestamp("Tue, 09 Jun 2026 03:20:35 GMT"));
        assert!(!is_timestamp("not a date"));
        assert!(!is_timestamp("Tue, 9 Jun 2026 03:20:35 GMT")); // day must be 2 digits
    }

    #[tokio::test]
    async fn distinct_invalid_byte_sequences_stay_distinct() {
        // P2 (Compare binary response bodies losslessly): from_utf8_lossy maps
        // 0x80 and 0x81 to the SAME replacement char, so two different binary
        // bodies used to hash identically and escape divergence detection. The
        // Latin-1 transform keeps them distinct.
        let a = normalize_body_bytes(&Bytes::from_static(&[0x80]));
        let b = normalize_body_bytes(&Bytes::from_static(&[0x81]));
        assert_ne!(
            a, b,
            "distinct bytes must stay distinct after normalization"
        );
        assert_eq!(a.as_ref(), &[0x80]);
        assert_eq!(b.as_ref(), &[0x81]);

        // And surfaced as an actual shadow body divergence.
        let ctx = ctx();
        let d = diff_responses(
            200,
            &[],
            &Bytes::from_static(&[0x80]),
            false,
            200,
            &[],
            &Bytes::from_static(&[0x81]),
            false,
            &ctx,
            1024,
        )
        .await;
        assert!(
            d.iter().any(|x| x.field == Field::Body && !x.truncated),
            "distinct binary bodies must diverge: {d:?}"
        );
    }

    #[test]
    fn latin1_roundtrip_preserves_bytes() {
        // Every byte value round-trips through the Latin-1 transform.
        let all: Vec<u8> = (0u8..=255).collect();
        let rt = latin1_to_bytes(&bytes_to_latin1(&all));
        assert_eq!(rt.as_ref(), &all[..]);
    }
}
