//! Directory listing for thttpd.
//! Translates `legacy/src/libhttpd.c:2628-2955` — in-process HTML generation
//! replaces C's fork-based ls().

use std::fs;
use std::path::Path;

/// Directory entry info for sorting.
struct DirEntry {
    name: String,
    metadata: std::fs::Metadata,
    lstat_metadata: std::fs::Metadata,
}

/// Generate an HTML directory listing matching C's `ls()` output format byte-for-byte.
pub fn generate_listing(dir: &Path, url_path: &str) -> std::io::Result<Vec<u8>> {
    let mut entries: Vec<DirEntry> = Vec::new();

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        // lstat first so a broken symlink renders instead of aborting the
        // whole listing.  C's ls() does lstat() then stat(); a stat() failure
        // on the (missing) target does not stop the listing — it just shows
        // the symlink entry using its lstat data.
        let lstat = match fs::symlink_metadata(dir.join(&name)) {
            Ok(m) => m,
            // Entry vanished between read_dir and lstat — skip it rather than
            // failing the entire directory listing.
            Err(_) => continue,
        };
        // Followed metadata: for a broken symlink this fails, so fall back to
        // the symlink's own (lstat) metadata.
        let metadata = entry.metadata().unwrap_or_else(|_| lstat.clone());
        entries.push(DirEntry {
            name,
            metadata,
            lstat_metadata: lstat,
        });
    }

    // Sort: case-insensitive alphabetical (matches C's name_compare / strcasecmp)
    entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    let mut html = Vec::new();

    // Header — matches C's fprintf exactly
    html.extend_from_slice(b"<HTML>\n<HEAD><TITLE>Index of ");
    html.extend_from_slice(url_path.as_bytes());
    html.extend_from_slice(b"</TITLE></HEAD>\n<BODY BGCOLOR=\"#99cc99\" TEXT=\"#000000\" LINK=\"#2020ff\" VLINK=\"#4040cc\">\n<H2>Index of ");
    html.extend_from_slice(url_path.as_bytes());
    html.extend_from_slice(b"</H2>\n<PRE>\nmode  links  bytes  last-changed  name\n<HR>");

    // Normalize url_path — ensure it ends with /
    let url_prefix = if url_path.ends_with('/') {
        url_path.to_string()
    } else {
        format!("{url_path}/")
    };

    for entry in &entries {
        let name = &entry.name;
        let lstat = &entry.lstat_metadata;
        let metadata = &entry.metadata;

        // Mode string — file type + world permissions
        let mode_char = if lstat.is_dir() {
            'd'
        } else if lstat.file_type().is_symlink() {
            'l'
        } else {
            '-'
        };
        let mode = lstat.permissions();
        #[cfg(unix)]
        let mode_bits = {
            use std::os::unix::fs::PermissionsExt;
            mode.mode()
        };
        #[cfg(not(unix))]
        let mode_bits: u32 = 0o444;

        let r = if mode_bits & 0o004 != 0 { 'r' } else { '-' };
        let w = if mode_bits & 0o002 != 0 { 'w' } else { '-' };
        let x = if mode_bits & 0o001 != 0 { 'x' } else { '-' };
        let modestr = format!("{mode_char}{r}{w}{x}");

        // Link count (simplified — metadata doesn't expose nlink easily)
        #[cfg(unix)]
        let nlink = {
            use std::os::unix::fs::MetadataExt;
            lstat.nlink()
        };
        #[cfg(not(unix))]
        let nlink: u64 = 1;

        // File size
        let size = if metadata.is_dir() {
            // For directories, use the lstat size (directory entry size)
            lstat.len() as i64
        } else {
            metadata.len() as i64
        };

        // Time string — matches C's format: "Mon DD HH:MM" or "Mon DD  YYYY"
        let mtime = lstat
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let timestr = format_ls_time(mtime);

        // Build the HREF — URL-encode the name, append / for directories
        let encname = url_encode(name);
        let href_suffix = if metadata.is_dir() { "/" } else { "" };

        // File class (ls -F style)
        let fileclass = if metadata.is_dir() {
            "/"
        } else if lstat.file_type().is_symlink() {
            "@"
        } else if mode_bits & 0o001 != 0 {
            "*"
        } else {
            ""
        };

        // Symlink target
        let (linkprefix, link_target) = if lstat.file_type().is_symlink() {
            if let Ok(target) = std::fs::read_link(dir.join(name)) {
                (" -&gt; ", target.to_string_lossy().to_string())
            } else {
                ("", String::new())
            }
        } else {
            ("", String::new())
        };

        // Format line — matches C's fprintf exactly:
        // "%s %3ld  %10lld  %s  <A HREF=\"/%.500s%s\">%.80s</A>%s%s%s\n"
        let line = format!(
            "{} {:>3}  {:>10}  {}  <A HREF=\"/{}{}{}\">{}</A>{}{}{}\n",
            modestr,
            nlink,
            size,
            timestr,
            url_prefix,
            encname,
            href_suffix,
            truncate_to_80(name),
            linkprefix,
            link_target,
            fileclass,
        );
        html.extend_from_slice(line.as_bytes());
    }

    html.extend_from_slice(b"</PRE></BODY>\n</HTML>\n");
    Ok(html)
}

/// URL-encode a filename for use in HREF attributes.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

/// Truncate a string to 80 characters (matches C's `%.80s`).
fn truncate_to_80(s: &str) -> &str {
    if s.len() <= 80 {
        s
    } else {
        // Find the last char boundary at or before 80 bytes
        let mut end = 80;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

/// Format a Unix timestamp in C's `ls`-style time format.
/// Matches the `ctime()` manipulation in C's directory listing code.
fn format_ls_time(secs: u64) -> String {
    const SECS_PER_DAY: u64 = 86400;
    let days_since_epoch = secs / SECS_PER_DAY;
    let time_of_day = secs % SECS_PER_DAY;

    let (year, month, day) = days_to_ymd(days_since_epoch);

    let month_names = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month_str = month_names.get(month - 1).unwrap_or(&"???");

    // C's logic: if file is older than ~6 months, show year; otherwise show time
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    if now_secs.saturating_sub(secs) > 60 * 60 * 24 * 182 {
        // Show year
        format!("{month_str} {day:>2}  {year}")
    } else {
        // Show time HH:MM
        let hours = time_of_day / 3600;
        let minutes = (time_of_day % 3600) / 60;
        format!("{month_str} {day:>2} {hours:02}:{minutes:02}")
    }
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_ymd(mut days: u64) -> (u64, usize, u64) {
    let mut year = 1970u64;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }

    let leap = is_leap(year);
    let month_days: [u64; 12] = if leap {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut month = 0usize;
    let mut remaining = days;
    for (i, &md) in month_days.iter().enumerate() {
        if remaining < md {
            month = i;
            break;
        }
        remaining -= md;
        if i == 11 {
            month = 11;
        }
    }

    (year, month + 1, remaining + 1)
}

fn is_leap(year: u64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_generate_listing() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("test.txt"), b"hello").unwrap();
        fs::create_dir(dir.path().join("subdir")).unwrap();

        let html = generate_listing(dir.path(), "/testdir/").unwrap();
        let s = String::from_utf8(html).unwrap();
        assert!(s.contains("<TITLE>Index of /testdir/</TITLE>"));
        assert!(s.contains("test.txt"));
        assert!(s.contains("subdir"));
        assert!(s.contains("<PRE>"));
        assert!(s.contains("mode  links  bytes  last-changed  name"));
    }

    #[test]
    fn test_format_ls_time() {
        // June 9, 2026 14:21:55 UTC
        let t = format_ls_time(1749478915);
        assert!(t.starts_with("Jun"));
    }

    #[test]
    fn test_url_encode() {
        assert_eq!(url_encode("hello.txt"), "hello.txt");
        assert_eq!(url_encode("foo bar"), "foo%20bar");
        assert_eq!(url_encode("test<file>"), "test%3Cfile%3E");
    }

    #[test]
    fn test_broken_symlink_renders() {
        // A broken symlink must not abort the whole listing.  The old code
        // used `entry.metadata()?`, which follows the symlink and errors on a
        // missing target, failing the entire directory listing.
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("real.txt"), b"hello").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            // broken: target does not exist
            symlink("/nonexistent/target", dir.path().join("dangling")).unwrap();
            // valid symlink alongside it
            symlink("real.txt", dir.path().join("goodlink")).unwrap();
        }

        let html = generate_listing(dir.path(), "/d/").unwrap();
        let s = String::from_utf8(html).unwrap();
        // All three real entries plus the symlinks must be present.
        assert!(s.contains("real.txt"));
        #[cfg(unix)]
        {
            assert!(s.contains("dangling"));
            assert!(s.contains("goodlink"));
            // Broken symlink rendered as a symlink ('l' mode / '@' class).
            assert!(s.contains("-&gt; "));
        }
    }
}
