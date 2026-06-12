//! Response building for thttpd.
//! Translates response construction from `legacy/src/libhttpd.c`.
//! Header order is critical for behavioral parity — uses `Vec<(String, String)>`, NOT HashMap.

use crate::conn::HttpConn;

/// Server version string matching C's EXPOSED_SERVER_SOFTWARE.
pub const SERVER_SOFTWARE: &str = "sthttpd/2.27.0 03oct2014";

/// Server address for error page footer links.
pub const SERVER_ADDRESS: &str = "http://localhost";

/// HTTP response builder.
pub struct ResponseBuilder {
    status_code: u16,
    status_text: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl ResponseBuilder {
    pub fn new() -> Self {
        Self {
            status_code: 200,
            status_text: "OK".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    pub fn status(mut self, code: u16, text: &str) -> Self {
        self.status_code = code;
        self.status_text = text.to_string();
        self
    }

    /// Add a response header. Order is preserved.
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    /// Set the response body.
    pub fn body(mut self, body: Vec<u8>) -> Self {
        self.body = body;
        self
    }

    /// Build the complete response as bytes.
    pub fn build(self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(format!("HTTP/1.0 {} {}\r\n", self.status_code, self.status_text).as_bytes());
        for (name, value) in &self.headers {
            out.extend_from_slice(format!("{}: {}\r\n", name, value).as_bytes());
        }
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(&self.body);
        out
    }
}

impl Default for ResponseBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Build a complete HTTP response matching C's `send_mime()` format.
///
/// Emits the standard 7-header block in C order:
/// Server, Content-Type, Date, Last-Modified, Accept-Ranges, Connection, Content-Length
///
/// For non-2xx/3xx status codes, appends `Cache-Control: no-cache,no-store`.
/// When `http.mime_flag` is false (HTTP/0.9), returns empty Vec.
pub fn build_full_response(
    http: &HttpConn,
    status: u16,
    status_text: &str,
    content_type: &str,
    length: i64,
    mtime: i64,
    extra_headers: &[(String, String)],
) -> Vec<u8> {
    // HTTP/0.9 raw mode — caller sends body-only bytes directly
    if !http.mime_flag {
        return Vec::new();
    }

    let now_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let mod_time = if mtime == 0 { now_ts } else { mtime };

    let now_str = thttpd_tdate::format_http_date(now_ts);
    let mod_str = thttpd_tdate::format_http_date(mod_time);

    // Apply charset to text/* types
    let fixed_type = if content_type.starts_with("text/") && !content_type.contains("charset=") {
        format!("{}; charset=iso-8859-1", content_type)
    } else {
        content_type.to_string()
    };

    let mut out = Vec::new();

    // Check for range upgrade BEFORE writing status line
    let (final_status, final_status_text, partial_content) =
        if http.got_range && status == 200
            && http.last_byte_index >= http.first_byte_index
            && (http.last_byte_index != length - 1 || http.first_byte_index != 0)
            && (http.range_if.is_none() || http.range_if == Some(mod_time))
        {
            (206, "Partial Content", true)
        } else {
            (status, status_text, false)
        };

    // Status line — use the request's protocol version (C uses hc->protocol at
    // libhttpd.c:638). Fall back to HTTP/1.0 for HTTP/0.9 (no version token).
    let protocol = if http.http_version.is_empty() {
        "HTTP/1.0".to_string()
    } else {
        http.http_version.clone()
    };
    out.extend_from_slice(format!("{} {} {}\r\n", protocol, final_status, final_status_text).as_bytes());

    // Standard headers in C order
    out.extend_from_slice(format!("Server: {}\r\n", SERVER_SOFTWARE).as_bytes());
    out.extend_from_slice(format!("Content-Type: {}\r\n", fixed_type).as_bytes());
    out.extend_from_slice(format!("Date: {}\r\n", now_str).as_bytes());
    out.extend_from_slice(format!("Last-Modified: {}\r\n", mod_str).as_bytes());
    out.extend_from_slice(b"Accept-Ranges: bytes\r\n");
    out.extend_from_slice(b"Connection: close\r\n");

    // Cache-Control for non-2xx/3xx
    let s100 = final_status / 100;
    if s100 != 2 && s100 != 3 {
        out.extend_from_slice(b"Cache-Control: no-cache,no-store\r\n");
    }

    // Content-Range + Content-Length for partial content, or just Content-Length
    if partial_content {
        let range_len = http.last_byte_index - http.first_byte_index + 1;
        out.extend_from_slice(
            format!("Content-Range: bytes {}-{}/{}\r\n",
                http.first_byte_index, http.last_byte_index, length).as_bytes()
        );
        out.extend_from_slice(format!("Content-Length: {}\r\n", range_len).as_bytes());
    } else if length >= 0 {
        out.extend_from_slice(format!("Content-Length: {}\r\n", length).as_bytes());
    }

    // Extra headers (P3P, Cache-Control max-age, etc.)
    for (name, value) in extra_headers {
        out.extend_from_slice(format!("{}: {}\r\n", name, value).as_bytes());
    }

    // Blank line
    out.extend_from_slice(b"\r\n");

    out
}

/// HTML-escape a string for use in error pages (matches C's `defang()`).
fn defang(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// Generate an HTML error page matching C's `send_response()` format exactly.
/// `form` is the error-specific message (may contain `%.80s` placeholder for `arg`).
/// `user_agent` triggers MSIE padding if it contains the substring "MSIE"
/// (matching C's `match("**MSIE**", hc->useragent)` at libhttpd.c:742-749).
pub fn error_page(status: u16, title: &str, form: &str, arg: &str, user_agent: Option<&str>) -> Vec<u8> {
    let defanged = defang(arg);

    let body_message = if form.contains("%.80s") {
        let truncated = if defanged.len() > 80 { &defanged[..80] } else { &defanged };
        form.replace("%.80s", truncated)
    } else {
        form.to_string()
    };

    // C's send_response() emits: <HTML>...<H2>form_msg[ + MSIE padding] <HR>...</HTML>
    // (libhttpd.c:738-751) — padding goes BETWEEN the body message and <HR>,
    // NOT after </HTML>. Order matters for byte-exact parity.
    let mut html = format!(
        "<HTML>\n<HEAD><TITLE>{} {}</TITLE></HEAD>\n<BODY BGCOLOR=\"#cc9999\" TEXT=\"#000000\" LINK=\"#2020ff\" VLINK=\"#4040cc\">\n<H2>{} {}</H2>\n{}",
        status, title, status, title, body_message
    );

    // MSIE padding — C appends this for clients identifying as MSIE
    // (libhttpd.c:742-749). 6 lines of "Padding so that MSIE deigns to show..."
    // between `<!--` and `-->`.
    if user_agent.map(|ua| ua.contains("MSIE")).unwrap_or(false) {
        html.push_str("<!--\n");
        for _ in 0..6 {
            html.push_str("Padding so that MSIE deigns to show this error instead of its own canned one.\n");
        }
        html.push_str("-->\n");
    }

    // Then the response tail: <HR>, <ADDRESS>, </BODY>, </HTML>
    html.push_str(&format!(
        "<HR>\n<ADDRESS><A HREF=\"{}\">{}</A></ADDRESS>\n</BODY>\n</HTML>\n",
        SERVER_ADDRESS, SERVER_SOFTWARE
    ));

    html.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_response_builder() {
        let resp = ResponseBuilder::new()
            .status(200, "OK")
            .header("Content-Type", "text/html")
            .header("Content-Length", "5")
            .body(b"hello".to_vec())
            .build();
        let s = String::from_utf8(resp).unwrap();
        assert!(s.starts_with("HTTP/1.0 200 OK\r\n"));
        assert!(s.contains("Content-Type: text/html\r\n"));
        assert!(s.contains("Content-Length: 5\r\n"));
    }

    #[test]
    fn test_header_order_preserved() {
        let resp = ResponseBuilder::new()
            .status(200, "OK")
            .header("Date", "now")
            .header("Server", "thttpd")
            .header("Content-Type", "text/html")
            .build();
        let s = String::from_utf8(resp).unwrap();
        let date_pos = s.find("Date:").unwrap();
        let server_pos = s.find("Server:").unwrap();
        let ct_pos = s.find("Content-Type:").unwrap();
        assert!(date_pos < server_pos);
        assert!(server_pos < ct_pos);
    }

    #[test]
    fn test_error_page_404() {
        let html = error_page(404, "Not Found", "The requested URL '%.80s' was not found on this server.\n", "/nonexistent.html", None);
        let s = String::from_utf8(html).unwrap();
        assert!(s.contains("<TITLE>404 Not Found</TITLE>"));
        assert!(s.contains("<H2>404 Not Found</H2>"));
        assert!(s.contains("was not found on this server"));
        assert!(s.contains("<HR>"));
        assert!(s.contains("<ADDRESS>"));
        assert!(s.contains(SERVER_SOFTWARE));
        // No MSIE padding when user_agent is None
        assert!(!s.contains("Padding so that MSIE"));
    }

    #[test]
    fn test_error_page_msie_padding() {
        // MSIE user agent gets the 6-line padding block
        let html = error_page(404, "Not Found", "not found\n", "x",
            Some("Mozilla/4.0 (compatible; MSIE 6.0; Windows NT 5.1)"));
        let s = String::from_utf8(html).unwrap();
        assert!(s.contains("<!--\n"));
        assert!(s.contains("-->\n"));
        let pad_count = s.matches("Padding so that MSIE deigns to show this error").count();
        assert_eq!(pad_count, 6, "MSIE padding must have exactly 6 lines (matches C's `for (n=0;n<6;n++)`)");
        // Padding text matches C byte-for-byte
        assert!(s.contains("Padding so that MSIE deigns to show this error instead of its own canned one.\n"));
    }

    #[test]
    fn test_error_page_no_msie_no_padding() {
        // Non-MSIE user agent does not get padding
        let html = error_page(404, "Not Found", "not found\n", "x",
            Some("Mozilla/5.0 (X11; Linux x86_64) Firefox/100.0"));
        let s = String::from_utf8(html).unwrap();
        assert!(!s.contains("Padding so that MSIE"));
        assert!(!s.contains("<!--"));
    }

    #[test]
    fn test_defang() {
        assert_eq!(defang("<script>"), "&lt;script&gt;");
        assert_eq!(defang("normal"), "normal");
    }

    #[test]
    fn test_build_full_response_headers() {
        let http = HttpConn::new();
        let resp = build_full_response(&http, 200, "OK", "text/html", 69, 1000000, &[]);
        let s = String::from_utf8(resp).unwrap();
        assert!(s.starts_with("HTTP/1.0 200 OK\r\n"));
        assert!(s.contains("Server: sthttpd/2.27.0 03oct2014\r\n"));
        assert!(s.contains("Content-Type: text/html; charset=iso-8859-1\r\n"));
        assert!(s.contains("Accept-Ranges: bytes\r\n"));
        assert!(s.contains("Connection: close\r\n"));
        assert!(s.contains("Content-Length: 69\r\n"));
        assert!(!s.contains("Cache-Control"));
    }

    #[test]
    fn test_build_full_response_error() {
        let http = HttpConn::new();
        let resp = build_full_response(&http, 404, "Not Found", "text/html", -1, 0, &[]);
        let s = String::from_utf8(resp).unwrap();
        assert!(s.contains("Cache-Control: no-cache,no-store\r\n"));
        assert!(!s.contains("Content-Length"));
    }

    #[test]
    fn test_build_full_response_0_9() {
        let mut http = HttpConn::new();
        http.mime_flag = false;
        let resp = build_full_response(&http, 200, "OK", "text/html", 13, 0, &[]);
        assert!(resp.is_empty());
    }
}
