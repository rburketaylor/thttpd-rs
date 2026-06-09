//! Request parsing FSM states for thttpd.
//! Translates the 12-state checked_state machine from `legacy/src/libhttpd.h:147-158`.

/// Result of the request-parsing FSM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GotRequest {
    /// No complete request yet — need more data.
    NoRequest,
    /// A complete request has been parsed.
    GotRequest,
    /// The request is malformed.
    BadRequest,
}

/// FSM states for incremental request parsing.
/// Translates `CHST_FIRSTWORD` through `CHST_BOGUS` (12 states).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseState {
    /// Parsing the first word of the request line (method).
    FirstWord,
    /// In whitespace between first and second word.
    FirstWs,
    /// Parsing the second word (URI).
    SecondWord,
    /// In whitespace between second and third word.
    SecondWs,
    /// Parsing the third word (HTTP version).
    ThirdWord,
    /// After third word, expecting CRLF.
    ThirdWs,
    /// At a line feed character.
    Lf,
    /// At a carriage return.
    Cr,
    /// After CR, expecting LF.
    Crlf,
    /// After CRLF, expecting CR of blank line (end of headers).
    Crlfcr,
    /// Inside a header line.
    Line,
    /// Complete request received.
    GotRequest,
    /// Malformed request detected.
    Bogus,
}

impl ParseState {
    /// Initial FSM state.
    pub fn initial() -> Self {
        ParseState::FirstWord
    }

    /// Check if this state represents a terminal condition.
    pub fn is_terminal(&self) -> bool {
        matches!(self, ParseState::GotRequest | ParseState::Bogus)
    }
}
