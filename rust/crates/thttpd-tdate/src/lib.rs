//! HTTP date parsing for thttpd.
//! Translates `legacy/src/tdate_parse.c` (330 lines).
//! Parses RFC 1123, RFC 850, asctime, and Atoi-style date formats.

/// Parse an HTTP date string into a Unix timestamp.
///
/// Supports:
/// - RFC 1123: `"Sun, 06 Nov 1994 08:49:37 GMT"`
/// - RFC 850: `"Sunday, 06-Nov-94 08:49:37 GMT"`
/// - asctime: `"Sun Nov  6 08:49:37 1994"`
/// - Atoi-style: plain integer seconds since epoch
pub fn parse_http_date(input: &str) -> Option<i64> {
    let s = input.trim();

    // Try plain integer first
    if let Ok(ts) = s.parse::<i64>() {
        return Some(ts);
    }

    // Try RFC 1123: "Sun, 06 Nov 1994 08:49:37 GMT"
    if let Some(ts) = parse_rfc1123(s) {
        return Some(ts);
    }

    // Try RFC 850: "Sunday, 06-Nov-94 08:49:37 GMT"
    if let Some(ts) = parse_rfc850(s) {
        return Some(ts);
    }

    // Try asctime: "Sun Nov  6 08:49:37 1994"
    if let Some(ts) = parse_asctime(s) {
        return Some(ts);
    }

    None
}

fn month_num(name: &str) -> Option<u32> {
    match name {
        "Jan" => Some(0),
        "Feb" => Some(1),
        "Mar" => Some(2),
        "Apr" => Some(3),
        "May" => Some(4),
        "Jun" => Some(5),
        "Jul" => Some(6),
        "Aug" => Some(7),
        "Sep" => Some(8),
        "Oct" => Some(9),
        "Nov" => Some(10),
        "Dec" => Some(11),
        _ => None,
    }
}

fn is_leap_year(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_year(y: i32) -> i32 {
    if is_leap_year(y) { 366 } else { 365 }
}

fn days_in_month(m: u32, y: i32) -> u32 {
    match m {
        0 | 2 | 4 | 6 | 7 | 9 | 11 => 31,
        3 | 5 | 8 | 10 => 30,
        1 => {
            if is_leap_year(y) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

fn date_to_epoch(year: i32, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> i64 {
    let mut days: i64 = 0;
    // Days from 1970 to year-1
    let y = if year < 1970 { year..1970 } else { 1970..year };
    for yr in y {
        days += days_in_year(yr) as i64;
    }
    if year < 1970 {
        days = -days;
    }
    // Days in this year before this month
    for m in 0..month {
        days += days_in_month(m, year) as i64;
    }
    // Days in this month
    days += (day - 1) as i64;

    days * 86400 + (hour as i64) * 3600 + (min as i64) * 60 + sec as i64
}

fn parse_rfc1123(s: &str) -> Option<i64> {
    // "Sun, 06 Nov 1994 08:49:37 GMT"
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() != 6 {
        return None;
    }
    // parts[0] = "Sun," (weekday+comma) — skip
    // parts[1] = day, parts[2] = month, parts[3] = year (or swapped with time)
    let day: u32 = parts[1].parse().ok()?;
    let month = month_num(parts[2])?;
    // parts[3] could be year or time depending on format; try year first
    let year: i32 = parts[3].parse().ok()?;
    let time_parts: Vec<&str> = parts[4].split(':').collect();
    if time_parts.len() != 3 {
        return None;
    }
    let hour: u32 = time_parts[0].parse().ok()?;
    let min: u32 = time_parts[1].parse().ok()?;
    let sec: u32 = time_parts[2].parse().ok()?;
    Some(date_to_epoch(year, month, day, hour, min, sec))
}

fn parse_rfc850(s: &str) -> Option<i64> {
    // "Sunday, 06-Nov-94 08:49:37 GMT"
    let parts: Vec<&str> = s.split([' ', ',']).filter(|p| !p.is_empty()).collect();
    if parts.len() < 4 {
        return None;
    }
    let date_parts: Vec<&str> = parts[0].split('-').collect();
    if date_parts.len() != 3 {
        return None;
    }
    let day: u32 = date_parts[0].parse().ok()?;
    let month = month_num(date_parts[1])?;
    let mut year: i32 = date_parts[2].parse().ok()?;
    if year < 70 {
        year += 2000;
    } else if year < 100 {
        year += 1900;
    }
    let time_parts: Vec<&str> = parts[1].split(':').collect();
    if time_parts.len() != 3 {
        return None;
    }
    let hour: u32 = time_parts[0].parse().ok()?;
    let min: u32 = time_parts[1].parse().ok()?;
    let sec: u32 = time_parts[2].parse().ok()?;
    Some(date_to_epoch(year, month, day, hour, min, sec))
}

fn parse_asctime(s: &str) -> Option<i64> {
    // "Sun Nov  6 08:49:37 1994"
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() != 5 {
        return None;
    }
    let month = month_num(parts[1])?;
    let day: u32 = parts[2].parse().ok()?;
    let time_parts: Vec<&str> = parts[3].split(':').collect();
    if time_parts.len() != 3 {
        return None;
    }
    let hour: u32 = time_parts[0].parse().ok()?;
    let min: u32 = time_parts[1].parse().ok()?;
    let sec: u32 = time_parts[2].parse().ok()?;
    let year: i32 = parts[4].parse().ok()?;
    Some(date_to_epoch(year, month, day, hour, min, sec))
}

/// Returns the current time as an HTTP date string (RFC 1123 format).
pub fn format_http_date(timestamp: i64) -> String {
    let days = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];

    let mut remaining = timestamp;
    let mut year = 1970i32;
    loop {
        let dy = days_in_year(year);
        if remaining < dy as i64 * 86400 {
            break;
        }
        remaining -= dy as i64 * 86400;
        year += 1;
    }

    let mut month = 0u32;
    loop {
        let dm = days_in_month(month, year);
        if remaining < dm as i64 * 86400 {
            break;
        }
        remaining -= dm as i64 * 86400;
        month += 1;
    }

    let day = (remaining / 86400) as u32 + 1;
    remaining %= 86400;
    let hour = (remaining / 3600) as u32;
    remaining %= 3600;
    let min = (remaining / 60) as u32;
    let sec = (remaining % 60) as u32;

    // Day of week calculation
    let total_days = (timestamp / 86400) as i32;
    let dow = ((total_days % 7 + 7) % 7) as usize;

    format!(
        "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
        days[dow], day, months[month as usize], year, hour, min, sec
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rfc1123() {
        let ts = parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT");
        assert_eq!(ts, Some(784111777));
    }

    #[test]
    fn test_plain_integer() {
        let ts = parse_http_date("784111777");
        assert_eq!(ts, Some(784111777));
    }

    #[test]
    fn test_roundtrip() {
        let ts: i64 = 784111777;
        let formatted = format_http_date(ts);
        let parsed = parse_http_date(&formatted);
        assert_eq!(parsed, Some(ts));
    }
}
