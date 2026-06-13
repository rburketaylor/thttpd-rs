//! HTTP method types for thttpd.

/// HTTP request method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Head,
    Post,
    Unknown,
}

impl Method {
    /// Parse a method from its string representation.
    pub fn parse(s: &str) -> Self {
        match s {
            "GET" => Method::Get,
            "HEAD" => Method::Head,
            "POST" => Method::Post,
            _ => Method::Unknown,
        }
    }

    /// Returns the method as a string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Head => "HEAD",
            Method::Post => "POST",
            Method::Unknown => "UNKNOWN",
        }
    }
}
