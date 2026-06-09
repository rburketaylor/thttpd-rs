//! Response building for thttpd.
//! Translates response construction from `legacy/src/libhttpd.c`.
//! Header order is critical for behavioral parity — uses `Vec<(String, String)>`, NOT HashMap.

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
        // Status line
        out.extend_from_slice(format!("HTTP/1.0 {} {}\r\n", self.status_code, self.status_text).as_bytes());
        // Headers in order
        for (name, value) in &self.headers {
            out.extend_from_slice(format!("{}: {}\r\n", name, value).as_bytes());
        }
        out.extend_from_slice(b"\r\n");
        // Body
        out.extend_from_slice(&self.body);
        out
    }
}

impl Default for ResponseBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Generate an HTML error page matching C's format.
pub fn error_page(title: &str, extra: Option<&str>) -> Vec<u8> {
    let extra_html = extra.map(|e| format!("<P>{e}</P>")).unwrap_or_default();
    format!(
        "<HTML><HEAD><TITLE>{}</TITLE></HEAD>\n<BODY BGCOLOR=\"#cc9999\" TEXT=\"#000000\" LINK=\"#2020ff\" VLINK=\"#4040cc\">\n<H2>{}</H2>\n{}\n</BODY></HTML>\n",
        title, title, extra_html
    ).into_bytes()
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
    fn test_error_page() {
        let html = error_page("Not Found", None);
        let s = String::from_utf8(html).unwrap();
        assert!(s.contains("<TITLE>Not Found</TITLE>"));
    }
}
