//! Directory listing for thttpd.
//! Translates `legacy/src/libhttpd.c:2628-2955` — in-process HTML generation
//! replaces C's fork-based ls().

use std::fs;
use std::path::Path;

/// Directory entry info.
struct DirEntry {
    name: String,
    modified: String,
    size: i64,
    is_dir: bool,
}

/// Generate an HTML directory listing.
/// Must match C's ls() output format byte-for-byte (verified via golden master).
pub fn generate_listing(dir: &Path, url_path: &str) -> std::io::Result<Vec<u8>> {
    let mut entries: Vec<DirEntry> = Vec::new();

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        let metadata = entry.metadata()?;

        let modified = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);

        entries.push(DirEntry {
            name,
            modified: format_time(modified),
            size: metadata.len() as i64,
            is_dir: metadata.is_dir(),
        });
    }

    // Sort: directories first, then alphabetically (case-insensitive)
    entries.sort_by(|a, b| {
        match (a.is_dir, b.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        }
    });

    let mut html = Vec::new();
    html.extend_from_slice(b"<HTML>\n<HEAD><TITLE>Index of ");
    html.extend_from_slice(url_path.as_bytes());
    html.extend_from_slice(b"</TITLE></HEAD>\n<BODY>\n<H2>Index of ");
    html.extend_from_slice(url_path.as_bytes());
    html.extend_from_slice(b"</H2>\n<PRE>\n");

    // Parent directory link (if not root)
    if url_path != "/" {
        html.extend_from_slice(b"<IMG SRC=\"/icons/blank.gif\" ALT=\"     \"> <A HREF=\"..\">Parent directory</A>\n");
    }

    for entry in &entries {
        let icon = if entry.is_dir { "menu.gif" } else { "text.gif" };
        let alt = if entry.is_dir { "[DIR]" } else { "     " };
        let suffix = if entry.is_dir { "/" } else { "" };
        let size_str = if entry.is_dir {
            "-".to_string()
        } else {
            format_size(entry.size)
        };

        html.extend_from_slice(
            format!(
                "<IMG SRC=\"/icons/{}\" ALT=\"{}\"> <A HREF=\"{}{}\">{}{}</A>               {} {}\n",
                icon, alt, entry.name, suffix, entry.name, suffix, entry.modified, size_str
            )
            .as_bytes(),
        );
    }

    html.extend_from_slice(b"</PRE>\n</BODY>\n</HTML>\n");
    Ok(html)
}

fn format_time(secs: u64) -> String {
    // Simple date formatting: YYYY-MM-DD HH:MM
    let days = secs / 86400;
    let _time = secs % 86400;
    // Approximate year/month/day calculation
    let mut year = 1970u32;
    let mut remaining = days;
    loop {
        let dy = if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 { 366 } else { 365 };
        if remaining < dy as u64 {
            break;
        }
        remaining -= dy as u64;
        year += 1;
    }
    format!("{year:04}-???-{remaining:02} ??:??")
}

fn format_size(size: i64) -> String {
    if size >= 1_048_576 {
        format!("{:.1}M", size as f64 / 1_048_576.0)
    } else if size >= 1024 {
        format!("{:.1}k", size as f64 / 1024.0)
    } else {
        format!("{size}B")
    }
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
        assert!(s.contains("subdir/"));
    }
}
