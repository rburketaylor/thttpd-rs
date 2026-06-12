//! Main event loop for thttpd.
//! Translates `legacy/src/thttpd.c:537-609`.
//! New connections get priority over existing connection I/O.

use crate::connection::ConnSlot;
use crate::server::Server;
use std::io::{self, Read, Write};
use std::path::Path;
use thttpd_fdwatch::{conn_token, is_listen_token, slab_key_from_token, Events, Interest, Token};
use thttpd_http::conn::ConnState;
use thttpd_http::parse::{got_request, parse_method};
use thttpd_http::parse_state::GotRequest;
use thttpd_http::response::{build_full_response, error_page, ResponseBuilder};
use thttpd_http::Method;
use thttpd_http::url::{normalize_path, percent_decode};
use thttpd_match::match_pattern;
use thttpd_mime::figure_mime;
use std::time::Duration;

/// Maximum number of connections we accept.
const MAX_CONNECTIONS: usize = 4096;

/// Size of the read buffer per connection — matches C's 60000.
const READ_BUF_SIZE: usize = 60000;

/// Maximum URL length before returning 500 Internal Error (matches C behavior).
const MAX_URL_LENGTH: usize = 10000;

/// Run the main event loop until termination.
pub fn run(server: &mut Server) -> io::Result<()> {
    // Bind listen sockets
    let listeners = crate::startup::bind_listeners(&server.config)?;
    server.listeners = listeners;

    // Register listeners with poll
    for (i, listener) in server.listeners.iter_mut().enumerate() {
        let token = Token(i);
        server.poll.registry().register(
            listener,
            token,
            Interest::READABLE,
        )?;
    }

    let mut events = Events::with_capacity(1024);

    loop {
        // Check termination signal
        if crate::signal::got_terminate() {
            break;
        }

        // Calculate poll timeout from timer wheel
        let timeout = server.timers.next_deadline().unwrap_or(Duration::from_secs(60));

        // Poll for events
        match server.poll.poll(&mut events, Some(timeout)) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }

        // Phase 1: Accept new connections (priority over existing I/O)
        for event in &events {
            if is_listen_token(event.token()) {
                let listener_idx = event.token().0;
                if listener_idx < server.listeners.len() {
                    handle_accept(server, listener_idx)?;
                }
            }
        }

        // Phase 2: Process existing connection events
        for event in &events {
            if !is_listen_token(event.token()) {
                if let Some(slab_key) = slab_key_from_token(event.token()) {
                    handle_connection_event(server, slab_key)?;
                }
            }
        }

        // Run expired timers
        let mut ctx = thttpd_timers::TimerCtx;
        server.timers.run(&mut ctx);

        // Periodic mmc cleanup
        server.mmc.cleanup();

        // Process signal flags
        if crate::signal::got_hup() {
            // SIGHUP received — re-open log file (libhttpd.c:237-254).
            // C's re_open_logfile() closes the current log file handle and
            // reopens it (allowing log rotation). The Rust port currently
            // doesn't have a persistent log file — this is a no-op. When
            // full logging support is added, the reopen logic would go here.
            if let Some(ref logfile) = server.config.logfile {
                eprintln!("thttpd: SIGHUP — would reopen logfile {:?}", logfile);
            }
            crate::signal::clear_hup();
        }
    }

    Ok(())
}

/// Accept new connections from a listen socket.
fn handle_accept(server: &mut Server, listener_idx: usize) -> io::Result<()> {
    // Accept as many connections as available (edge-triggered friendliness)
    loop {
        let (stream, peer_addr) = match server.listeners[listener_idx].accept() {
            Ok(pair) => {
                if server.conns.len() >= MAX_CONNECTIONS {
                    eprintln!("thttpd: max connections reached, rejecting {pair:?}");
                    continue;
                }
                pair
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                // No more connections to accept
                break;
            }
            Err(e) => {
                eprintln!("thttpd: accept error: {e}");
                break;
            }
        };

        // Allocate a slab slot
        let mut conn_slot = ConnSlot::new();
        conn_slot.state = ConnState::Reading;
        conn_slot.stream = Some(stream);
        conn_slot.http = thttpd_http::HttpConn::new();
        conn_slot.peer_addr = Some(peer_addr);

        let slab_key = server.conns.insert(conn_slot);
        let token = conn_token(slab_key);

        // Register the stream with mio
        let stream = server.conns[slab_key].stream.as_mut().unwrap();
        if let Err(e) = server.poll.registry().register(
            stream,
            token,
            Interest::READABLE,
        ) {
            eprintln!("thttpd: failed to register connection: {e}");
            server.conns.remove(slab_key);
            continue;
        }

        server.stats.connections += 1;
    }

    Ok(())
}

/// Dispatch a connection event based on its current state.
fn handle_connection_event(server: &mut Server, slab_key: usize) -> io::Result<()> {
    // Check that the slab key is still valid
    if !server.conns.contains(slab_key) {
        return Ok(());
    }

    let state = server.conns[slab_key].state;

    match state {
        ConnState::Reading => handle_read(server, slab_key),
        ConnState::Sending => handle_send(server, slab_key),
        ConnState::Lingering => handle_linger(server, slab_key),
        ConnState::Pausing => {
            close_connection(server, slab_key);
            Ok(())
        }
        ConnState::Free => Ok(()),
    }
}

/// Read data from a connection and process the request.
fn handle_read(server: &mut Server, slab_key: usize) -> io::Result<()> {
    // Read into the http read buffer
    // Check buffer space first
    {
        let slot = &server.conns[slab_key];
        let buf_remaining = READ_BUF_SIZE - slot.http.read_idx;
        if buf_remaining == 0 {
            let user_agent = slot.http.user_agent.clone();
            let response = build_error_response(400, "Bad Request", "", Some(&user_agent));
            let slot = &mut server.conns[slab_key];
            slot.http.response = response;
            slot.http.response_len = slot.http.response.len();
            transition_to_sending(server, slab_key);
            return Ok(());
        }
    }

    let n = {
        let slot = &mut server.conns[slab_key];
        let stream = match slot.stream.as_mut() {
            Some(s) => s,
            None => {
                close_connection(server, slab_key);
                return Ok(());
            }
        };
        let http = &mut slot.http;

        match stream.read(&mut http.read_buf[http.read_idx..]) {
            Ok(0) => {
                close_connection(server, slab_key);
                return Ok(());
            }
            Ok(n) => n,
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                return Ok(());
            }
            Err(e) => {
                eprintln!("thttpd: read error on slot {slab_key}: {e}");
                close_connection(server, slab_key);
                return Ok(());
            }
        }
    };

    server.conns[slab_key].http.read_idx += n;

    // Run the request-detection FSM
    let (result, new_checked, new_state) = {
        let http = &server.conns[slab_key].http;
        got_request(&http.read_buf, http.checked_idx, http.read_idx, http.parse_state.clone())
    };

    {
        let http = &mut server.conns[slab_key].http;
        http.checked_idx = new_checked;
        http.parse_state = new_state;
    }

    match result {
        GotRequest::NoRequest => Ok(()),
        GotRequest::BadRequest => {
            let user_agent = server.conns[slab_key].http.user_agent.clone();
            let response = build_error_response(400, "Bad Request", "", Some(&user_agent));
            let slot = &mut server.conns[slab_key];
            slot.http.response = response;
            slot.http.response_len = slot.http.response.len();
            transition_to_sending(server, slab_key);
            Ok(())
        }
        GotRequest::GotRequest => {
            process_request(server, slab_key);
            Ok(())
        }
    }
}

/// Process a complete HTTP request.
fn process_request(server: &mut Server, slab_key: usize) {
    // Parse request line
    let (url_str, version_str, host_str, has_version) = {
        let slot = &server.conns[slab_key];
        let http = &slot.http;
        let buf = &http.read_buf[..http.checked_idx];

        let request_line_end = buf.iter().position(|&b| b == b'\r').unwrap_or(buf.len());
        let request_line = String::from_utf8_lossy(&buf[..request_line_end]);
        let mut parts = request_line.split_whitespace();

        let _method_str = parts.next().unwrap_or("GET");
        let url = parts.next().unwrap_or("/").to_string();
        let version = parts.next().map(|v| v.to_string());

        let header_start = buf.iter().position(|&b| b == b'\n').map(|p| p + 1).unwrap_or(0);
        let headers_bytes = &buf[header_start..];
        let host = extract_header(headers_bytes, "Host").unwrap_or_default();

        (url, version.clone().unwrap_or_else(|| "HTTP/0.9".to_string()), host, version.is_some())
    };

    // Parse method
    let method = {
        let slot = &server.conns[slab_key];
        parse_method(&slot.http.read_buf, slot.http.checked_idx)
    };

    // Update HttpConn fields
    {
        let slot = &mut server.conns[slab_key];
        slot.http.method = method;
        slot.http.http_version = version_str.clone();
        // C: if protocol is not "HTTP/1.0" (case-insensitive), set one_one=1
        // (libhttpd.c:1965). one_one requires a Host header.
        slot.http.one_one = has_version
            && !version_str.is_empty()
            && !version_str.eq_ignore_ascii_case("HTTP/1.0");
        slot.http.encoded_url = url_str.clone();
        slot.http.host = host_str;
        slot.http.mime_flag = has_version; // HTTP/0.9 when no version token

        slot.http.decoded_url = percent_decode(&url_str);

        if let Some(qpos) = slot.http.decoded_url.find('?') {
            slot.http.query = slot.http.decoded_url[qpos + 1..].to_string();
            slot.http.decoded_url.truncate(qpos);
        }
    }

    // one_one requires Host header (libhttpd.c:2250-2255). C returns 400 if
    // one_one is set but no Host header was provided.
    if server.conns[slab_key].http.one_one
        && server.conns[slab_key].http.host.is_empty()
    {
        let user_agent = server.conns[slab_key].http.user_agent.clone();
        let v = server.conns[slab_key].http.http_version.clone();
        let body = error_page(400, "Bad Request", "Your request has bad syntax or is inherently impossible to satisfy.\n", &v, Some(&user_agent));
        let http_ref = &server.conns[slab_key].http;
        let response = build_full_response(http_ref, 400, "Bad Request", "text/html", -1, 0, &[]);
        let full_response = if http_ref.mime_flag {
            let mut r = response;
            r.extend_from_slice(&body);
            r
        } else {
            body
        };
        let len = full_response.len();
        let slot = &mut server.conns[slab_key];
        slot.http.response = full_response;
        slot.http.response_len = len;
        transition_to_sending(server, slab_key);
        return;
    }

    // Unknown method → 501
    if server.conns[slab_key].http.method == Method::Unknown {
        let method_str = {
            let slot = &server.conns[slab_key];
            let buf = &slot.http.read_buf[..slot.http.checked_idx];
            let request_line_end = buf.iter().position(|&b| b == b'\r').unwrap_or(buf.len());
            let request_line = String::from_utf8_lossy(&buf[..request_line_end]);
            request_line.split_whitespace().next().unwrap_or("UNKNOWN").to_string()
        };
        let user_agent = server.conns[slab_key].http.user_agent.clone();
        let body = error_page(501, "Not Implemented", "The requested method '%.80s' is not implemented by this server.\n", &method_str, Some(&user_agent));
        let http_ref = &server.conns[slab_key].http;
        let response = build_full_response(http_ref, 501, "Not Implemented", "text/html", -1, 0, &[]);
        let full_response = if http_ref.mime_flag {
            let mut r = response;
            r.extend_from_slice(&body);
            r
        } else {
            body
        };
        let slot = &mut server.conns[slab_key];
        slot.http.response = full_response;
        slot.http.response_len = slot.http.response.len();
        transition_to_sending(server, slab_key);
        return;
    }

    // Parse request headers
    {
        let slot = &mut server.conns[slab_key];
        let buf = &slot.http.read_buf[..slot.http.checked_idx];
        let header_start = buf.iter().position(|&b| b == b'\n').map(|p| p + 1).unwrap_or(0);
        let headers_bytes = &buf[header_start..];

        // If-Modified-Since
        if let Some(ims_str) = extract_header(headers_bytes, "If-Modified-Since") {
            slot.http.if_modified_since = thttpd_tdate::parse_http_date(&ims_str);
        }

        // Range: bytes=N-M
        if let Some(range_str) = extract_header(headers_bytes, "Range") {
            if !range_str.contains(',') {
                if let Some(eq_pos) = range_str.find('=') {
                    let range_spec = &range_str[eq_pos + 1..];
                    if let Some(dash_pos) = range_spec.find('-') {
                        if dash_pos > 0 {
                            let first_str = &range_spec[..dash_pos];
                            if let Ok(first) = first_str.parse::<i64>() {
                                slot.http.got_range = true;
                                slot.http.first_byte_index = if first < 0 { 0 } else { first };
                                if dash_pos + 1 < range_spec.len() {
                                    let rest = &range_spec[dash_pos + 1..];
                                    if let Ok(last) = rest.parse::<i64>() {
                                        slot.http.last_byte_index = if last < 0 { -1 } else { last };
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Content-Type
        if let Some(ct) = extract_header(headers_bytes, "Content-Type") {
            slot.http.content_type = ct;
        }

        // Content-Length — reject negative values (C thttpd uses atol() which
        // returns -1 for "-1", and its default/contentlength == -1 means "none").
        if let Some(cl_str) = extract_header(headers_bytes, "Content-Length") {
            slot.http.content_length = cl_str.trim().parse::<i64>().ok().filter(|&v| v >= 0);
        }

        // User-Agent
        if let Some(ua) = extract_header(headers_bytes, "User-Agent") {
            slot.http.user_agent = ua;
        }

        // Referer
        if let Some(refr) = extract_header(headers_bytes, "Referer") {
            slot.http.referer = refr;
        }

        // Accept
        if let Some(acc) = extract_header(headers_bytes, "Accept") {
            slot.http.accept = acc;
        }

        // Accept-Encoding
        if let Some(ae) = extract_header(headers_bytes, "Accept-Encoding") {
            slot.http.accept_encoding = ae;
        }

        // Accept-Language
        if let Some(al) = extract_header(headers_bytes, "Accept-Language") {
            slot.http.accept_language = al;
        }

        // Cookie
        if let Some(ck) = extract_header(headers_bytes, "Cookie") {
            slot.http.cookie = ck;
        }

        // Authorization
        if let Some(auth) = extract_header(headers_bytes, "Authorization") {
            slot.http.authorization = auth;
        }

        // X-Forwarded-For — first IP is the original client (libhttpd.c:2210).
        // Used for CGI REMOTE_ADDR and log lines.
        if let Some(xff) = extract_header(headers_bytes, "X-Forwarded-For") {
            // XFF can be "client, proxy1, proxy2" — C takes the first one
            let first = xff.split(',').next().unwrap_or("").trim().to_string();
            if !first.is_empty() {
                slot.http.x_forwarded_for = first;
            }
        }
    }

    // URL length limit
    {
        let slot = &server.conns[slab_key];
        if slot.http.encoded_url.len() > MAX_URL_LENGTH {
            let user_agent = slot.http.user_agent.clone();
            let body = error_page(500, "Internal Error", "There was an unusual problem serving the requested URL '%.80s'.\n", &slot.http.encoded_url, Some(&user_agent));
            let http_ref = &server.conns[slab_key].http;
            let response = build_full_response(http_ref, 500, "Internal Error", "text/html", -1, 0, &[]);
            let full_response = if http_ref.mime_flag {
                let mut r = response;
                r.extend_from_slice(&body);
                r
            } else {
                body
            };
            let slot = &mut server.conns[slab_key];
            slot.http.response = full_response;
            slot.http.response_len = slot.http.response.len();
            transition_to_sending(server, slab_key);
            return;
        }
    }

    // Resolve the file path
    let file_path = {
        let slot = &server.conns[slab_key];
        let decoded = &slot.http.decoded_url;

        let normalized = match normalize_path(decoded) {
            Some(p) => p,
            None => {
                // normalize_path returns None for directory traversal (..)
                // Check for // separately (returns 400 Bad Request)
                let (status, title, form_msg) = if decoded.contains("//") {
                    (400, "Bad Request", "Your request has bad syntax or is inherently impossible to satisfy.\n")
                } else {
                    (404, "Not Found", "The requested URL '%.80s' was not found on this server.\n")
                };
                let user_agent = server.conns[slab_key].http.user_agent.clone();
                let body = error_page(status, title, form_msg, decoded, Some(&user_agent));
                let http_ref = &server.conns[slab_key].http;
                let response = build_full_response(http_ref, status, title, "text/html", -1, 0, &[]);
                let full_response = if http_ref.mime_flag {
                    let mut r = response;
                    r.extend_from_slice(&body);
                    r
                } else {
                    body
                };
                let slot = &mut server.conns[slab_key];
                slot.http.response = full_response;
                slot.http.response_len = slot.http.response.len();
                transition_to_sending(server, slab_key);
                return;
            }
        };

        let path = if normalized == "/" {
            server.config.dir.join("index.html")
        } else {
            let relative = &normalized[1..];
            server.config.dir.join(relative)
        };

        let slot = &mut server.conns[slab_key];
        slot.http.orig_filename = normalized;
        path
    };

    // --- Virtual hosting (libhttpd.c:1342-1421 vhost_map) ---
    // If vhost is enabled, prepend the hostname (lowercased) to orig_filename
    // AND re-derive file_path. This is a simple port that doesn't include
    // C's VHOST_DIRLEVELS subdirectory split (off by default).
    let file_path = if server.config.vhost
        && !server.conns[slab_key].http.host.is_empty()
    {
        let host_lower: String = server.conns[slab_key]
            .http.host
            .to_lowercase();
        let hostdir = host_lower;
        let new_orig = format!("/{}{}", hostdir, server.conns[slab_key].http.orig_filename);
        let slot = &mut server.conns[slab_key];
        slot.http.orig_filename = new_orig.clone();
        slot.http.vhost_dir = hostdir;
        // Re-derive file_path
        if new_orig == "/" {
            server.config.dir.join("index.html")
        } else {
            server.config.dir.join(&new_orig[1..])
        }
    } else {
        file_path
    };
    // --- PATH_INFO extraction (libhttpd.c:2240-2270) ---
    // Find the longest prefix of orig_filename that exists on disk.
    // The remainder is the PATH_INFO. We do this BEFORE the CGI check so
    // that the pathinfo-on-non-CGI 403 check below has the correct path_info.
    // We also update orig_filename to the resolved script path so that
    // dispatch_cgi uses the correct script.
    //
    // When vhost is enabled, skip this — vhost and PATH_INFO are mutually
    // exclusive. The vhost-prepended file_path is the authoritative file.
    {
        let vhost_active = server.config.vhost
            && !server.conns[slab_key].http.vhost_dir.is_empty();
        if !vhost_active {
            let orig = server.conns[slab_key].http.orig_filename.clone();
            let mut test_path = orig.clone();
            let mut extracted_pathinfo = String::new();
            loop {
                // Build the on-disk path for this test_path
                let full_path = if test_path == "/" {
                    server.config.dir.join("index.html")
                } else {
                    server.config.dir.join(&test_path[1..])
                };
                if full_path.exists() {
                    break;
                }
                if let Some(last_slash) = test_path.rfind('/') {
                    if last_slash == 0 {
                        // Reached root, no file found
                        break;
                    }
                    let stripped = &test_path[last_slash + 1..];
                    if extracted_pathinfo.is_empty() {
                        extracted_pathinfo = format!("/{}", stripped);
                    } else {
                        extracted_pathinfo = format!("/{}{}", stripped, extracted_pathinfo);
                    }
                    test_path = test_path[..last_slash].to_string();
                } else {
                    break;
                }
            }
            if !extracted_pathinfo.is_empty() {
                server.conns[slab_key].http.path_info = extracted_pathinfo;
                // Also update orig_filename to the resolved script path so
                // dispatch_cgi uses the correct script.
                server.conns[slab_key].http.orig_filename = test_path;
            }
        }
    }

    // Check CGI pattern — try progressively shorter prefixes to handle PATH_INFO
    let is_cgi = {
        let slot = &server.conns[slab_key];
        match &server.config.cgi_pattern {
            Some(pattern) => {
                let orig = &slot.http.orig_filename;
                // First try the full URL
                if match_pattern(pattern, orig) {
                    true
                } else {
                    // Try stripping path components from right (PATH_INFO extraction)
                    let mut test = orig.as_str();
                    let mut found_cgi = false;
                    while let Some(last_slash) = test.rfind('/') {
                        if last_slash == 0 {
                            break;
                        }
                        test = &test[..last_slash];
                        if match_pattern(pattern, test) {
                            found_cgi = true;
                            break;
                        }
                    }
                    found_cgi
                }
            }
            None => false,
        }
    };

    if is_cgi {
        dispatch_cgi(server, slab_key, &file_path);
        return;
    }

    // Pathinfo on non-CGI file → 403 (libhttpd.c:3801-3810).
    // If the pathinfo was extracted by the is_cgi check above, but the
    // file isn't actually CGI, return 403 here.
    if !server.conns[slab_key].http.path_info.is_empty() {
        let url = server.conns[slab_key].http.encoded_url.clone();
        let user_agent = server.conns[slab_key].http.user_agent.clone();
        let body = error_page(
            403,
            "Forbidden",
            "The requested URL '%.80s' resolves to a file plus CGI-style pathinfo, but the file is not a valid CGI file.\n",
            &url,
            Some(&user_agent),
        );
        let http_ref = &server.conns[slab_key].http;
        let response = build_full_response(http_ref, 403, "Forbidden", "text/html", -1, 0, &[]);
        let full_response = if http_ref.mime_flag { let mut r = response; r.extend_from_slice(&body); r } else { body };
        let slot = &mut server.conns[slab_key];
        slot.http.response = full_response;
        slot.http.response_len = slot.http.response.len();
        transition_to_sending(server, slab_key);
        return;
    }

    // Static file serving
    serve_static(server, slab_key, &file_path);
}

/// Serve a static file.
fn serve_static(server: &mut Server, slab_key: usize, file_path: &Path) {
    // Propagate the configured charset to the connection so response
    // builders can use it for Content-Type text/* responses.
    server.conns[slab_key].http.charset = server.config.charset.clone();

    // --- Symlink escape prevention ---
    let file_path = {
        let canonical_root = match std::fs::canonicalize(&server.config.dir) {
            Ok(p) => p,
            Err(_) => {
                let user_agent = server.conns[slab_key].http.user_agent.clone();
                let body = error_page(500, "Internal Error", "There was an unusual problem serving the requested URL '%.80s'.\n", &server.config.dir.to_string_lossy(), Some(&user_agent));
                let http_ref = &server.conns[slab_key].http;
                let response = build_full_response(http_ref, 500, "Internal Error", "text/html", -1, 0, &[]);
                let full_response = if http_ref.mime_flag { let mut r = response; r.extend_from_slice(&body); r } else { body };
                let slot = &mut server.conns[slab_key];
                slot.http.response = full_response;
                slot.http.response_len = slot.http.response.len();
                transition_to_sending(server, slab_key);
                return;
            }
        };
        match std::fs::canonicalize(file_path) {
            Ok(canonical) => {
                if !canonical.starts_with(&canonical_root) {
                    let url = server.conns[slab_key].http.encoded_url.clone();
                    let user_agent = server.conns[slab_key].http.user_agent.clone();
                    let body = error_page(403, "Forbidden", "The requested URL '%.80s' resolves to a file outside the permitted web server directory tree.\n", &url, Some(&user_agent));
                    let http_ref = &server.conns[slab_key].http;
                    let response = build_full_response(http_ref, 403, "Forbidden", "text/html", -1, 0, &[]);
                    let full_response = if http_ref.mime_flag { let mut r = response; r.extend_from_slice(&body); r } else { body };
                    let slot = &mut server.conns[slab_key];
                    slot.http.response = full_response;
                    slot.http.response_len = slot.http.response.len();
                    transition_to_sending(server, slab_key);
                    return;
                }
                canonical
            }
            Err(_) => file_path.to_path_buf()
        }
    };

    // --- Permission / existence check ---
    let metadata = match std::fs::metadata(&file_path) {
        Ok(m) => m,
        Err(e) => {
            let url = server.conns[slab_key].http.encoded_url.clone();
            // For very long filenames (ENAMETOOLONG) return 500, matching C behavior
            let (status, title) = if e.kind() == std::io::ErrorKind::NotFound {
                (404, "Not Found")
            } else if file_path.components().any(|c| {
                if let std::path::Component::Normal(os_str) = c {
                    os_str.len() > 255
                } else {
                    false
                }
            }) {
                (500, "Internal Error")
            } else {
                (403, "Forbidden")
            };
            let form_msg = if status == 500 {
                "There was an unusual problem serving the requested URL '%.80s'.\n"
            } else if status == 404 {
                "The requested URL '%.80s' was not found on this server.\n"
            } else {
                "The requested URL '%.80s' is not accessible.\n"
            };
            let user_agent = server.conns[slab_key].http.user_agent.clone();
            let body = error_page(status, title, form_msg, &url, Some(&user_agent));
            let http_ref = &server.conns[slab_key].http;
            let response = build_full_response(http_ref, status, title, "text/html", -1, 0, &[]);
            let full_response = if http_ref.mime_flag { let mut r = response; r.extend_from_slice(&body); r } else { body };
            let slot = &mut server.conns[slab_key];
            slot.http.response = full_response;
            slot.http.response_len = slot.http.response.len();
            transition_to_sending(server, slab_key);
            return;
        }
    };

    // --- Basic Auth check (libhttpd.c:3732-3773) ---
    // If a .htpasswd file exists in the directory tree containing the
    // requested file, require Basic Auth. Returns 401 if missing or wrong.
    {
        let (authorization, encoded_url) = {
            let slot = &server.conns[slab_key];
            (slot.http.authorization.clone(), slot.http.encoded_url.clone())
        };
        match thttpd_http::auth::auth_check2(&file_path, &authorization) {
            thttpd_http::auth::AuthResult::NoAuthFile | thttpd_http::auth::AuthResult::Ok => {
                // No auth required or auth successful — continue
            }
            thttpd_http::auth::AuthResult::Unauthorized => {
                // Send 401 Unauthorized with WWW-Authenticate: Basic realm="..."
                // Realm is the URL directory path (no leading slash), matching
                // C's `send_authenticate(hc, dirname)` where dirname is derived
                // from the relative expnfilename.
                let realm = encoded_url
                    .rsplit_once('/')
                    .map(|(dir, _)| dir.trim_start_matches('/'))
                    .unwrap_or("")
                    .to_string();
                let user_agent = server.conns[slab_key].http.user_agent.clone();
                let body = error_page(
                    401,
                    thttpd_http::auth::ERR_401_TITLE,
                    thttpd_http::auth::ERR_401_FORM,
                    &encoded_url,
                    Some(&user_agent),
                );
                let extra_headers: [(String, String); 1] = [(
                    "WWW-Authenticate".to_string(),
                    format!("Basic realm=\"{}\"", realm),
                )];
                let http_ref = &server.conns[slab_key].http;
                let response = build_full_response(
                    http_ref, 401, thttpd_http::auth::ERR_401_TITLE,
                    "text/html", -1, 0, &extra_headers,
                );
                let full_response = if http_ref.mime_flag {
                    let mut r = response;
                    r.extend_from_slice(&body);
                    r
                } else {
                    body
                };
                let slot = &mut server.conns[slab_key];
                slot.http.response = full_response;
                slot.http.response_len = slot.http.response.len();
                transition_to_sending(server, slab_key);
                return;
            }
        }
    }

    // Directory listing
    if metadata.is_dir() {
        let url_path = server.conns[slab_key].http.orig_filename.clone();
        let dir = file_path.to_path_buf();
        let mtime = metadata.modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        match thttpd_http::dirlist::generate_listing(&dir, &url_path) {
            Ok(body) => {
                let http_ref = &server.conns[slab_key].http;
                let response = build_full_response(http_ref, 200, "OK", "text/html", -1, mtime, &[]);
                let full_response = if http_ref.mime_flag { let mut r = response; r.extend_from_slice(&body); r } else { body };
                let slot = &mut server.conns[slab_key];
                slot.http.response = full_response;
                slot.http.response_len = slot.http.response.len();
                transition_to_sending(server, slab_key);
                return;
            }
            Err(e) => {
                eprintln!("thttpd: directory listing error: {e}");
                let user_agent = server.conns[slab_key].http.user_agent.clone();
                let body = error_page(500, "Internal Error", "There was an unusual problem serving the requested URL '%.80s'.\n", &file_path.to_string_lossy(), Some(&user_agent));
                let http_ref = &server.conns[slab_key].http;
                let response = build_full_response(http_ref, 500, "Internal Error", "text/html", -1, 0, &[]);
                let full_response = if http_ref.mime_flag { let mut r = response; r.extend_from_slice(&body); r } else { body };
                let slot = &mut server.conns[slab_key];
                slot.http.response = full_response;
                slot.http.response_len = slot.http.response.len();
                transition_to_sending(server, slab_key);
                return;
            }
        }
    }

    // Check world-readable permission (Unix mode bits)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode();
        if (mode & 0o004) == 0 {
            let url = server.conns[slab_key].http.encoded_url.clone();
            let user_agent = server.conns[slab_key].http.user_agent.clone();
            let body = error_page(403, "Forbidden", "The requested URL '%.80s' resolves to a file that is not world-readable.\n", &url, Some(&user_agent));
            let http_ref = &server.conns[slab_key].http;
            let response = build_full_response(http_ref, 403, "Forbidden", "text/html", -1, 0, &[]);
            let full_response = if http_ref.mime_flag { let mut r = response; r.extend_from_slice(&body); r } else { body };
            let slot = &mut server.conns[slab_key];
            slot.http.response = full_response;
            slot.http.response_len = slot.http.response.len();
            transition_to_sending(server, slab_key);
            return;
        }

        // Non-CGI executable file → 403 (libhttpd.c:3790-3799).
        // The CGI check happens in process_request, so we only get here
        // for non-CGI files. If the file is world-executable but not in
        // the CGI pattern, C returns 403.
        if (mode & 0o001) != 0 {
            let url = server.conns[slab_key].http.encoded_url.clone();
            let user_agent = server.conns[slab_key].http.user_agent.clone();
            let body = error_page(
                403,
                "Forbidden",
                "The requested URL '%.80s' resolves to a file which is marked executable but is not a CGI file; retrieving it is forbidden.\n",
                &url,
                Some(&user_agent),
            );
            let http_ref = &server.conns[slab_key].http;
            let response = build_full_response(http_ref, 403, "Forbidden", "text/html", -1, 0, &[]);
            let full_response = if http_ref.mime_flag { let mut r = response; r.extend_from_slice(&body); r } else { body };
            let slot = &mut server.conns[slab_key];
            slot.http.response = full_response;
            slot.http.response_len = slot.http.response.len();
            transition_to_sending(server, slab_key);
            return;
        }
    }

    // Pathinfo on non-CGI file → 403 (libhttpd.c:3801-3810).
    // If the request had pathinfo but the file is not CGI, C rejects it.
    if !server.conns[slab_key].http.path_info.is_empty() {
        let url = server.conns[slab_key].http.encoded_url.clone();
        let user_agent = server.conns[slab_key].http.user_agent.clone();
        let body = error_page(
            403,
            "Forbidden",
            "The requested URL '%.80s' resolves to a file plus CGI-style pathinfo, but the file is not a valid CGI file.\n",
            &url,
            Some(&user_agent),
        );
        let http_ref = &server.conns[slab_key].http;
        let response = build_full_response(http_ref, 403, "Forbidden", "text/html", -1, 0, &[]);
        let full_response = if http_ref.mime_flag { let mut r = response; r.extend_from_slice(&body); r } else { body };
        let slot = &mut server.conns[slab_key];
        slot.http.response = full_response;
        slot.http.response_len = slot.http.response.len();
        transition_to_sending(server, slab_key);
        return;
    }

    let file_size = metadata.len() as i64;
    let file_mtime = metadata.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    // Fill in last_byte_index if needed
    {
        let slot = &mut server.conns[slab_key];
        if slot.http.got_range {
            if slot.http.last_byte_index == -1 || slot.http.last_byte_index >= file_size {
                slot.http.last_byte_index = file_size - 1;
            }
        }
    }

    let method = server.conns[slab_key].http.method;

    // --- HEAD: headers with Content-Length but no body ---
    if method == Method::Head {
        let http_ref = &server.conns[slab_key].http;
        let filename = file_path.to_string_lossy();
        let mime_info = figure_mime(&filename);
        let content_type = mime_info.mime_type;
        let extra_headers: Vec<(String, String)> = if let Some(enc) = mime_info.encoding {
            vec![("Content-Encoding".to_string(), enc.to_string())]
        } else {
            Vec::new()
        };
        let response = build_full_response(http_ref, 200, "OK", content_type, file_size, file_mtime, &extra_headers);
        let full_response = if http_ref.mime_flag { response } else { Vec::new() };
        let slot = &mut server.conns[slab_key];
        slot.http.response = full_response;
        slot.http.response_len = slot.http.response.len();
        slot.http.status_code = 200;
        transition_to_sending(server, slab_key);
        return;
    }

    // --- If-Modified-Since: 304 ---
    if let Some(ims) = server.conns[slab_key].http.if_modified_since {
        if ims >= file_mtime {
            let http_ref = &server.conns[slab_key].http;
            let filename = file_path.to_string_lossy();
            let mime_info = figure_mime(&filename);
            let content_type = mime_info.mime_type;
            let extra_headers: Vec<(String, String)> = if let Some(enc) = mime_info.encoding {
                vec![("Content-Encoding".to_string(), enc.to_string())]
            } else {
                Vec::new()
            };
            let response = build_full_response(http_ref, 304, "Not Modified", content_type, -1, file_mtime, &extra_headers);
            let full_response = if http_ref.mime_flag { response } else { Vec::new() };
            let slot = &mut server.conns[slab_key];
            slot.http.response = full_response;
            slot.http.response_len = slot.http.response.len();
            slot.http.status_code = 304;
            transition_to_sending(server, slab_key);
            return;
        }
    }

    // --- GET: mmap and serve ---
    let file_path_owned = file_path.to_path_buf();
    let mmap_result = server.mmc.map(&file_path_owned);

    match mmap_result {
        Ok(mmap) => {
            let filename = file_path.to_string_lossy();
            let mime_info = figure_mime(&filename);
            let content_type = mime_info.mime_type;
            let http_ref = &server.conns[slab_key].http;

            let is_range = http_ref.got_range;
            let first_byte = http_ref.first_byte_index;
            let last_byte = http_ref.last_byte_index;

            let body = if is_range {
                let start = first_byte as usize;
                let end = (last_byte as usize) + 1;
                let data = &mmap[..];
                if start < data.len() && end <= data.len() {
                    data[start..end].to_vec()
                } else {
                    data.to_vec()
                }
            } else {
                mmap.to_vec()
            };

            let extra_headers: Vec<(String, String)> = if let Some(enc) = mime_info.encoding {
                vec![("Content-Encoding".to_string(), enc.to_string())]
            } else {
                Vec::new()
            };
            let response = build_full_response(http_ref, 200, "OK", content_type, file_size, file_mtime, &extra_headers);
            let slot = &mut server.conns[slab_key];
            let full_response = if slot.http.mime_flag { let mut r = response; r.extend_from_slice(&body); r } else { body };
            slot.http.file_address = Some(mmap);
            slot.http.response = full_response;
            slot.http.response_len = slot.http.response.len();
            slot.http.bytes_sent = 0;
            slot.http.status_code = if is_range { 206 } else { 200 };
            transition_to_sending(server, slab_key);
        }
        Err(_) => {
            let url = server.conns[slab_key].http.encoded_url.clone();
            let user_agent = server.conns[slab_key].http.user_agent.clone();
            let body = error_page(404, "Not Found", "The requested URL '%.80s' was not found on this server.\n", &url, Some(&user_agent));
            let http_ref = &server.conns[slab_key].http;
            let response = build_full_response(http_ref, 404, "Not Found", "text/html", -1, 0, &[]);
            let full_response = if http_ref.mime_flag { let mut r = response; r.extend_from_slice(&body); r } else { body };
            let slot = &mut server.conns[slab_key];
            slot.http.response = full_response;
            slot.http.response_len = slot.http.response.len();
            transition_to_sending(server, slab_key);
        }
    }
}

/// Dispatch a CGI request.
fn dispatch_cgi(server: &mut Server, slab_key: usize, _script_path: &Path) {
    // Propagate the configured charset to the connection
    server.conns[slab_key].http.charset = server.config.charset.clone();

    let (method, orig_filename, query, host, peer_addr, content_type, content_length,
         user_agent, referer, accept, accept_encoding, accept_language, cookie, path_info, x_forwarded_for) = {
        let slot = &server.conns[slab_key];
        (
            slot.http.method.as_str().to_string(),
            slot.http.orig_filename.clone(),
            slot.http.query.clone(),
            slot.http.host.clone(),
            slot.peer_addr.map(|a| a.to_string()).unwrap_or_default(),
            slot.http.content_type.clone(),
            slot.http.content_length,
            slot.http.user_agent.clone(),
            slot.http.referer.clone(),
            slot.http.accept.clone(),
            slot.http.accept_encoding.clone(),
            slot.http.accept_language.clone(),
            slot.http.cookie.clone(),
            slot.http.path_info.clone(),
            slot.http.x_forwarded_for.clone(),
        )
    };

    // --- PATH_INFO extraction ---
    let (resolved_script, final_path_info) = if path_info.is_empty() {
        let mut test_path = orig_filename.clone();
        let mut extracted_pathinfo = String::new();

        loop {
            let full_path = server.config.dir.join(&test_path[1..]);
            if full_path.exists() {
                break (test_path, extracted_pathinfo);
            }
            if let Some(last_slash) = test_path.rfind('/') {
                if last_slash == 0 {
                    break (orig_filename.clone(), String::new());
                }
                let stripped = &test_path[last_slash + 1..];
                if extracted_pathinfo.is_empty() {
                    extracted_pathinfo = format!("/{}", stripped);
                } else {
                    extracted_pathinfo = format!("/{}{}", stripped, extracted_pathinfo);
                }
                test_path = test_path[..last_slash].to_string();
            } else {
                break (orig_filename.clone(), String::new());
            }
        }
    } else {
        (orig_filename.clone(), path_info)
    };

    // Update path_info in HttpConn
    {
        let slot = &mut server.conns[slab_key];
        slot.http.path_info = final_path_info.clone();
    }

    let resolved_path = server.config.dir.join(&resolved_script[1..]);

    // --- CGI not-found check ---
    if !resolved_path.exists() || resolved_path.is_dir() {
        let url = server.conns[slab_key].http.encoded_url.clone();
        let user_agent = server.conns[slab_key].http.user_agent.clone();
        let body = error_page(404, "Not Found", "The requested URL '%.80s' was not found on this server.\n", &url, Some(&user_agent));
        let http_ref = &server.conns[slab_key].http;
        let response = build_full_response(http_ref, 404, "Not Found", "text/html", -1, 0, &[]);
        let full_response = if http_ref.mime_flag { let mut r = response; r.extend_from_slice(&body); r } else { body };
        let slot = &mut server.conns[slab_key];
        slot.http.response = full_response;
        slot.http.response_len = slot.http.response.len();
        transition_to_sending(server, slab_key);
        return;
    }

    // Build HTTP headers map
    let mut http_headers = std::collections::HashMap::new();
    if !host.is_empty() { http_headers.insert("Host".to_string(), host.rsplit_once(':').map(|(ip, _)| ip).unwrap_or(&host).to_string()); }
    if !user_agent.is_empty() { http_headers.insert("User-Agent".to_string(), user_agent); }
    if !referer.is_empty() { http_headers.insert("Referer".to_string(), referer); }
    if !accept.is_empty() { http_headers.insert("Accept".to_string(), accept); }
    if !accept_encoding.is_empty() { http_headers.insert("Accept-Encoding".to_string(), accept_encoding); }
    if !accept_language.is_empty() { http_headers.insert("Accept-Language".to_string(), accept_language); }
    if !cookie.is_empty() { http_headers.insert("Cookie".to_string(), cookie); }

    // Compute REMOTE_ADDR. C uses httpd_ntoa (libhttpd.c:4063-4085) which:
    //   1. Uses X-Forwarded-For first IP if present (libhttpd.c:2210)
    //   2. Strips "::ffff:" prefix from IPv4-mapped IPv6 addresses
    //   3. Falls back to inet_ntoa (plain IPv4)
    let remote_addr_clean = if !x_forwarded_for.is_empty() {
        x_forwarded_for.clone()
    } else {
        // Strip port, then strip "::ffff:" prefix from IPv4-mapped addresses.
        // SocketAddr::to_string() for an IPv4-mapped addr gives "[::ffff:127.0.0.1]:port"
        // or "::ffff:127.0.0.1:port" depending on formatting. Strip port first.
        let without_port = peer_addr
            .rsplit_once(':')
            .map(|(ip, _)| ip)
            .unwrap_or(&peer_addr)
            .trim_start_matches('[')
            .trim_end_matches(']');
        // Strip the IPv6-style "::ffff:" prefix if present (mirrors C at libhttpd.c:4077)
        if let Some(stripped) = without_port.strip_prefix("::ffff:") {
            stripped.to_string()
        } else {
            without_port.to_string()
        }
    };
    let _host_clean = host.rsplit_once(':').map(|(ip, _)| ip).unwrap_or(&host).to_string();

    // Get hostname via gethostname()
    let server_name = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "localhost".to_string());

    let path_translated = if final_path_info.is_empty() {
        None
    } else {
        Some(server.config.dir.join(&final_path_info[1..]).to_string_lossy().to_string())
    };

    let cgi_pattern_str = server.config.cgi_pattern.as_deref().unwrap_or("");

    let ctx = thttpd_http::cgi::CgiContext {
        server_software: "sthttpd/2.27.0 03oct2014".to_string(),
        server_name,
        gateway_interface: "CGI/1.1".to_string(),
        server_protocol: "HTTP/1.0".to_string(),
        server_port: server.config.port,
        request_method: method,
        script_name: resolved_script.clone(),
        query_string: query,
        remote_addr: remote_addr_clean,
        content_type: if content_type.is_empty() { None } else { Some(content_type) },
        content_length,
        http_headers,
        path_info: if final_path_info.is_empty() { None } else { Some(final_path_info) },
        path_translated,
        remote_user: None,
        auth_type: None,
    };

    let env = thttpd_http::cgi::build_envp(&ctx, &resolved_script, cgi_pattern_str);

    // Read POST body if present
    // C passes the raw socket fd to CGI, so cat reads ALL remaining data
    // including any trailing bytes beyond Content-Length.  Match that.
    let post_body = server.conns.get(slab_key).and_then(|slot| {
        slot.http.content_length.and_then(|len| {
            let body_start = slot.http.checked_idx;
            if body_start + (len as usize) <= slot.http.read_idx {
                // Include everything buffered after headers (not just Content-Length)
                Some(slot.http.read_buf[body_start..slot.http.read_idx].to_vec())
            } else {
                None
            }
        })
    });

    match thttpd_http::cgi::execute_cgi(&resolved_path, env, post_body.as_deref()) {
        Ok(mut cgi_result) => {
            let mut output = Vec::new();
            if let Some(stdout) = cgi_result.child.stdout.take() {
                let mut stdout = stdout;
                let _ = stdout.read_to_end(&mut output);
            }
            // If stdout is empty, try reading stderr (for error scripts that write to stderr)
            if output.is_empty() {
                if let Some(stderr) = cgi_result.child.stderr.take() {
                    let mut stderr = stderr;
                    let _ = stderr.read_to_end(&mut output);
                }
            }
            let _exit_status = cgi_result.child.wait();

            let response = if cgi_result.is_nph {
                output
            } else {
                // Raw passthrough: build status line + append raw CGI output bytes
                let (status_code, status_text) = extract_cgi_status(&output);
                let mut resp = Vec::new();
                resp.extend_from_slice(format!("HTTP/1.0 {} {}\r\n", status_code, status_text).as_bytes());
                resp.extend_from_slice(&output);
                resp
            };

            let slot = &mut server.conns[slab_key];
            slot.http.response = response;
            slot.http.response_len = slot.http.response.len();
            transition_to_sending(server, slab_key);
        }
        Err(e) => {
            eprintln!("thttpd: CGI error: {e}");
            let url = server.conns[slab_key].http.encoded_url.clone();
            let user_agent = server.conns[slab_key].http.user_agent.clone();
            let body = error_page(500, "Internal Error", "There was an unusual problem serving the requested URL '%.80s'.\n", &url, Some(&user_agent));
            let http_ref = &server.conns[slab_key].http;
            let response = build_full_response(http_ref, 500, "Internal Error", "text/html", -1, 0, &[]);
            let full_response = if http_ref.mime_flag { let mut r = response; r.extend_from_slice(&body); r } else { body };
            let slot = &mut server.conns[slab_key];
            slot.http.response = full_response;
            slot.http.response_len = slot.http.response.len();
            transition_to_sending(server, slab_key);
        }
    }
}

/// Extract status code and text from CGI output headers.
/// Extract status code and text from CGI output headers.
/// Matches C's cgi_interpose_output logic at libhttpd.c:3258-3295:
///   1. Look for "HTTP/" status line
///   2. Else look for "Status:" header (overrides default 200)
///   3. Else if "Location:" header present, set 302
///   4. Map known status codes to their text; unknown → "Something"
fn extract_cgi_status(output: &[u8]) -> (u16, String) {
    let blank_pos = output.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .or_else(|| output.windows(2).position(|w| w == b"\n\n"));

    let header_end = match blank_pos {
        Some(pos) => pos,
        None => return (200, "OK".to_string()),
    };

    let header_bytes = &output[..header_end];
    let header_str = String::from_utf8_lossy(header_bytes);

    let mut status: u16 = 200;
    let mut status_set = false;

    for line in header_str.lines() {
        if let Some(colon_pos) = line.find(':') {
            let name = &line[..colon_pos];
            if name.trim().eq_ignore_ascii_case("status") {
                let value = line[colon_pos + 1..].trim();
                // C: status = atoi( value ) — takes the leading number
                let code_str: String = value
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                if let Ok(code) = code_str.parse::<u16>() {
                    status = code;
                    status_set = true;
                }
            } else if !status_set && name.trim().eq_ignore_ascii_case("location") {
                // C: if no Status: header and Location: is present, set 302
                status = 302;
                status_set = true;
            }
        }
    }

    let title = match status {
        200 => "OK",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        500 => "Internal Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        _ => "Something",
    };
    (status, title.to_string())
}

/// Send response bytes to the connection.
fn handle_send(server: &mut Server, slab_key: usize) -> io::Result<()> {
    let response_len = server.conns[slab_key].http.response_len;
    let bytes_sent = server.conns[slab_key].http.bytes_sent;

    if bytes_sent >= response_len as i64 {
        transition_to_lingering(server, slab_key);
        return Ok(());
    }

    // Write bytes from the response buffer
    let n = {
        let slot = &mut server.conns[slab_key];
        let stream = match slot.stream.as_mut() {
            Some(s) => s,
            None => {
                close_connection(server, slab_key);
                return Ok(());
            }
        };

        let remaining = &slot.http.response[bytes_sent as usize..];
        match stream.write(remaining) {
            Ok(n) => n,
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                return Ok(());
            }
            Err(e) => {
                eprintln!("thttpd: write error on slot {slab_key}: {e}");
                close_connection(server, slab_key);
                return Ok(());
            }
        }
    };

    server.conns[slab_key].http.bytes_sent += n as i64;
    server.stats.bytes_sent += n as u64;

    let bytes_sent = server.conns[slab_key].http.bytes_sent;
    if bytes_sent >= server.conns[slab_key].http.response_len as i64 {
        transition_to_lingering(server, slab_key);
    } else {
        // More to send — reregister for writable
        let token = conn_token(slab_key);
        let slot = &mut server.conns[slab_key];
        if let Some(ref mut stream) = slot.stream {
            let _ = server.poll.registry().reregister(
                stream,
                token,
                Interest::WRITABLE,
            );
        }
    }

    Ok(())
}

/// Drain remaining bytes from the socket before closing.
fn handle_linger(server: &mut Server, slab_key: usize) -> io::Result<()> {
    let mut buf = [0u8; 1024];
    let slot = &mut server.conns[slab_key];

    let stream = match slot.stream.as_mut() {
        Some(s) => s,
        None => {
            close_connection(server, slab_key);
            return Ok(());
        }
    };

    match stream.read(&mut buf) {
        Ok(0) | Err(_) => {
            close_connection(server, slab_key);
        }
        Ok(_) => {
            close_connection(server, slab_key);
        }
    }

    Ok(())
}

/// Transition a connection from Reading to Sending.
fn transition_to_sending(server: &mut Server, slab_key: usize) {
    if !server.conns.contains(slab_key) {
        return;
    }

    let token = conn_token(slab_key);
    let slot = &mut server.conns[slab_key];
    slot.state = ConnState::Sending;
    slot.http.bytes_sent = 0;

    if let Some(ref mut stream) = slot.stream {
        let _ = server.poll.registry().reregister(
            stream,
            token,
            Interest::WRITABLE,
        );
    }

    server.stats.requests += 1;
}

/// Transition a connection to Lingering state.
fn transition_to_lingering(server: &mut Server, slab_key: usize) {
    if !server.conns.contains(slab_key) {
        return;
    }

    let token = conn_token(slab_key);
    let slot = &mut server.conns[slab_key];
    slot.state = ConnState::Lingering;

    // Release mmap reference if held
    if let Some(mmap) = slot.http.file_address.take() {
        server.mmc.unmap(&mmap);
    }

    if let Some(ref mut stream) = slot.stream {
        let _ = server.poll.registry().reregister(
            stream,
            token,
            Interest::READABLE,
        );
    }
}

/// Close a connection and free its slot.
fn close_connection(server: &mut Server, slab_key: usize) {
    if !server.conns.contains(slab_key) {
        return;
    }

    let slot = &mut server.conns[slab_key];
    if let Some(ref mut stream) = slot.stream {
        let _ = server.poll.registry().deregister(stream);
    }
    slot.stream = None;

    // Release mmap reference
    slot.http.file_address = None;

    slot.state = ConnState::Free;
    server.conns.remove(slab_key);
}

/// Build a complete HTTP error response.
fn build_error_response(status_code: u16, status_text: &str, extra: &str, user_agent: Option<&str>) -> Vec<u8> {
    // For 400 errors, extra is used as arg (empty string)
    // The form is the generic bad-request message
    let form = "Your request has bad syntax or is inherently impossible to satisfy.\n";
    let body = error_page(status_code, status_text, form, extra, user_agent);
    ResponseBuilder::new()
        .status(status_code, status_text)
        .header("Content-Type", "text/html")
        .header("Content-Length", &body.len().to_string())
        .body(body)
        .build()
}

/// Extract a header value from raw HTTP header bytes.
fn extract_header(headers: &[u8], name: &str) -> Option<String> {
    let search = format!("{}:", name);
    let search_lower = search.to_lowercase();
    let header_str = String::from_utf8_lossy(headers);

    for line in header_str.lines() {
        if line.to_lowercase().starts_with(&search_lower) {
            let value = line[search.len()..].trim();
            return Some(value.to_string());
        }
    }

    None
}
