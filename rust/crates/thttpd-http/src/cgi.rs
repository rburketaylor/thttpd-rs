//! CGI execution for thttpd.
//! Translates `legacy/src/libhttpd.c:3322-3540` CGI fork/exec chain.
//! Uses `std::process::Command` with `Stdio::piped()` — eliminates interposer processes.

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// CGI execution context.
pub struct CgiContext {
    pub server_software: String,
    pub server_name: String,
    pub gateway_interface: String,
    pub server_protocol: String,
    pub server_port: u16,
    pub request_method: String,
    pub script_name: String,
    pub query_string: String,
    pub remote_addr: String,
    pub content_type: Option<String>,
    pub content_length: Option<i64>,
    pub http_headers: HashMap<String, String>,
    pub path_info: Option<String>,
    pub path_translated: Option<String>,
    pub remote_user: Option<String>,
    pub auth_type: Option<String>,
}

/// CGI execution result.
pub struct CgiResult {
    pub child: std::process::Child,
    pub is_nph: bool,
}

/// Check if a CGI script is an NPH (Non-Parsed-Headers) script.
pub fn is_nph_script(script_path: &str) -> bool {
    Path::new(script_path)
        .file_name()
        .and_then(|n| n.to_str())
        .map(|name| name.starts_with("nph-"))
        .unwrap_or(false)
}

/// Build the CGI environment variables in the exact order C's `make_envp()` uses.
/// Order matters for legacy CGI scripts.
pub fn build_envp(ctx: &CgiContext, script_path: &str, cgi_pattern: &str) -> Vec<(String, String)> {
    // Order must match C's make_envp() at libhttpd.c:3002-3081.
    let mut env = vec![
        (
            "PATH".to_string(),
            "/usr/local/bin:/usr/ucb:/bin:/usr/bin".to_string(),
        ),
        ("SERVER_SOFTWARE".to_string(), ctx.server_software.clone()),
        ("SERVER_NAME".to_string(), ctx.server_name.clone()),
        (
            "GATEWAY_INTERFACE".to_string(),
            ctx.gateway_interface.clone(),
        ),
        ("SERVER_PROTOCOL".to_string(), ctx.server_protocol.clone()),
        ("SERVER_PORT".to_string(), ctx.server_port.to_string()),
        ("REQUEST_METHOD".to_string(), ctx.request_method.clone()),
    ];

    if let Some(ref path_info) = ctx.path_info {
        env.push(("PATH_INFO".to_string(), path_info.clone()));
    }
    if let Some(ref path_translated) = ctx.path_translated {
        env.push(("PATH_TRANSLATED".to_string(), path_translated.clone()));
    }
    env.push(("SCRIPT_NAME".to_string(), script_path.to_string()));

    // QUERY_STRING only when non-empty
    if !ctx.query_string.is_empty() {
        env.push(("QUERY_STRING".to_string(), ctx.query_string.clone()));
    }

    env.push(("REMOTE_ADDR".to_string(), ctx.remote_addr.clone()));

    if let Some(ref auth_type) = ctx.auth_type {
        env.push(("AUTH_TYPE".to_string(), auth_type.clone()));
    }
    if let Some(ref remote_user) = ctx.remote_user {
        env.push(("REMOTE_USER".to_string(), remote_user.clone()));
    }

    // HTTP_* headers in C's fixed order
    let fixed_order = [
        "Referer",
        "User-Agent",
        "Accept",
        "Accept-Encoding",
        "Accept-Language",
        "Cookie",
        "Host",
    ];
    for header in &fixed_order {
        if let Some(value) = ctx.http_headers.get(*header) {
            let env_key = format!("HTTP_{}", header.to_uppercase().replace('-', "_"));
            env.push((env_key, value.clone()));
        }
    }

    if let Some(ref content_type) = ctx.content_type {
        env.push(("CONTENT_TYPE".to_string(), content_type.clone()));
    }
    if let Some(content_length) = ctx.content_length {
        env.push(("CONTENT_LENGTH".to_string(), content_length.to_string()));
    }

    // CGI_PATTERN always present
    env.push(("CGI_PATTERN".to_string(), cgi_pattern.to_string()));

    env
}

/// Execute a CGI script.
///
/// `working_dir` sets the child's current directory.  C thttpd `chdir()`s to
/// the script's parent directory before `execve()` (legacy/src/libhttpd.c:3497),
/// so a CGI that opens a relative path (e.g. `cat data.txt`) reads from its
/// own directory.  Pass `None` to inherit the server's cwd.
pub fn execute_cgi(
    script_path: &Path,
    working_dir: Option<&Path>,
    env: Vec<(String, String)>,
    post_body: Option<&[u8]>,
) -> std::io::Result<CgiResult> {
    let is_nph = is_nph_script(&script_path.to_string_lossy());
    let executable_path = if working_dir.is_some() && script_path.is_relative() {
        std::env::current_dir()?.join(script_path)
    } else {
        script_path.to_path_buf()
    };

    let mut cmd = Command::new(&executable_path);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped()) // capture stderr for error reporting
        .env_clear();

    if let Some(dir) = working_dir {
        cmd.current_dir(dir);
    }

    for (key, value) in env {
        cmd.env(key, value);
    }

    let mut child = cmd.spawn()?;

    // Write POST body to stdin if present, then close stdin.
    // MUST close stdin even when there is no body — otherwise the CGI child
    // (e.g. `cat`) blocks reading from stdin while we block reading its stdout,
    // producing a deadlock.
    if let Some(mut stdin) = child.stdin.take() {
        if let Some(body) = post_body {
            let _ = stdin.write_all(body);
        }
        // stdin is dropped here, closing the pipe and sending EOF to the child.
    }

    Ok(CgiResult { child, is_nph })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nph_detection() {
        assert!(is_nph_script("nph-test.cgi"));
        assert!(!is_nph_script("test.cgi"));
    }

    #[test]
    fn test_env_order() {
        let ctx = CgiContext {
            server_software: "thttpd/2.27".into(),
            server_name: "localhost".into(),
            gateway_interface: "CGI/1.1".into(),
            server_protocol: "HTTP/1.0".into(),
            server_port: 80,
            request_method: "GET".into(),
            script_name: "/test.cgi".into(),
            query_string: "".into(),
            remote_addr: "127.0.0.1".into(),
            content_type: None,
            content_length: None,
            http_headers: HashMap::new(),
            path_info: None,
            path_translated: None,
            remote_user: None,
            auth_type: None,
        };
        let env = build_envp(&ctx, "/test.cgi", "**.cgi");
        // PATH must come first (matching C's order)
        assert_eq!(env[0].0, "PATH");
        assert_eq!(env[0].1, "/usr/local/bin:/usr/ucb:/bin:/usr/bin");
    }

    #[cfg(unix)]
    fn make_executable(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn test_cgi_cwd_is_script_dir() {
        // A CGI that reads a relative path must read from its own directory —
        // C chdir()s to the script's parent before execve().
        let dir = tempfile::tempdir().unwrap();
        // A data file sitting next to the script.
        std::fs::write(dir.path().join("data.txt"), b"relative-data").unwrap();
        // Script cats the relative file (uses $PWD after chdir).
        let script = dir.path().join("show.cgi");
        std::fs::write(&script, b"#!/bin/sh\ncat data.txt\n").unwrap();
        make_executable(&script);

        let result = execute_cgi(&script, Some(dir.path()), Vec::new(), None).unwrap();
        let mut child = result.child;
        let mut out = String::new();
        use std::io::Read;
        if let Some(mut stdout) = child.stdout.take() {
            stdout.read_to_string(&mut out).unwrap();
        }
        let _ = child.wait();
        assert_eq!(out.trim(), "relative-data");
    }

    #[cfg(unix)]
    #[test]
    fn test_cgi_relative_script_path_survives_current_dir() {
        let cwd = std::env::current_dir().unwrap();
        let root = tempfile::tempdir_in(&cwd).unwrap();
        let cgi_dir = root.path().join("cgi-bin");
        std::fs::create_dir(&cgi_dir).unwrap();
        std::fs::write(cgi_dir.join("data.txt"), b"relative-data").unwrap();

        let script = cgi_dir.join("show.cgi");
        std::fs::write(&script, b"#!/bin/sh\ncat data.txt\n").unwrap();
        make_executable(&script);

        let relative_script = script.strip_prefix(&cwd).unwrap();
        let result = execute_cgi(relative_script, Some(&cgi_dir), Vec::new(), None).unwrap();
        let mut child = result.child;
        let mut out = String::new();
        use std::io::Read;
        if let Some(mut stdout) = child.stdout.take() {
            stdout.read_to_string(&mut out).unwrap();
        }
        let _ = child.wait();
        assert_eq!(out.trim(), "relative-data");
    }

    #[cfg(unix)]
    #[test]
    fn test_cgi_cwd_none_inherits() {
        // With working_dir = None, the child inherits the test process cwd.
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("pwd.cgi");
        std::fs::write(&script, b"#!/bin/sh\npwd\n").unwrap();
        make_executable(&script);

        let result = execute_cgi(&script, None, Vec::new(), None).unwrap();
        let mut child = result.child;
        let mut out = String::new();
        use std::io::Read;
        if let Some(mut stdout) = child.stdout.take() {
            stdout.read_to_string(&mut out).unwrap();
        }
        let _ = child.wait();
        // Inherited cwd is the test's cwd, NOT the script dir.
        assert_ne!(std::path::Path::new(out.trim()), dir.path());
    }

    #[cfg(unix)]
    #[test]
    fn test_cgi_post_body_reaches_stdin() {
        // A POST body written via execute_cgi must reach the CGI's stdin in
        // full, including a large body read incrementally by the child.  This
        // guards the stdin write-then-EOF path that lets a CGI drain a body
        // across multiple of its own read() calls.
        let dir = tempfile::tempdir().unwrap();
        // wc -c counts stdin bytes; read in small chunks internally via cat.
        let script = dir.path().join("echo.cgi");
        std::fs::write(&script, b"#!/bin/sh\nwc -c\n").unwrap();
        make_executable(&script);

        let body = b"X".repeat(50_000); // large enough to span multiple pipe reads
        let result = execute_cgi(&script, Some(dir.path()), Vec::new(), Some(&body)).unwrap();
        let mut child = result.child;
        let mut out = String::new();
        use std::io::Read;
        if let Some(mut stdout) = child.stdout.take() {
            stdout.read_to_string(&mut out).unwrap();
        }
        let _ = child.wait();
        assert_eq!(out.trim(), "50000");
    }
}
