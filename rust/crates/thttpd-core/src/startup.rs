//! Startup sequence for thttpd.
//! Security-critical ordering: chroot → bind → setuid.
//! Translates `legacy/src/thttpd.c:234-327`.

use crate::config::ServerConfig;
use mio::net::TcpListener;
use std::net::TcpListener as StdTcpListener;

/// Bind listen sockets (IPv4 + IPv6 where supported).
///
/// With no `-h`, attempt `[::]:port` (IPv6, v6only) and `0.0.0.0:port`
/// (IPv4); continue if one family is unsupported (EAFNOSUPPORT/EPFNOSUPPORT)
/// but fail hard on any real bind error such as `AddrInUse` or
/// `PermissionDenied`, even if the other family bound successfully — matching
/// C's `lookup_hostname` which returns -1 unless `errno` is one of the two
/// family-not-supported codes. With `-h`, resolve every address and bind each
/// unique IPv4/IPv6 result under the same fatal-vs-ignorable rule.
pub fn bind_listeners(config: &ServerConfig) -> std::io::Result<Vec<TcpListener>> {
    let mut listeners = Vec::new();
    let mut fatal: Option<std::io::Error> = None;

    match &config.hostname {
        None => {
            // Try IPv6 first (v6only so it doesn't shadow the IPv4 bind), then IPv4.
            // Attempt both families even if one fails: an unsupported family is
            // silently skipped, but a real bind error (port in use, permission
            // denied) is fatal regardless of whether the other succeeded.
            if let Err(e) = try_bind_v6only(
                &std::net::Ipv6Addr::UNSPECIFIED,
                config.port,
                &mut listeners,
            ) {
                if !is_ignorable_bind_error(&e) {
                    fatal = Some(std::io::Error::new(
                        e.kind(),
                        format!("IPv6 [::]:{}: {e}", config.port),
                    ));
                }
            }
            if let Err(e) =
                try_bind_v4(std::net::Ipv4Addr::UNSPECIFIED, config.port, &mut listeners)
            {
                if !is_ignorable_bind_error(&e) {
                    fatal = Some(std::io::Error::new(
                        e.kind(),
                        format!("IPv4 0.0.0.0:{}: {e}", config.port),
                    ));
                }
            }
        }
        Some(host) => {
            let addrs = resolve_hostname(host, config.port);
            for addr in &addrs {
                let err = match addr {
                    std::net::SocketAddr::V4(v4) => {
                        try_bind_v4(*v4.ip(), v4.port(), &mut listeners).err()
                    }
                    std::net::SocketAddr::V6(v6) => {
                        try_bind_v6only(v6.ip(), v6.port(), &mut listeners).err()
                    }
                };
                if let Some(e) = err {
                    if !is_ignorable_bind_error(&e) {
                        fatal = Some(std::io::Error::new(e.kind(), format!("{addr}: {e}")));
                        break;
                    }
                }
            }
            if listeners.is_empty() && fatal.is_none() && addrs.is_empty() {
                return Err(std::io::Error::other(format!(
                    "could not resolve hostname '{host}'"
                )));
            }
        }
    }

    if let Some(e) = fatal {
        return Err(e);
    }
    if listeners.is_empty() {
        return Err(std::io::Error::other("no usable listen address"));
    }
    Ok(listeners)
}

/// Classify a bind error as ignorable (unsupported address family) or fatal.
///
/// Mirrors C's `lookup_hostname`, which treats `EAFNOSUPPORT` and
/// `EPFNOSUPPORT` as "this family isn't available, try the next one" but
/// fails hard on any other error (port in use, permission denied, etc.).
/// On systems without an IPv6 stack, `socket(AF_INET6, …)` returns one of
/// these codes, letting the caller fall back to IPv4.
fn is_ignorable_bind_error(e: &std::io::Error) -> bool {
    #[cfg(unix)]
    if let Some(raw) = e.raw_os_error() {
        return raw == libc::EAFNOSUPPORT || raw == libc::EPFNOSUPPORT;
    }
    // Non-Unix: no known ignorable bind errors; treat all failures as fatal.
    false
}

/// Resolve a hostname to socket addresses on the given port, preserving the
/// order `getaddrinfo` returns (IPv6 entries before IPv4, matching C).
fn resolve_hostname(host: &str, port: u16) -> Vec<std::net::SocketAddr> {
    use std::net::ToSocketAddrs;
    // If the host is already a literal IP, ToSocketAddrs still works and yields
    // a single address; otherwise the OS resolver is used.
    match (host, port).to_socket_addrs() {
        Ok(iter) => {
            // Dedup while preserving order so we don't double-bind.
            let mut seen = std::collections::HashSet::new();
            iter.filter(move |a| seen.insert(*a)).collect()
        }
        Err(_) => Vec::new(),
    }
}

fn try_bind_v4(
    ip: std::net::Ipv4Addr,
    port: u16,
    out: &mut Vec<TcpListener>,
) -> std::io::Result<()> {
    let std_listener = StdTcpListener::bind((ip, port))?;
    std_listener.set_nonblocking(true)?;
    out.push(TcpListener::from_std(std_listener));
    Ok(())
}

fn try_bind_v6only(
    ip: &std::net::Ipv6Addr,
    port: u16,
    out: &mut Vec<TcpListener>,
) -> std::io::Result<()> {
    // Build the socket with socket2 so we can set IPV6_V6ONLY before binding,
    // avoiding dual-stack conflicts with the IPv4 listener (C sets this too).
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::IPV6, Type::STREAM, Some(Protocol::TCP))?;
    #[cfg(unix)]
    let _ = socket.set_only_v6(true);
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&socket2::SockAddr::from(std::net::SocketAddr::new(
        std::net::IpAddr::V6(*ip),
        port,
    )))?;
    socket.listen(128)?;
    // Convert socket2 -> std -> mio.
    #[cfg(unix)]
    {
        use std::os::unix::io::{FromRawFd, IntoRawFd};
        let fd = socket.into_raw_fd();
        // SAFETY: into_raw_fd hands us ownership of a valid fd.
        let std_listener = unsafe { StdTcpListener::from_raw_fd(fd) };
        out.push(TcpListener::from_std(std_listener));
    }
    #[cfg(not(unix))]
    {
        use std::os::windows::io::{FromRawSocket, IntoRawSocket};
        let sock = socket.into_raw_socket();
        let std_listener = unsafe { StdTcpListener::from_raw_socket(sock) };
        out.push(TcpListener::from_std(std_listener));
    }
    Ok(())
}

/// Perform chroot if configured.
///
/// After `chroot(config.dir)` the original absolute path no longer resolves
/// inside the jail, so the serving root must move to `/`, matching
/// `legacy/src/thttpd.c:587` (`strcpy(cwd, "/")`). Rewriting `config.dir`
/// keeps request resolution (`config.dir.join(...)`) and the symlink
/// boundary check (`canonicalize(config.dir)`) correct within the new root.
pub fn do_chroot(config: &mut ServerConfig) -> Result<(), String> {
    if !config.do_chroot {
        return Ok(());
    }
    #[cfg(unix)]
    {
        let dir = config.dir.clone();
        if let Err(e) = nix::unistd::chroot(&dir) {
            return Err(format!("chroot failed: {e}"));
        }
        if let Err(e) = nix::unistd::chdir("/") {
            return Err(format!("chdir after chroot failed: {e}"));
        }
        config.dir = std::path::PathBuf::from("/");
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = config;
        Err("chroot not supported on this platform".to_string())
    }
}

/// Drop privileges to the configured user.
pub fn drop_privileges(config: &ServerConfig) -> Result<(), String> {
    if let Some(ref username) = config.user {
        #[cfg(unix)]
        {
            use std::ffi::CString;
            let pwd = nix::unistd::User::from_name(username)
                .map_err(|e| format!("User::from_name({username}): {e}"))?
                .ok_or_else(|| format!("user '{username}' not found"))?;
            nix::unistd::setgid(pwd.gid).map_err(|e| format!("setgid: {e}"))?;
            let c_username =
                CString::new(username.as_str()).map_err(|e| format!("invalid username: {e}"))?;
            // SAFETY: c_username is a valid NUL-terminated string and gid is
            // obtained from the system user database. libc returns 0 on success.
            #[cfg(target_os = "macos")]
            let raw_gid = pwd.gid.as_raw() as libc::c_int;
            #[cfg(not(target_os = "macos"))]
            let raw_gid = pwd.gid.as_raw();
            if unsafe { libc::initgroups(c_username.as_ptr(), raw_gid) } != 0 {
                return Err(format!("initgroups: {}", std::io::Error::last_os_error()));
            }
            nix::unistd::setuid(pwd.uid).map_err(|e| format!("setuid: {e}"))?;
        }
        #[cfg(not(unix))]
        {
            let _ = username;
        }
    }
    Ok(())
}

/// Write the configured pidfile while still root and before chroot, so a
/// conventional `/var/run/...pid` outside the chroot tree stays writable.
/// Mirrors `legacy/src/thttpd.c:533-544`, which precedes both chroot and
/// `setuid`.
pub fn write_pidfile(config: &ServerConfig) -> Result<(), String> {
    let Some(path) = &config.pidfile else {
        return Ok(());
    };
    std::fs::write(path, format!("{}\n", std::process::id()))
        .map_err(|error| format!("writing pidfile {}: {error}", path.display()))
}

/// Daemonize the process on Unix: fork, become a session leader (setsid),
/// fork again to shed the controlling terminal, and reopen stdio onto
/// `/dev/null`. Mirrors `daemon(1,1)` plus the second fork in
/// `legacy/src/thttpd.c:490-540`. On non-Unix targets this returns an error
/// so callers can surface a clear message.
/// Token returned by [`daemonize_with_handshake`]; signals startup status
/// to the original parent process via a pipe.
pub struct DaemonHandshake {
    #[cfg(unix)]
    write_fd: std::os::fd::OwnedFd,
    reported: bool,
}

impl DaemonHandshake {
    /// Signal successful startup. The original parent exits 0.
    /// Safe to call more than once (subsequent calls are no-ops).
    pub fn report_success(&mut self) {
        self.report(0);
    }

    fn report(&mut self, code: u8) {
        if !self.reported {
            #[cfg(unix)]
            let _ = nix::unistd::write(&self.write_fd, &[code]);
            #[cfg(not(unix))]
            let _ = code;
            self.reported = true;
        }
    }
}

impl Drop for DaemonHandshake {
    fn drop(&mut self) {
        // If we never explicitly reported status (e.g. the daemon panicked
        // or exited during startup), signal failure to the parent.
        #[cfg(unix)]
        if !self.reported {
            let _ = nix::unistd::write(&self.write_fd, &[1u8]);
        }
        // write_fd is closed by OwnedFd's Drop
    }
}

pub fn daemonize(keep_stdout: bool) -> Result<(), String> {
    #[cfg(unix)]
    {
        use nix::unistd::{ForkResult, dup2_stderr, dup2_stdin, dup2_stdout, fork, setsid};
        use std::os::fd::AsFd;

        // First fork: detach from the controlling terminal.
        // SAFETY: fork() is only unsafe for multithreaded programs, where
        // another thread may hold a lock across the fork. daemonize runs early
        // in startup, before the mio event loop or any helper thread exists, so
        // the process is single-threaded here and the hazard does not apply.
        match unsafe { fork() }.map_err(|e| format!("first fork: {e}"))? {
            ForkResult::Parent { child } => {
                let _ = child;
                std::process::exit(0);
            }
            ForkResult::Child => {}
        }
        setsid().map_err(|e| format!("setsid: {e}"))?;

        // Second fork: guarantee we cannot reacquire a controlling terminal.
        // SAFETY: still single-threaded between forks (setsid does not spawn
        // threads), so the same fork-safety justification holds.
        match unsafe { fork() }.map_err(|e| format!("second fork: {e}"))? {
            ForkResult::Parent { child } => {
                let _ = child;
                std::process::exit(0);
            }
            ForkResult::Child => {}
        }

        // Reopen stdio onto /dev/null so the daemon never blocks on a tty
        // and never leaks the parent's fds. Matches thttpd.c:493-499.
        let devnull = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/null")
            .map_err(|e| format!("opening /dev/null: {e}"))?;
        let fd = devnull.as_fd();
        dup2_stdin(fd).map_err(|e| format!("dup2 stdin: {e}"))?;
        // Legacy thttpd redirects stdout to /dev/null only when the access
        // log does not target it. With `-l -` the daemon must keep the
        // original stdout or every access-log line disappears.
        if !keep_stdout {
            dup2_stdout(fd).map_err(|e| format!("dup2 stdout: {e}"))?;
        }
        dup2_stderr(fd).map_err(|e| format!("dup2 stderr: {e}"))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        Err("daemonize not supported on this platform".to_string())
    }
}

/// Daemonize with a parent-child status handshake.
///
/// Like [`daemonize`], but the original parent blocks on a pipe until the
/// grandchild either calls [`DaemonHandshake::report_success`] or drops the
/// handshake (signalling failure).  This lets the caller surface daemon
/// startup failures: run all fallible startup *before* daemonizing, then
/// report success right before entering the event loop.
#[cfg(unix)]
pub fn daemonize_with_handshake(keep_stdout: bool) -> Result<DaemonHandshake, String> {
    use nix::unistd::{ForkResult, dup2_stderr, dup2_stdin, dup2_stdout, fork, setsid};
    use std::os::fd::AsFd;

    let (read_fd, write_fd) = nix::unistd::pipe().map_err(|e| format!("pipe: {e}"))?;

    // First fork: detach from the controlling terminal.
    // SAFETY: fork() is only unsafe for multithreaded programs, where
    // another thread may hold a lock across the fork. daemonize runs early
    // in startup, before the mio event loop or any helper thread exists, so
    // the process is single-threaded here and the hazard does not apply.
    match unsafe { fork() }.map_err(|e| format!("first fork: {e}"))? {
        ForkResult::Parent { .. } => {
            // Close write end (so the pipe breaks when the child exits).
            drop(write_fd);
            let mut buf = [0u8; 1];
            let status = match nix::unistd::read(&read_fd, &mut buf) {
                Ok(1) => buf[0],
                _ => 1,
            };
            std::process::exit(status as i32);
        }
        ForkResult::Child => {
            drop(read_fd);
        }
    }
    setsid().map_err(|e| format!("setsid: {e}"))?;

    // Second fork: guarantee we cannot reacquire a controlling terminal.
    // SAFETY: still single-threaded between forks (setsid does not spawn
    // threads), so the same fork-safety justification holds.
    match unsafe { fork() }.map_err(|e| format!("second fork: {e}"))? {
        ForkResult::Parent { .. } => {
            std::process::exit(0);
        }
        ForkResult::Child => {}
    }

    // Reopen stdio onto /dev/null (thttpd.c:493-499).
    let devnull = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
        .map_err(|e| format!("opening /dev/null: {e}"))?;
    let fd = devnull.as_fd();
    dup2_stdin(fd).map_err(|e| format!("dup2 stdin: {e}"))?;
    // Legacy thttpd redirects stdout to /dev/null only when the access log
    // does not target it. With `-l -` the daemon must keep the original
    // stdout (AccessLogger writes via io::stdout()) or every access-log line
    // disappears.
    if !keep_stdout {
        dup2_stdout(fd).map_err(|e| format!("dup2 stdout: {e}"))?;
    }
    dup2_stderr(fd).map_err(|e| format!("dup2 stderr: {e}"))?;

    Ok(DaemonHandshake {
        write_fd,
        reported: false,
    })
}

#[cfg(not(unix))]
pub fn daemonize_with_handshake(_keep_stdout: bool) -> Result<DaemonHandshake, String> {
    Err("daemonize not supported on this platform".to_string())
}

/// Apply `data_dir`: chdir into it so relative serving paths and CGI working
/// directories resolve correctly. Called after chroot (thttpd.c:598-606).
///
/// After a successful chdir, the effective serving root (`config.dir`) is
/// recomputed via [`effective_dir_after_chdir`]:
/// - Absolute `data_dir` becomes the serving root.
/// - Relative `data_dir` under chroot resolves under the jail root (`/data_dir`),
///   since the chdir left cwd at `/` before descending into `data_dir`.
/// - Relative `data_dir` without chroot: when the configured serving dir is
///   absolute it resolves under it; otherwise the root is `.` (cwd), since the
///   chdir already moved into `config_dir`/`data_dir`.
pub fn apply_data_dir(config: &mut ServerConfig) -> Result<(), String> {
    let Some(data_dir) = config.data_dir.clone() else {
        return Ok(());
    };
    #[cfg(unix)]
    {
        let chdir_to = data_dir_chdir_path(&config.dir, &data_dir, config.do_chroot);
        if let Err(e) = nix::unistd::chdir(&chdir_to) {
            return Err(format!("data_dir chdir {}: {e}", chdir_to.display()));
        }
        config.dir = effective_dir_after_chdir(&config.dir, &data_dir, config.do_chroot);
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = config;
        Err("data_dir not supported on this platform".to_string())
    }
}

/// Resolve the effective serving root after chdir'ing into `data_dir`.
///
/// Extracted from [`apply_data_dir`] so the path-selection logic is unit
/// testable without the process-global `chdir` side effect.
fn effective_dir_after_chdir(
    config_dir: &std::path::Path,
    data_dir: &std::path::Path,
    do_chroot: bool,
) -> std::path::PathBuf {
    if data_dir.is_absolute() {
        data_dir.to_path_buf()
    } else if do_chroot {
        // After chroot the cwd is `/`; a relative data_dir descends from the
        // jail root, so e.g. "htdocs" resolves to "/htdocs".
        std::path::PathBuf::from("/").join(data_dir)
    } else if config_dir.is_absolute() {
        config_dir.join(data_dir)
    } else {
        // The chdir above landed in config_dir/data_dir, so the new cwd is the
        // serving root; a relative path would re-resolve under it (htdocs/htdocs).
        std::path::PathBuf::from(".")
    }
}

fn data_dir_chdir_path(
    config_dir: &std::path::Path,
    data_dir: &std::path::Path,
    do_chroot: bool,
) -> std::path::PathBuf {
    if data_dir.is_absolute() {
        data_dir.to_path_buf()
    } else if do_chroot {
        std::path::PathBuf::from("/").join(data_dir)
    } else {
        config_dir.join(data_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::path::{Path, PathBuf};

    #[test]
    fn wildcard_bind_fails_when_ipv4_port_is_in_use() {
        // Reserve an IPv4 loopback port so 0.0.0.0:<port> will collide.
        // The IPv6 [::]:<port> bind (v6only) typically succeeds since it's a
        // separate address space, proving the fix: startup must fail even
        // though one family bound successfully.
        let reserved = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = reserved.local_addr().unwrap().port();
        let config = ServerConfig {
            port,
            hostname: None,
            ..ServerConfig::default()
        };

        let result = bind_listeners(&config);
        assert!(
            result.is_err(),
            "bind_listeners must fail when a real bind error occurs on one family"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("IPv4"),
            "error should identify the failing family: {msg}"
        );
    }

    #[test]
    fn wildcard_bind_fails_with_addrinuse_on_both_families() {
        // When the port is taken on both families (v6only bound externally),
        // startup must fail rather than silently succeeding.
        let v6 = TcpListener::bind("[::1]:0").unwrap();
        let v4 = TcpListener::bind("127.0.0.1:0").unwrap();
        // These may get different ports; use the v4 port which is the one
        // guaranteed to collide for 0.0.0.0.
        let port = v4.local_addr().unwrap().port();
        let config = ServerConfig {
            port,
            hostname: None,
            ..ServerConfig::default()
        };

        let result = bind_listeners(&config);
        assert!(result.is_err());
        drop(v6);
        drop(v4);
    }

    #[test]
    fn addr_in_use_is_not_ignorable() {
        let e = std::io::Error::from(std::io::ErrorKind::AddrInUse);
        assert!(!is_ignorable_bind_error(&e));
    }

    #[test]
    fn permission_denied_is_not_ignorable() {
        let e = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert!(!is_ignorable_bind_error(&e));
    }

    #[test]
    #[cfg(unix)]
    fn address_family_not_supported_is_ignorable() {
        // EAFNOSUPPORT: the OS lacks the requested address family (e.g. no
        // IPv6 stack). This must be ignorable so the caller falls back to IPv4.
        let e = std::io::Error::from_raw_os_error(libc::EAFNOSUPPORT);
        assert!(is_ignorable_bind_error(&e));
    }

    #[test]
    #[cfg(unix)]
    fn epfnosupport_is_ignorable() {
        let e = std::io::Error::from_raw_os_error(libc::EPFNOSUPPORT);
        assert!(is_ignorable_bind_error(&e));
    }

    #[test]
    fn wildcard_bind_succeeds_on_free_port() {
        // Sanity check: with no conflict, both families should bind and
        // bind_listeners returns Ok with at least one listener.
        let config = ServerConfig {
            port: 0, // let the OS pick a free port
            hostname: None,
            ..ServerConfig::default()
        };

        let result = bind_listeners(&config);
        assert!(result.is_ok(), "free port should bind: {:?}", result.err());
        let listeners = result.unwrap();
        assert!(!listeners.is_empty());
    }

    #[test]
    fn relative_data_dir_under_chroot_resolves_under_jail_root() {
        // After chroot, cwd is "/", so a relative data_dir descends from the
        // jail root and config.dir must point there for request resolution.
        let dir = effective_dir_after_chdir(Path::new("/"), Path::new("htdocs"), true);
        assert_eq!(dir, PathBuf::from("/htdocs"));
    }

    #[test]
    fn relative_data_dir_without_chroot_resolves_under_config_dir() {
        let original = PathBuf::from("/var/www");
        let dir = effective_dir_after_chdir(&original, Path::new("htdocs"), false);
        assert_eq!(dir, PathBuf::from("/var/www/htdocs"));
    }

    #[test]
    fn relative_config_dir_with_relative_data_dir_uses_cwd_as_root() {
        // dir="." + data_dir="htdocs": after chdir into htdocs the serving root
        // is "." (cwd), not "./htdocs" (which would resolve under htdocs/htdocs).
        let dir = effective_dir_after_chdir(Path::new("."), Path::new("htdocs"), false);
        assert_eq!(dir, PathBuf::from("."));
    }

    #[test]
    fn relative_data_dir_without_chroot_chdirs_under_config_dir() {
        let original = PathBuf::from("/var/www");
        let dir = data_dir_chdir_path(&original, Path::new("htdocs"), false);
        assert_eq!(dir, PathBuf::from("/var/www/htdocs"));
    }

    #[test]
    fn relative_data_dir_under_chroot_chdirs_from_jail_root() {
        let dir = data_dir_chdir_path(Path::new("/"), Path::new("htdocs"), true);
        assert_eq!(dir, PathBuf::from("/htdocs"));
    }

    #[test]
    fn absolute_data_dir_becomes_serving_root() {
        // An absolute data_dir is adopted as the serving root regardless of
        // chroot, matching legacy's effect on document resolution.
        let dir = effective_dir_after_chdir(Path::new("/"), Path::new("/srv/htdocs"), true);
        assert_eq!(dir, PathBuf::from("/srv/htdocs"));
    }
}
