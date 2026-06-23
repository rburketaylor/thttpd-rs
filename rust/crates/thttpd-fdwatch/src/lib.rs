//! I/O multiplexing abstraction for thttpd.
//! Re-exports mio types and provides token constants for event dispatch.
//!
//! Token mapping uses two disjoint ranges so that listener tokens never
//! collide with connection tokens, no matter how many addresses `-h`
//! resolves to:
//! - `Token(0 .. MAX_LISTENERS)` = listen sockets (one per bound address)
//! - `Token(CONN_BASE + slab_key)` = connection at slab index, where
//!   `CONN_BASE == MAX_LISTENERS`.
//!
//! Keeping the ranges disjoint means a `-h` value that resolves to more than
//! two addresses (the historical `Token(0)`/`Token(1)` pair) can no longer
//! hand a listener the same token as `conn_token(0)`.

pub use mio::{
    Events, Interest, Poll, Registry, Token,
    event::Event,
    net::{TcpListener, TcpStream},
};

/// Maximum number of listen sockets that can be registered at once. Listener
/// tokens occupy the reserved range `[0, MAX_LISTENERS)`; connection tokens
/// start at `CONN_BASE == MAX_LISTENERS`. The reserved range only needs to
/// exceed the number of addresses a single `-h` can resolve to (the default
/// dual-stack bind uses two), so a small fixed block is plenty.
pub const MAX_LISTENERS: usize = 16;

/// Token for the first listen socket (historically the IPv6 listener).
pub const LISTEN6: Token = Token(0);

/// Token for the second listen socket (historically the IPv4 listener).
pub const LISTEN4: Token = Token(1);

/// Base token value for connections. Connection tokens are
/// `Token(CONN_BASE + slab_key)`, and `CONN_BASE == MAX_LISTENERS` keeps the
/// listener and connection ranges disjoint.
pub const CONN_BASE: usize = MAX_LISTENERS;

/// Token for the listener bound at `listener_idx`. Returns `None` when the
/// index would fall outside the reserved listener range, so callers can
/// refuse to register colliding tokens instead of silently reusing a
/// connection token.
#[inline]
#[must_use]
pub fn listen_token(listener_idx: usize) -> Option<Token> {
    if listener_idx < MAX_LISTENERS {
        Some(Token(listener_idx))
    } else {
        None
    }
}

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
        // CONN_BASE is the size of the reserved listener range, so the first
        // connection token sits just past every possible listener token.
        assert_eq!(CONN_BASE, MAX_LISTENERS);
    }

    #[test]
    fn test_listen_token_roundtrip() {
        assert_eq!(listen_token(0), Some(Token(0)));
        assert_eq!(listen_token(1), Some(Token(1)));
        // A third listener must still map into the listener range — this is
        // the case that used to collide with conn_token(0) == Token(2).
        assert_eq!(listen_token(2), Some(Token(2)));
        assert!(is_listen_token(listen_token(2).unwrap()));
    }

    #[test]
    fn test_listen_token_out_of_range() {
        // The reserved range is exhausted exactly at MAX_LISTENERS.
        assert_eq!(listen_token(MAX_LISTENERS), None);
        assert_eq!(listen_token(MAX_LISTENERS + 1), None);
    }

    #[test]
    fn test_conn_token_starts_at_conn_base() {
        // conn_token(0) is the first connection token and must not overlap
        // any listener token, including Token(2).
        assert_eq!(conn_token(0), Token(CONN_BASE));
        assert!(!is_listen_token(conn_token(0)));
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
        assert!(is_listen_token(Token(2)));
        assert!(!is_listen_token(conn_token(0)));
    }

    #[test]
    fn test_slab_key_rejects_listen_tokens() {
        // Every listener token (including Token(2)) must not decode as a slab
        // key, otherwise a ready listener would be mistaken for a connection.
        assert_eq!(slab_key_from_token(Token(2)), None);
        assert_eq!(slab_key_from_token(Token(MAX_LISTENERS - 1)), None);
        assert_eq!(slab_key_from_token(conn_token(0)), Some(0));
    }
}
