//! Bandwidth throttling for thttpd.
//! Translates `legacy/src/thttpd.c:1316-1358` (throttletab) and
//! `legacy/src/thttpd.c:1369-1462` (read_throttlefile).
//! Integer arithmetic must match C's truncation exactly.

/// Throttle time constant (seconds) — matches C's THROTTLE_TIME.
pub const THROTTLE_TIME: i64 = 2;

/// Maximum number of throttle patterns per connection.
pub const MAX_THROTTLE_NUMS: usize = 10;

/// CGI byte count constant — all CGI responses counted as 25KB for throttling.
pub const CGI_BYTECOUNT: i64 = 25000;

/// Sentinel meaning "no limit".
pub const THROTTLE_NOLIMIT: i64 = -1;

/// A single throttle rule parsed from the throttlefile.
/// Format: `pattern min-max` or `pattern max`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThrottleEntry {
    pub pattern: String,
    pub max_limit: i64,
    pub min_limit: i64,
    pub rate: i64,
    pub bytes_since_avg: i64,
    pub num_sending: i64,
}

impl ThrottleEntry {
    /// Create a new throttle entry from parsed values.
    pub fn new(pattern: String, max_limit: i64, min_limit: i64) -> Self {
        Self {
            pattern,
            max_limit,
            min_limit,
            rate: 0,
            bytes_since_avg: 0,
            num_sending: 0,
        }
    }
}

/// Error returned by `ThrottleTable::load` when a line is unparsable.
/// (C logs a syslog error and continues; we return the error so
/// callers can choose to log or ignore.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThrottleParseError {
    pub line: String,
    pub line_number: usize,
}

impl std::fmt::Display for ThrottleParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unparsable line {}: {:?}", self.line_number, self.line)
    }
}

/// Bandwidth throttle table.
pub struct ThrottleTable {
    entries: Vec<ThrottleEntry>,
}

/// Result of admitting a request against the throttle table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThrottleDecision {
    /// No throttle rule matched — send without rate limiting.
    Unlimited,
    /// Admit the request; carry the matched throttle indexes and the
    /// connection's effective max/min limits.
    Allow {
        tnums: Vec<usize>,
        max_limit: i64,
        min_limit: i64,
    },
    /// Reject with HTTP 503 — the throttle is over `max_limit * 2` or below
    /// `min_limit` and must not start a new transfer.
    Reject,
}

impl ThrottleTable {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Load throttle rules from a file. Matches C's `read_throttlefile`
    /// at thttpd.c:1369-1462:
    ///   - Lines starting with `#` are comments (after trimming)
    ///   - Empty lines are ignored
    ///   - `pattern min-max` (3 sscanf tokens) or `pattern max` (2 tokens)
    ///   - Unparsable lines: log error and skip
    ///   - Leading `/` in pattern is stripped
    ///   - `|/` in pattern is replaced with `|`
    ///   - Realloc array if needed (we use Vec, no realloc needed)
    pub fn load(path: &std::path::Path) -> std::io::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let mut entries = Vec::new();
        for (idx, raw_line) in content.lines().enumerate() {
            // Trim trailing whitespace (C's "nuke trailing whitespace")
            let line = raw_line.trim_end_matches([' ', '\t', '\n', '\r']);

            // Skip comments
            if line.contains('#') {
                // Nuke comments: C uses strchr('#') and sets to '\0'
                let trimmed = line.split('#').next().unwrap_or("");
                if trimmed.trim().is_empty() {
                    continue;
                }
                // Use the comment-stripped line
                let line = trimmed.trim_end();
                if let Some(entry) = parse_line(line) {
                    entries.push(entry);
                } else {
                    eprintln!(
                        "thttpd: unparsable line {} in throttlefile: {:?}",
                        idx + 1,
                        raw_line
                    );
                }
                continue;
            }
            if line.is_empty() {
                continue;
            }
            if let Some(entry) = parse_line(line) {
                entries.push(entry);
            } else {
                eprintln!(
                    "thttpd: unparsable line {} in throttlefile: {:?}",
                    idx + 1,
                    raw_line
                );
            }
        }
        Ok(Self { entries })
    }

    pub fn entries(&self) -> &[ThrottleEntry] {
        &self.entries
    }

    /// Mutable access to entries (for fair-share recompute tests).
    pub fn entries_mut(&mut self) -> &mut [ThrottleEntry] {
        &mut self.entries
    }

    /// True when there are no throttle rules loaded.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Admission decision for a new request. Mirrors C's `check_throttles`
    /// (thttpd.c:1882-1921): walk every pattern, reject the start when the
    /// rolling rate is above `max_limit * 2` or below `min_limit`, otherwise
    /// record the matched indexes, bump `num_sending`, and compute the
    /// fair-share max/min for this connection.
    pub fn check_request(&mut self, filename: &str) -> ThrottleDecision {
        if self.entries.is_empty() {
            return ThrottleDecision::Unlimited;
        }
        let mut tnums: Vec<usize> = Vec::new();
        let mut max_limit = THROTTLE_NOLIMIT;
        let mut min_limit = THROTTLE_NOLIMIT;
        for (tnum, entry) in self.entries.iter_mut().enumerate() {
            if tnums.len() >= MAX_THROTTLE_NUMS {
                break;
            }
            if thttpd_match::match_pattern(&entry.pattern, filename) {
                // Way over the limit, or below the minimum: don't even start.
                if entry.rate > entry.max_limit * 2 || entry.rate < entry.min_limit {
                    // Roll back any increments done so far on this request.
                    for &t in &tnums {
                        self.entries[t].num_sending -= 1;
                    }
                    return ThrottleDecision::Reject;
                }
                if entry.num_sending < 0 {
                    entry.num_sending = 0;
                }
                entry.num_sending += 1;
                tnums.push(tnum);
                let l = entry.max_limit / entry.num_sending;
                max_limit = if max_limit == THROTTLE_NOLIMIT {
                    l
                } else {
                    max_limit.min(l)
                };
                min_limit = if min_limit == THROTTLE_NOLIMIT {
                    entry.min_limit
                } else {
                    min_limit.max(entry.min_limit)
                };
            }
        }
        if tnums.is_empty() {
            ThrottleDecision::Unlimited
        } else {
            ThrottleDecision::Allow {
                tnums,
                max_limit,
                min_limit,
            }
        }
    }

    /// Decrement `num_sending` for each matched throttle of a closing
    /// connection (C's `clear_throttles`, thttpd.c:1922-1928).
    pub fn clear(&mut self, tnums: &[usize]) {
        for &tnum in tnums {
            if let Some(entry) = self.entries.get_mut(tnum) {
                entry.num_sending -= 1;
            }
        }
    }

    /// Account `n` body bytes against every matched throttle (C does
    /// `throttles[tnum].bytes_since_avg += sz` in handle_send).
    pub fn add_bytes(&mut self, tnums: &[usize], n: i64) {
        for &tnum in tnums {
            if let Some(entry) = self.entries.get_mut(tnum) {
                entry.bytes_since_avg += n;
            }
        }
    }

    /// Recompute a connection's fair-share max from the current `num_sending`
    /// of each matched throttle. Returns THROTTLE_NOLIMIT when nothing matched.
    /// Mirrors `update_throttles`'s per-connection loop (thttpd.c:1989-2002).
    pub fn fair_share_for(&self, tnums: &[usize]) -> i64 {
        let mut max_limit = THROTTLE_NOLIMIT;
        for &tnum in tnums {
            if let Some(entry) = self.entries.get(tnum) {
                let senders = entry.num_sending.max(1);
                let l = entry.max_limit / senders;
                max_limit = if max_limit == THROTTLE_NOLIMIT {
                    l
                } else {
                    max_limit.min(l)
                };
            }
        }
        max_limit
    }

    /// Update rolling averages for every throttle. Called every THROTTLE_TIME
    /// seconds by the event loop (C's `update_throttles`, thttpd.c:1968-1976).
    pub fn update_averages(&mut self) {
        for entry in &mut self.entries {
            Self::update_rate(entry);
        }
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

    /// Check if a file should be throttled. Returns the matching entry's
    /// (max_limit, min_limit) or (NOLIMIT, NOLIMIT) if no match.
    /// Mirrors C's `check_throttles` at thttpd.c:1882-1921 (read-only view).
    pub fn check_throttles(&self, filename: &str) -> (i64, i64) {
        if self.entries.is_empty() {
            return (THROTTLE_NOLIMIT, THROTTLE_NOLIMIT);
        }
        for entry in &self.entries {
            if thttpd_match::match_pattern(&entry.pattern, filename) {
                return (entry.max_limit, entry.min_limit);
            }
        }
        (THROTTLE_NOLIMIT, THROTTLE_NOLIMIT)
    }
}

impl Default for ThrottleTable {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse a single line of the throttlefile. Returns Some(ThrottleEntry)
/// on success, None on parse failure.
fn parse_line(line: &str) -> Option<ThrottleEntry> {
    // Format: "pattern min-max" or "pattern max"
    // Pattern can be quoted (no spaces inside) or unquoted.
    let (pattern, rest) = if let Some(stripped) = line.strip_prefix('"') {
        // Quoted pattern: find closing quote
        let end = stripped.find('"')?;
        let pattern = &stripped[..end];
        let rest = stripped[end + 1..].trim_start();
        (pattern, rest)
    } else {
        // Unquoted: pattern is up to first whitespace
        let mut parts = line.splitn(2, char::is_whitespace);
        let pattern = parts.next()?.trim();
        let rest = parts.next()?.trim_start();
        (pattern, rest)
    };

    // Strip leading slashes from pattern
    let pattern = pattern.trim_start_matches('/');

    // Replace "|/" with "|" in pattern (C thttpd.c:1425-1426)
    let pattern = if pattern.contains("|/") {
        pattern.replace("|/", "|")
    } else {
        pattern.to_string()
    };

    // Parse the rate(s) — try "min-max" first, fall back to "max"
    // The first whitespace-separated token after the pattern is the rate spec.
    let first = rest.split(char::is_whitespace).next()?.trim();

    let (min_limit, max_limit) = if let Some((min_str, max_str)) = first.split_once('-') {
        // "min-max" format — two integers separated by a dash
        let min_limit: i64 = min_str.trim().parse().ok()?;
        let max_limit: i64 = max_str.trim().parse().ok()?;
        (min_limit, max_limit)
    } else {
        // "max" format — single integer
        let max_limit: i64 = first.parse().ok()?;
        (0, max_limit)
    };

    if pattern.is_empty() {
        return None;
    }

    Some(ThrottleEntry::new(
        pattern.to_string(),
        max_limit,
        min_limit,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

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

    #[test]
    fn test_parse_line_single_rate() {
        let entry = parse_line("*.html 1000000").unwrap();
        assert_eq!(entry.pattern, "*.html");
        assert_eq!(entry.max_limit, 1000000);
        assert_eq!(entry.min_limit, 0);
    }

    #[test]
    fn test_parse_line_min_max() {
        let entry = parse_line("*.html 1000-1000000").unwrap();
        assert_eq!(entry.pattern, "*.html");
        assert_eq!(entry.max_limit, 1000000);
        assert_eq!(entry.min_limit, 1000);
    }

    #[test]
    fn test_parse_line_strips_leading_slash() {
        let entry = parse_line("/cgi-bin/** 1000-10000").unwrap();
        assert_eq!(entry.pattern, "cgi-bin/**");
    }

    #[test]
    fn test_parse_line_replaces_slash_pipe() {
        // C strips leading / and replaces |/ with |
        let entry = parse_line("|/*.html 1000").unwrap();
        assert_eq!(entry.pattern, "|*.html");
    }

    #[test]
    fn test_parse_line_unparsable() {
        // Missing rate
        assert!(parse_line("*.html").is_none());
        // Garbage
        assert!(parse_line("just some text here").is_none());
    }

    #[test]
    fn test_load_skips_comments_and_blanks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("throttle.conf");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "# This is a comment").unwrap();
        writeln!(f).unwrap();
        writeln!(f, "*.html 1000000").unwrap();
        writeln!(f, "# Another comment").unwrap();
        writeln!(f, "*.png 500-5000").unwrap();
        writeln!(f, "garbage line without numbers").unwrap();

        let table = ThrottleTable::load(&path).unwrap();
        assert_eq!(table.entries().len(), 2);
        assert_eq!(table.entries()[0].pattern, "*.html");
        assert_eq!(table.entries()[0].max_limit, 1000000);
        assert_eq!(table.entries()[1].pattern, "*.png");
        assert_eq!(table.entries()[1].min_limit, 500);
    }

    #[test]
    fn test_check_throttles_match() {
        let entry = ThrottleEntry::new("*.html".into(), 1000, 0);
        let table = ThrottleTable {
            entries: vec![entry],
        };
        let (max, _min) = table.check_throttles("page.html");
        assert_eq!(max, 1000);
    }

    #[test]
    fn test_check_throttles_no_match() {
        let entry = ThrottleEntry::new("*.html".into(), 1000, 0);
        let table = ThrottleTable {
            entries: vec![entry],
        };
        let (max, min) = table.check_throttles("page.png");
        assert_eq!(max, THROTTLE_NOLIMIT);
        assert_eq!(min, THROTTLE_NOLIMIT);
    }

    #[test]
    fn test_check_throttles_empty_table() {
        let table = ThrottleTable::new();
        let (max, min) = table.check_throttles("anything");
        assert_eq!(max, THROTTLE_NOLIMIT);
        assert_eq!(min, THROTTLE_NOLIMIT);
    }

    #[test]
    fn check_request_admits_and_bumps_num_sending() {
        let mut table = ThrottleTable {
            entries: vec![ThrottleEntry::new("*.html".into(), 10000, 0)],
        };
        let dec = table.check_request("page.html");
        let ThrottleDecision::Allow {
            tnums, max_limit, ..
        } = dec
        else {
            panic!("expected Allow, got {dec:?}");
        };
        assert_eq!(tnums, vec![0]);
        assert_eq!(max_limit, 10000); // 10000 / 1 sender
        assert_eq!(table.entries()[0].num_sending, 1);
    }

    #[test]
    fn check_request_rejects_when_rate_far_exceeds_limit() {
        let mut entry = ThrottleEntry::new("*.html".into(), 1000, 0);
        entry.rate = 5000; // > max_limit * 2 = 2000
        let mut table = ThrottleTable {
            entries: vec![entry],
        };
        assert_eq!(table.check_request("page.html"), ThrottleDecision::Reject);
        // num_sending must not have been bumped.
        assert_eq!(table.entries()[0].num_sending, 0);
    }

    #[test]
    fn check_request_rejects_when_below_min_limit() {
        let mut entry = ThrottleEntry::new("*.html".into(), 1000000, 5000);
        entry.rate = 100; // < min_limit = 5000
        let mut table = ThrottleTable {
            entries: vec![entry],
        };
        assert_eq!(table.check_request("page.html"), ThrottleDecision::Reject);
    }

    #[test]
    fn clear_decrements_num_sending() {
        let mut table = ThrottleTable {
            entries: vec![ThrottleEntry::new("*.html".into(), 10000, 0)],
        };
        let ThrottleDecision::Allow { tnums, .. } = table.check_request("page.html") else {
            unreachable!();
        };
        assert_eq!(table.entries()[0].num_sending, 1);
        table.clear(&tnums);
        assert_eq!(table.entries()[0].num_sending, 0);
    }

    #[test]
    fn fair_share_splits_bandwidth_across_senders() {
        let mut table = ThrottleTable {
            entries: vec![ThrottleEntry::new("*.html".into(), 10000, 0)],
        };
        // Two admitted requests share the 10000 budget.
        let a = table.check_request("a.html");
        let b = table.check_request("b.html");
        let tnums_a = match a {
            ThrottleDecision::Allow { tnums, .. } => tnums,
            _ => unreachable!(),
        };
        assert_eq!(table.fair_share_for(&tnums_a), 5000);
        // After one clears, the survivor gets the full budget again.
        let tnums_b = match b {
            ThrottleDecision::Allow { tnums, .. } => tnums,
            _ => unreachable!(),
        };
        table.clear(&tnums_b);
        assert_eq!(table.fair_share_for(&tnums_a), 10000);
    }

    #[test]
    fn update_averages_matches_c_formula() {
        let mut table = ThrottleTable {
            entries: vec![ThrottleEntry {
                pattern: "*.html".into(),
                max_limit: 10000,
                min_limit: 0,
                rate: 5000,
                bytes_since_avg: 4000,
                num_sending: 1,
            }],
        };
        table.update_averages();
        // (2 * 5000 + 4000 / 2) / 3 = (10000 + 2000) / 3 = 4000
        assert_eq!(table.entries()[0].rate, 4000);
        assert_eq!(table.entries()[0].bytes_since_avg, 0);
    }
}
