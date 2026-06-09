//! Startup sequence for thttpd.
//! Security-critical ordering: chroot → bind → setuid.
//! Translates `legacy/src/thttpd.c:234-327`.

use crate::config::ServerConfig;
use mio::net::TcpListener;
use std::net::TcpListener as StdTcpListener;

/// Bind listen sockets (IPv4 + optionally IPv6).
pub fn bind_listeners(config: &ServerConfig) -> std::io::Result<Vec<TcpListener>> {
    let addr = format!("{}:{}", config.hostname.as_deref().unwrap_or("0.0.0.0"), config.port);
    let std_listener = StdTcpListener::bind(&addr)?;
    std_listener.set_nonblocking(true)?;
    let listener = TcpListener::from_std(std_listener);
    Ok(vec![listener])
}

/// Perform chroot if configured.
pub fn do_chroot(config: &ServerConfig) -> Result<(), String> {
    if !config.do_chroot {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::path::Path;
        let dir = Path::new(&config.dir);
        if let Err(e) = nix::unistd::chroot(dir) {
            return Err(format!("chroot failed: {e}"));
        }
        if let Err(e) = nix::unistd::chdir("/") {
            return Err(format!("chdir after chroot failed: {e}"));
        }
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
            nix::unistd::setgid(pwd.gid)
                .map_err(|e| format!("setgid: {e}"))?;
            let c_username = CString::new(username.as_str())
                .map_err(|e| format!("invalid username: {e}"))?;
            nix::unistd::initgroups(&c_username, pwd.gid)
                .map_err(|e| format!("initgroups: {e}"))?;
            nix::unistd::setuid(pwd.uid)
                .map_err(|e| format!("setuid: {e}"))?;
        }
        #[cfg(not(unix))]
        {
            let _ = username;
        }
    }
    Ok(())
}
