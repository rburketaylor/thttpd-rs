//! Startup sequence for thttpd.
//! Security-critical ordering: chroot → bind → setuid.
//! Translates `legacy/src/thttpd.c:234-327`.

use crate::config::ServerConfig;
use mio::net::TcpListener;
use std::net::TcpListener as StdTcpListener;

/// Bind listen sockets (IPv4 + optionally IPv6).
pub fn bind_listeners(config: &ServerConfig) -> std::io::Result<Vec<TcpListener>> {
    let addr = format!(
        "{}:{}",
        config.hostname.as_deref().unwrap_or("0.0.0.0"),
        config.port
    );
    let std_listener = StdTcpListener::bind(&addr)?;
    std_listener.set_nonblocking(true)?;
    let listener = TcpListener::from_std(std_listener);
    Ok(vec![listener])
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
