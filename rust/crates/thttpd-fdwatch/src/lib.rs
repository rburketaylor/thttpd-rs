//! I/O multiplexing abstraction for thttpd.
//! Re-exports mio types and provides token constants for event dispatch.
//!
//! Token mapping:
//! - `Token(0)` = LISTEN6 (IPv6 listen socket)
//! - `Token(1)` = LISTEN4 (IPv4 listen socket)
//! - `Token(CONN_BASE + slab_key)` = connection at slab index

pub use mio::{
    Events, Interest, Poll, Registry, Token,
    event::Event,
    net::{TcpListener, TcpStream},
};

/// Token for the IPv6 listen socket.
pub const LISTEN6: Token = Token(0);

/// Token for the IPv4 listen socket.
pub const LISTEN4: Token = Token(1);

/// Base token value for connections. Connection tokens are `Token(CONN_BASE + slab_key)`.
pub const CONN_BASE: usize = 2;

/// Convert a slab key to a mio Token.
#[inline]
#[must_use]
pub fn conn_token(slab_key: usize) -> Token {
    Token(CONN_BASE + slab_key)
}

/// Extract the slab key from a connection Token. Returns None for listen tokens.
#[inline]
pub fn slab_key_from_token(token: Token) -> Option<usize> {
    if token.0 >= CONN_BASE {
        Some(token.0 - CONN_BASE)
    } else {
        None
    }
}

/// Check if a token corresponds to a listen socket.
#[inline]
#[must_use]
pub fn is_listen_token(token: Token) -> bool {
    token.0 < CONN_BASE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_constants() {
        assert_eq!(LISTEN6, Token(0));
        assert_eq!(LISTEN4, Token(1));
        assert_eq!(CONN_BASE, 2);
    }

    #[test]
    fn test_conn_token_roundtrip() {
        let key = 42;
        let token = conn_token(key);
        assert_eq!(slab_key_from_token(token), Some(key));
    }

    #[test]
    fn test_listen_tokens() {
        assert!(is_listen_token(LISTEN6));
        assert!(is_listen_token(LISTEN4));
        assert!(!is_listen_token(conn_token(0)));
    }
}
