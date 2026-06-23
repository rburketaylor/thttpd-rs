//! Access logging for thttpd.
//! Translates `legacy/src/libhttpd.c:3864-3945` (`make_log_entry`).
//!
//! Emits CERN Combined Log Format lines:
//! ```text
//! ADDR - USER [DATE] "METHOD URL PROTO" STATUS BYTES "REFERER" "UA"
//! ```
//! Targets mirror C thttpd:
//! - no `-l`: syslog target (currently a no-op file sink; see KNOWN_DEVIATIONS)
//! - `-l <path>`: append to file (created/chmod 0600, owned by the server uid)
//! - `-l -`: stdout
//! - `-l /dev/null`: disabled

use crate::config::ServerConfig;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Where access-log lines are sent.
pub enum LogTarget {
    /// Default when no `-l` is supplied. C thttpd writes to syslog; the Rust
    /// port currently drops these lines (documented deviation) rather than
    /// pulling in a syslog dependency.
    Syslog,
    /// `-l -` — write to stdout.
    Stdout,
    /// `-l <path>` — append to a file that SIGHUP can reopen.
    File { path: PathBuf, file: std::fs::File },
    /// `-l /dev/null` — logging disabled.
    Disabled,
}

/// Access-log owner, held by [`crate::server::Server`].
pub struct AccessLogger {
    target: LogTarget,
}

/// A single request's log-relevant fields.
pub struct LogEntry<'a> {
    pub remote_addr: &'a str,
    pub remote_user: &'a str,
    pub method: &'a str,
    pub url: &'a str,
    pub protocol: &'a str,
    pub status: u16,
    pub bytes_sent: i64,
    pub referer: &'a str,
    pub user_agent: &'a str,
}

impl AccessLogger {
    /// Open the configured log target. Mirrors thttpd.c:417-456: `-l -` is
    /// stdout, `-l /dev/null` disables logging, anything else opens (or
    /// creates) the file for append. Failure to open a real logfile is fatal,
    /// matching C's syslog(LOG_CRIT) + exit path.
    pub fn open(config: &ServerConfig) -> io::Result<Self> {
        let target = match &config.logfile {
            None => LogTarget::Syslog,
            Some(path) if path == std::path::Path::new("-") => LogTarget::Stdout,
            Some(path) if path == std::path::Path::new("/dev/null") => LogTarget::Disabled,
            Some(path) => {
                let file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .read(false)
                    .open(path)?;
                configure_logfile(&file, config)?;
                LogTarget::File {
                    path: path.clone(),
                    file,
                }
            }
        };
        Ok(Self { target })
    }

    /// Rewrite a file-backed log path to the path visible after chroot.
    ///
    /// The file descriptor opened before chroot remains valid. This only
    /// changes the stored path used by future SIGHUP reopens.
    pub fn remap_after_chroot(&mut self, jail_dir: &Path) {
        if let LogTarget::File { path, .. } = &mut self.target {
            if let Some(in_jail_path) = path_after_chroot(path, jail_dir) {
                *path = in_jail_path;
            }
        }
    }

    /// True when there is no file/stdout sink to write to.
    pub fn is_file_backed(&self) -> bool {
        matches!(self.target, LogTarget::File { .. })
    }

    /// True when the access log targets stdout (`-l -`). Daemonization must
    /// preserve fd 1 in this case: legacy thttpd only redirects stdout to
    /// /dev/null when the log target is *not* stdout, otherwise every access
    /// log line would be lost in daemon mode.
    pub fn is_stdout(&self) -> bool {
        matches!(self.target, LogTarget::Stdout)
    }

    /// Reopen file-backed targets (SIGHUP). Stdout/syslog/disabled targets
    /// are left untouched, matching C's `re_open_logfile`.
    pub fn reopen(&mut self) -> io::Result<()> {
        if let LogTarget::File { path, .. } = &self.target {
            let file = OpenOptions::new().create(true).append(true).open(path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
            }
            self.target = LogTarget::File {
                path: path.clone(),
                file,
            };
        }
        Ok(())
    }

    /// Write a single CERN-format log line.
    pub fn log_request(&mut self, entry: &LogEntry<'_>) {
        let line = format_cern_line(entry);
        match &mut self.target {
            LogTarget::File { file, .. } => {
                let _ = file.write_all(line.as_bytes());
                let _ = file.flush();
            }
            LogTarget::Stdout => {
                let _ = io::stdout().write_all(line.as_bytes());
                let _ = io::stdout().flush();
            }
            LogTarget::Syslog | LogTarget::Disabled => {}
        }
    }
}

#[cfg(unix)]
fn configure_logfile(file: &std::fs::File, config: &ServerConfig) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    if unsafe { libc::geteuid() } == 0 {
        if let Some((uid, gid)) = configured_owner(config)? {
            use std::os::fd::AsRawFd;
            if unsafe { libc::fchown(file.as_raw_fd(), uid, gid) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn configure_logfile(_file: &std::fs::File, _config: &ServerConfig) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn configured_owner(config: &ServerConfig) -> io::Result<Option<(libc::uid_t, libc::gid_t)>> {
    let Some(username) = &config.user else {
        return Ok(None);
    };
    let user = nix::unistd::User::from_name(username)
        .map_err(io::Error::other)?
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("user '{username}' not found"),
            )
        })?;
    Ok(Some((user.uid.as_raw(), user.gid.as_raw())))
}

fn path_after_chroot(path: &Path, jail_dir: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let suffix = path.strip_prefix(jail_dir).ok()?;
    if suffix.as_os_str().is_empty() {
        Some(PathBuf::from("/"))
    } else {
        Some(PathBuf::from("/").join(suffix))
    }
}

/// Format a CERN Combined Log line, matching libhttpd.c:3936:
/// `%.80s - %.80s [%s] "%.80s %.300s %.80s" %d %s "%.200s" "%.200s"\n`
pub fn format_cern_line(entry: &LogEntry<'_>) -> String {
    let user = if entry.remote_user.is_empty() {
        "-"
    } else {
        entry.remote_user
    };
    let bytes = if entry.bytes_sent >= 0 {
        entry.bytes_sent.to_string()
    } else {
        "-".to_string()
    };
    let date = cern_localtime();
    format!(
        "{} - {} [{}] \"{} {} {}\" {} {} \"{}\" \"{}\"\n",
        truncate(entry.remote_addr, 80),
        truncate(user, 80),
        date,
        truncate(entry.method, 80),
        truncate(entry.url, 300),
        truncate(entry.protocol, 80),
        entry.status,
        bytes,
        truncate(entry.referer, 200),
        truncate(entry.user_agent, 200),
    )
}

/// Truncate to at most `n` chars, matching C's `%.Ns` precision.
fn truncate(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

/// Format the current local time as `dd/Mon/YYYY:HH:MM:SS +ZZZZ` (CERN).
fn cern_localtime() -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    #[cfg(unix)]
    {
        // SAFETY: localtime_r writes into the caller-provided buffer and is
        // thread-safe; we pass a valid mutable tm pointer. This is the only
        // portable way to obtain tm_gmtoff for the numeric timezone.
        unsafe {
            let now: libc::time_t = libc::time(std::ptr::null_mut());
            let mut tm: libc::tm = std::mem::zeroed();
            libc::localtime_r(&now, &mut tm);
            let mut zone = tm.tm_gmtoff / 60;
            let sign = if zone >= 0 { '+' } else { '-' };
            if zone < 0 {
                zone = -zone;
            }
            zone = (zone / 60) * 100 + zone % 60;
            format!(
                "{:02}/{}/{:04}:{:02}:{:02}:{:02} {}{:04}",
                tm.tm_mday,
                MONTHS[(tm.tm_mon as usize).min(11)],
                tm.tm_year + 1900,
                tm.tm_hour,
                tm.tm_min,
                tm.tm_sec,
                sign,
                zone,
            )
        }
    }
    #[cfg(not(unix))]
    {
        // Fallback: no numeric timezone available without libc.
        format!("01/Jan/1970:00:00:00 +0000")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry<'a>() -> LogEntry<'a> {
        LogEntry {
            remote_addr: "127.0.0.1",
            remote_user: "",
            method: "GET",
            url: "/index.html",
            protocol: "HTTP/1.0",
            status: 200,
            bytes_sent: 1234,
            referer: "",
            user_agent: "test-agent",
        }
    }

    #[test]
    fn cern_line_format_matches_legacy_layout() {
        let line = format_cern_line(&entry());
        // Field order and quoting must match libhttpd.c:3936.
        assert!(line.contains("127.0.0.1 - - ["), "addr and user: {line}");
        assert!(
            line.contains("] \"GET /index.html HTTP/1.0\" 200 1234"),
            "{line}"
        );
        assert!(
            line.contains("\"\" \"test-agent\""),
            "empty referer + user agent: {line}"
        );
        assert!(line.ends_with('\n'));
    }

    #[test]
    fn truncate_matches_precision() {
        assert_eq!(truncate("abcdefgh", 3), "abc");
        assert_eq!(truncate("ab", 5), "ab");
        assert_eq!(truncate("", 5), "");
    }

    #[test]
    fn file_target_round_trip_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("access.log");
        let cfg = ServerConfig {
            logfile: Some(path.clone()),
            ..ServerConfig::default()
        };
        let mut logger = AccessLogger::open(&cfg).unwrap();
        assert!(logger.is_file_backed());
        logger.log_request(&entry());
        // Rename the file (log rotation) then reopen on SIGHUP.
        let rotated = dir.path().join("access.log.1");
        std::fs::rename(&path, &rotated).unwrap();
        logger.reopen().unwrap();
        logger.log_request(&entry());

        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents.lines().count(), 1, "only post-rotate line present");
        let rotated_contents = std::fs::read_to_string(&rotated).unwrap();
        assert_eq!(
            rotated_contents.lines().count(),
            1,
            "pre-rotate line preserved"
        );
    }

    #[test]
    fn chrooted_logfile_reopen_path_is_remapped_inside_jail() {
        let dir = tempfile::tempdir().unwrap();
        let jail = dir.path().join("www");
        let log_dir = jail.join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();
        let path = log_dir.join("access.log");
        let cfg = ServerConfig {
            logfile: Some(path),
            ..ServerConfig::default()
        };
        let mut logger = AccessLogger::open(&cfg).unwrap();

        logger.remap_after_chroot(&jail);

        match logger.target {
            LogTarget::File { ref path, .. } => assert_eq!(path, Path::new("/logs/access.log")),
            _ => panic!("expected file log target"),
        }
    }

    #[test]
    fn chroot_remap_leaves_external_logfile_path_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let jail = dir.path().join("www");
        let external = dir.path().join("access.log");
        std::fs::create_dir_all(&jail).unwrap();
        let cfg = ServerConfig {
            logfile: Some(external.clone()),
            ..ServerConfig::default()
        };
        let mut logger = AccessLogger::open(&cfg).unwrap();

        logger.remap_after_chroot(&jail);

        match logger.target {
            LogTarget::File { ref path, .. } => assert_eq!(path, &external),
            _ => panic!("expected file log target"),
        }
    }

    #[test]
    fn dev_null_disables_logging() {
        let cfg = ServerConfig {
            logfile: Some(std::path::PathBuf::from("/dev/null")),
            ..ServerConfig::default()
        };
        let mut logger = AccessLogger::open(&cfg).unwrap();
        assert!(!logger.is_file_backed());
        // Must not panic and must not write.
        logger.log_request(&entry());
    }

    #[cfg(unix)]
    #[test]
    fn reopen_failure_preserves_old_fd() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("access.log");
        let cfg = ServerConfig {
            logfile: Some(path.clone()),
            ..ServerConfig::default()
        };
        let mut logger = AccessLogger::open(&cfg).unwrap();
        logger.log_request(&entry());

        // Drop the tempdir entirely. On Unix the held fd keeps the inode
        // alive so the old writes still land, but the path is gone so
        // reopen's create(true) fails (parent dir no longer exists).
        drop(dir);

        // reopen() must return Err — the path is unreachable.
        let result = logger.reopen();
        assert!(result.is_err(), "reopen should fail when path is gone");

        // The old fd is preserved: log_request still works without panic.
        // On Unix the unlinked inode is still writable through the held fd.
        logger.log_request(&entry());
    }
    #[test]
    fn dash_selects_stdout_target() {
        let cfg = ServerConfig {
            logfile: Some(std::path::PathBuf::from("-")),
            ..ServerConfig::default()
        };
        let logger = AccessLogger::open(&cfg).unwrap();
        assert!(!logger.is_file_backed());
        assert!(matches!(logger.target, LogTarget::Stdout));
    }
}
