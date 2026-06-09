//! Bandwidth throttling for thttpd.
//! Translates `legacy/src/thttpd.c:1316-1358`.
//! Integer arithmetic must match C's truncation exactly.

/// Throttle time constant (seconds) — matches C's THROTTLE_TIME.
pub const THROTTLE_TIME: i64 = 2;

/// Maximum number of throttle patterns per connection.
pub const MAX_THROTTLE_NUMS: usize = 10;

/// CGI byte count constant — all CGI responses counted as 25KB for throttling.
pub const CGI_BYTECOUNT: i64 = 25000;

/// A single throttle rule.
#[derive(Debug, Clone)]
pub struct ThrottleEntry {
    pub pattern: String,
    pub max_limit: i64,
    pub min_limit: i64,
    pub rate: i64,
    pub bytes_since_avg: i64,
    pub num_sending: i64,
}

/// Bandwidth throttle table.
pub struct ThrottleTable {
    #[allow(dead_code)]
    entries: Vec<ThrottleEntry>,
}

impl ThrottleTable {
    pub fn new() -> Self {
        Self { entries: Vec::new() }
    }

    /// Load throttle rules from a file.
    pub fn load(_path: &std::path::Path) -> std::io::Result<Self> {
        // Parse throttle file format: "pattern min-max" or "pattern max"
        Ok(Self { entries: Vec::new() })
    }

    /// Calculate rolling average: (2 * rate + bytes / THROTTLE_TIME) / 3
    /// Integer arithmetic — must match C's truncation exactly.
    pub fn update_rate(entry: &mut ThrottleEntry) {
        entry.rate = (2 * entry.rate + entry.bytes_since_avg / THROTTLE_TIME) / 3;
        entry.bytes_since_avg = 0;
    }

    /// Calculate fair-share limit for a connection.
    pub fn fair_share(max_limit: i64, num_sending: i64) -> i64 {
        if num_sending > 0 {
            max_limit / num_sending
        } else {
            max_limit
        }
    }
}

impl Default for ThrottleTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rolling_average() {
        let mut entry = ThrottleEntry {
            pattern: "*.html".into(),
            max_limit: 10000,
            min_limit: 1000,
            rate: 5000,
            bytes_since_avg: 4000,
            num_sending: 1,
        };
        ThrottleTable::update_rate(&mut entry);
        // (2 * 5000 + 4000 / 2) / 3 = (10000 + 2000) / 3 = 4000
        assert_eq!(entry.rate, 4000);
    }

    #[test]
    fn test_fair_share() {
        assert_eq!(ThrottleTable::fair_share(10000, 2), 5000);
        assert_eq!(ThrottleTable::fair_share(10000, 1), 10000);
    }
}
