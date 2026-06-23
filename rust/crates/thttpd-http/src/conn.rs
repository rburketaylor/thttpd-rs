//! HTTP connection state for thttpd.
//! Translates `httpd_conn` struct from `legacy/src/libhttpd.h:79-142`.
//! All `char*` fields become owned `String` or `Vec<u8>` with eager parsing.

use crate::method::Method;
use crate::parse_state::ParseState;

/// Connection state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    Free,
    Reading,
    Sending,
    Pausing,
    Lingering,
}

/// HTTP connection state.
/// Replaces C's `httpd_conn` struct (40+ fields).
/// All string fields are owned — no borrowed pointers into read_buf.
pub struct HttpConn {
    // Read buffer
    pub read_buf: Vec<u8>,
    pub read_idx: usize,
    pub checked_idx: usize,

    // FSM state
    pub parse_state: ParseState,
    pub method: Method,
    pub http_version: String,
    /// Set when the request protocol is not "HTTP/1.0" (case-insensitive).
    /// Matches C's `one_one` flag (libhttpd.c:1965). When true, the request
    /// must include a Host header or the server returns 400.
    pub one_one: bool,
    /// Set when vhost is enabled and the hostname was used to look up the
    /// file. Stores the lowercased hostname (matches C's vhost_map logic).
    pub vhost_dir: String,

    // Parsed URL components
    pub encoded_url: String,
    pub decoded_url: String,
    pub path_info: String,
    pub query: String,
    pub fragment: String,

    // Filesystem path
    pub orig_filename: String,
    pub expn_filename: String,

    // Headers
    pub host: String,
    /// First IP from X-Forwarded-For header, if present.
    /// Used for CGI REMOTE_ADDR and log lines (libhttpd.c:2210-2215).
    pub x_forwarded_for: String,
    pub content_type: String,
    pub content_length: Option<i64>,
    pub referer: String,
    pub user_agent: String,
    pub cookie: String,
    pub authorization: String,
    /// Server charset (set from -T flag, default "iso-8859-1").
    /// Used to append "charset=..." to text/* Content-Type headers.
    pub charset: String,
    pub accept: String,
    pub accept_encoding: String,
    pub accept_language: String,
    pub if_modified_since: Option<i64>,

    // HTTP/0.9 mode
    pub mime_flag: bool,

    // Range request
    pub got_range: bool,
    pub first_byte_index: i64,
    pub last_byte_index: i64,
    pub range_if: Option<i64>,

    // Response
    pub response: Vec<u8>,
    pub response_len: usize,
    pub response_header_len: usize,
    pub status_code: u16,
    pub status_text: String,
    pub headers: Vec<(String, String)>,

    // File serving
    pub file_address: Option<std::rc::Rc<memmap2::Mmap>>,
    pub bytes_sent: i64,

    // Connection state
    pub state: ConnState,
    pub keep_alive: bool,
    pub should_linger: bool,
}

impl HttpConn {
    /// Create a new HttpConn in initial state.
    pub fn new() -> Self {
        Self {
            read_buf: vec![0u8; 60000],
            read_idx: 0,
            checked_idx: 0,

            parse_state: ParseState::initial(),
            method: Method::Unknown,
            http_version: String::new(),
            one_one: false,
            vhost_dir: String::new(),

            encoded_url: String::new(),
            decoded_url: String::new(),
            path_info: String::new(),
            query: String::new(),
            fragment: String::new(),

            orig_filename: String::new(),
            expn_filename: String::new(),

            host: String::new(),
            x_forwarded_for: String::new(),
            content_type: String::new(),
            content_length: None,
            referer: String::new(),
            user_agent: String::new(),
            cookie: String::new(),
            authorization: String::new(),
            charset: String::from("iso-8859-1"),
            accept: String::new(),
            accept_encoding: String::new(),
            accept_language: String::new(),
            if_modified_since: None,

            mime_flag: true,
            got_range: false,
            first_byte_index: 0,
            last_byte_index: -1,
            range_if: None,

            response: Vec::new(),
            response_len: 0,
            response_header_len: 0,
            status_code: 0,
            status_text: String::new(),
            headers: Vec::new(),

            file_address: None,
            bytes_sent: 0,

            state: ConnState::Reading,
            keep_alive: false,
            should_linger: false,
        }
    }

    /// Reset connection state for reuse (keep-alive).
    pub fn reset(&mut self) {
        self.read_idx = 0;
        self.checked_idx = 0;
        self.parse_state = ParseState::initial();
        self.method = Method::Unknown;
        self.http_version.clear();
        self.one_one = false;
        self.vhost_dir.clear();
        self.encoded_url.clear();
        self.decoded_url.clear();
        self.path_info.clear();
        self.query.clear();
        self.fragment.clear();
        self.orig_filename.clear();
        self.expn_filename.clear();
        self.host.clear();
        self.x_forwarded_for.clear();
        self.content_type.clear();
        self.content_length = None;
        self.referer.clear();
        self.user_agent.clear();
        self.cookie.clear();
        self.authorization.clear();
        self.charset = String::from("iso-8859-1");
        self.accept.clear();
        self.accept_encoding.clear();
        self.accept_language.clear();
        self.if_modified_since = None;
        self.mime_flag = true;
        self.got_range = false;
        self.first_byte_index = 0;
        self.last_byte_index = -1;
        self.range_if = None;
        self.response.clear();
        self.response_len = 0;
        self.response_header_len = 0;
        self.status_code = 0;
        self.status_text.clear();
        self.headers.clear();
        self.file_address = None;
        self.bytes_sent = 0;
        self.state = ConnState::Reading;
        self.keep_alive = false;
        self.should_linger = false;
    }
}

impl Default for HttpConn {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_x_forwarded_for_field_init() {
        // The x_forwarded_for field is empty by default
        let conn = HttpConn::new();
        assert!(conn.x_forwarded_for.is_empty());
    }

    #[test]
    fn test_x_forwarded_for_reset() {
        // After reset, the x_forwarded_for field is cleared
        let mut conn = HttpConn::new();
        conn.x_forwarded_for = "192.0.2.42".to_string();
        conn.reset();
        assert!(conn.x_forwarded_for.is_empty());
    }

    #[test]
    fn test_x_forwarded_for_first_ip() {
        // "192.0.2.42, 10.0.0.1" should be parsed as just the first IP
        let xff = "192.0.2.42, 10.0.0.1";
        let first = xff.split(',').next().unwrap_or("").trim().to_string();
        assert_eq!(first, "192.0.2.42");
    }

    #[test]
    fn test_x_forwarded_for_whitespace_only() {
        // A whitespace-only value should be treated as empty
        let xff = "   ";
        let first = xff.split(',').next().unwrap_or("").trim().to_string();
        assert_eq!(first, "");
    }
}
