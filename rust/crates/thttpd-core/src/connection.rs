//! Connection management for thttpd.
//! Translates connection handling from `legacy/src/thttpd.c`.
//! Uses `slab::Slab` for connection table management.

use mio::net::TcpStream;
use std::net::SocketAddr;
use std::time::Instant;
use thttpd_http::HttpConn;
use thttpd_http::conn::ConnState;

/// Sentinel meaning "no throttle limit" — mirrors `THROTTLE_NOLIMIT`.
pub const NOLIMIT: i64 = -1;

/// Per-connection throttle bookkeeping, mirroring C's `connecttab` fields
/// (`tnums`, `numtnums`, `max_limit`, `min_limit`, `started_at`,
/// `active_at`, `wouldblock_delay`). A connection with `tnums.is_empty()`
/// is unthrottled.
#[derive(Debug, Clone)]
pub struct ConnThrottle {
    /// Matched throttle-table indexes (bounded by MAX_THROTTLE_NUMS).
    pub tnums: Vec<usize>,
    /// Effective fair-share max for this connection (bytes/sec; NOLIMIT=∞).
    pub max_limit: i64,
    /// Effective min for this connection (bytes/sec; NOLIMIT=none).
    pub min_limit: i64,
    /// Byte offset where the body begins in `http.response` (past headers).
    /// Throttle accounting applies only to body bytes.
    pub header_len: usize,
    /// Body bytes sent so far (drives the rate check).
    pub body_bytes: i64,
    /// When sending started, for elapsed-time rate math.
    pub started_at: Option<Instant>,
    /// Last time we made forward progress (idle-timeout parity hook).
    pub active_at: Option<Instant>,
    /// When a paused (throttled) connection should resume writing.
    pub pause_until: Option<Instant>,
    /// True once `transition_to_sending`'s throttle admission check has
    /// already been performed upstream (e.g. by [`serve_static`]), so the
    /// downstream check is skipped.
    pub checked: bool,
}

impl ConnThrottle {
    pub fn new() -> Self {
        Self {
            tnums: Vec::new(),
            max_limit: NOLIMIT,
            min_limit: NOLIMIT,
            header_len: 0,
            body_bytes: 0,
            started_at: None,
            active_at: None,
            pause_until: None,
            checked: false,
        }
    }

    /// True when this connection matched at least one throttle rule.
    pub fn is_throttled(&self) -> bool {
        !self.tnums.is_empty()
    }

    pub fn reset(&mut self) {
        self.tnums.clear();
        self.max_limit = NOLIMIT;
        self.min_limit = NOLIMIT;
        self.header_len = 0;
        self.body_bytes = 0;
        self.started_at = None;
        self.active_at = None;
        self.pause_until = None;
        self.checked = false;
    }
}

impl Default for ConnThrottle {
    fn default() -> Self {
        Self::new()
    }
}

/// A connection slot in the connection table.
pub struct ConnSlot {
    pub state: ConnState,
    pub stream: Option<TcpStream>,
    pub http: HttpConn,
    /// Remote address for logging / CGI REMOTE_ADDR.
    pub peer_addr: Option<SocketAddr>,
    /// Per-connection throttle + timing state.
    pub throttle: ConnThrottle,
    /// True once a CGI program is actually dispatched (past the limit/not-found/
    /// throttle-reject guards). Used to avoid re-admitting the CGI response in
    /// `transition_to_sending`: CGI output is already charged a flat
    /// `CGI_BYTECOUNT` on completion, so it must not be rate-limited a second
    /// time as if it were ordinary file bytes.
    pub is_cgi: bool,
    /// True when a CGI POST dispatch has been deferred because the request body
    /// (Content-Length bytes) has not finished arriving yet. While set, the
    /// connection stays in `Reading` and `handle_read` accumulates body bytes
    /// until the full body is buffered, then re-dispatches the CGI.
    pub pending_cgi_body: bool,
}

impl ConnSlot {
    pub fn new() -> Self {
        Self {
            state: ConnState::Free,
            stream: None,
            http: HttpConn::new(),
            peer_addr: None,
            throttle: ConnThrottle::new(),
            is_cgi: false,
            pending_cgi_body: false,
        }
    }

    pub fn is_free(&self) -> bool {
        self.state == ConnState::Free
    }
}

impl Default for ConnSlot {
    fn default() -> Self {
        Self::new()
    }
}
