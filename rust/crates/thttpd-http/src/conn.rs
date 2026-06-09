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
    pub content_type: String,
    pub content_length: Option<i64>,
    pub referer: String,
    pub user_agent: String,
    pub cookie: String,
    pub authorization: String,
    pub accept: String,
    pub accept_encoding: String,
    pub if_modified_since: Option<i64>,

    // Response
    pub response: Vec<u8>,
    pub response_len: usize,
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

            encoded_url: String::new(),
            decoded_url: String::new(),
            path_info: String::new(),
            query: String::new(),
            fragment: String::new(),

            orig_filename: String::new(),
            expn_filename: String::new(),

            host: String::new(),
            content_type: String::new(),
            content_length: None,
            referer: String::new(),
            user_agent: String::new(),
            cookie: String::new(),
            authorization: String::new(),
            accept: String::new(),
            accept_encoding: String::new(),
            if_modified_since: None,

            response: Vec::new(),
            response_len: 0,
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
        self.encoded_url.clear();
        self.decoded_url.clear();
        self.path_info.clear();
        self.query.clear();
        self.fragment.clear();
        self.orig_filename.clear();
        self.expn_filename.clear();
        self.host.clear();
        self.content_type.clear();
        self.content_length = None;
        self.referer.clear();
        self.user_agent.clear();
        self.cookie.clear();
        self.authorization.clear();
        self.accept.clear();
        self.accept_encoding.clear();
        self.if_modified_since = None;
        self.response.clear();
        self.response_len = 0;
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
