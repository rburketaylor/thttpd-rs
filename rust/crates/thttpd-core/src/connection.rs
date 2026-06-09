//! Connection management for thttpd.
//! Translates connection handling from `legacy/src/thttpd.c`.
//! Uses `slab::Slab` for connection table management.

use mio::net::TcpStream;
use std::net::SocketAddr;
use thttpd_http::conn::ConnState;
use thttpd_http::HttpConn;

/// A connection slot in the connection table.
pub struct ConnSlot {
    pub state: ConnState,
    pub stream: Option<TcpStream>,
    pub http: HttpConn,
    /// Remote address for logging / CGI REMOTE_ADDR.
    pub peer_addr: Option<SocketAddr>,
}

impl ConnSlot {
    pub fn new() -> Self {
        Self {
            state: ConnState::Free,
            stream: None,
            http: HttpConn::new(),
            peer_addr: None,
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
