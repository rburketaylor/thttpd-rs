//! Main event loop for thttpd.
//! Translates `legacy/src/thttpd.c:537-609`.
//! New connections get priority over existing connection I/O.

use crate::connection::ConnSlot;
use crate::logging::LogEntry;
use crate::server::Server;
use crate::throttle::{CGI_BYTECOUNT, THROTTLE_TIME, ThrottleDecision};
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::Duration;
use std::time::Instant;
use thttpd_fdwatch::{
    Events, Interest, MAX_LISTENERS, conn_token, is_listen_token, listen_token, slab_key_from_token,
};
use thttpd_http::Method;
use thttpd_http::conn::ConnState;
use thttpd_http::parse::{got_request, parse_method};
use thttpd_http::parse_state::GotRequest;
use thttpd_http::response::{ResponseBuilder, build_full_response, error_page};
use thttpd_http::url::{normalize_path, percent_decode};
use thttpd_match::match_pattern;
use thttpd_mime::figure_mime;

/// Maximum number of connections we accept.
const MAX_CONNECTIONS: usize = 4096;

/// Size of the read buffer per connection — matches C's 60000.
const READ_BUF_SIZE: usize = 60000;

/// Maximum URL length before returning 500 Internal Error (matches C behavior).
const MAX_URL_LENGTH: usize = 10000;

/// Normalize a request/script path for throttle matching: strip a single
/// leading `/` so the path is compared against throttle patterns the same
/// way the throttlefile loader stores them. `throttle::parse_line` strips
/// leading slashes from patterns at load time, so a rule like `*.html`
/// (whose single `*` does not cross `/`) would otherwise never match a
/// request path that arrives as `/index.html`. Only the value handed to
/// `ThrottleTable::check_request` is normalized; user-facing paths, access
/// logs, and error pages keep their original leading-slash form.
#[inline]
fn throttle_match_path(path: &str) -> &str {
    path.strip_prefix('/').unwrap_or(path)
}

/// Internal helper: throttle admission result carried between non-overlapping
/// borrows of `server.throttles` and `server.conns[slab_key]`.
enum ThrottleAction {
    Allow {
        tnums: Vec<usize>,
        max_limit: i64,
        min_limit: i64,
    },
    Reject,
}

/// Run the main event loop until termination.
pub fn run(server: &mut Server) -> io::Result<()> {
    // Register listeners with poll. Listener tokens occupy the reserved
    // range [0, MAX_LISTENERS); refuse to register more listeners than that
    // range holds instead of handing a listener the same token as a
    // connection (which would corrupt event dispatch).
    for (i, listener) in server.listeners.iter_mut().enumerate() {
        let token = match listen_token(i) {
            Some(t) => t,
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "too many listen sockets ({}): maximum is {MAX_LISTENERS}",
                        server.listeners.len()
                    ),
                ));
            }
        };
        server
            .poll
            .registry()
            .register(listener, token, Interest::READABLE)?;
    }

    let mut events = Events::with_capacity(1024);
    let mut last_throttle_update = Instant::now();

    loop {
        // Check termination signal
        if crate::signal::got_terminate() {
            break;
        }

        // SIGUSR1 — graceful drain
        if !server.draining && crate::signal::got_usr1() {
            server.draining = true;
            // Deregister listeners so poll stops reporting pending connections.
            // Without this, a level-triggered readable listener with a pending
            // connection causes the event loop to busy-wait at 100% CPU.
            for listener in &mut server.listeners {
                let _ = server.poll.registry().deregister(listener);
            }
            if server.conns.is_empty() {
                break;
            }
        }

        // Calculate poll timeout from the timer wheel, paused throttles, and
        // the periodic throttle-average update deadline.
        let timeout = server
            .timers
            .next_deadline()
            .into_iter()
            .chain(next_pause_deadline(server))
            .chain(next_throttle_deadline(server, last_throttle_update))
            .min()
            .unwrap_or(Duration::from_secs(60));

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

        resume_ready_paused_connections(server)?;

        // Run expired timers
        let mut ctx = thttpd_timers::TimerCtx;
        server.timers.run(&mut ctx);

        // Periodic mmc cleanup
        server.mmc.cleanup();

        // Periodic throttle update (every THROTTLE_TIME seconds)
        {
            let now = Instant::now();
            if now.duration_since(last_throttle_update) >= Duration::from_secs(THROTTLE_TIME as u64)
            {
                if let Some(ref mut throttles) = server.throttles {
                    throttles.update_averages();
                }
                last_throttle_update = now;

                // Wake paused connections and recompute fair-share
                for (slab_key, slot) in server.conns.iter_mut() {
                    if slot.state == ConnState::Pausing && slot.throttle.is_throttled() {
                        if let Some(pause_until) = slot.throttle.pause_until {
                            if Instant::now() >= pause_until {
                                if let Some(ref throttles) = server.throttles {
                                    let new_max = throttles.fair_share_for(&slot.throttle.tnums);
                                    slot.throttle.max_limit = new_max;
                                }
                                slot.state = ConnState::Sending;
                                slot.throttle.pause_until = None;
                                let token = conn_token(slab_key);
                                if let Some(ref mut stream) = slot.stream {
                                    // Re-register (not reregister): the stream
                                    // was deregistered during the pause, so
                                    // reregister would fail and the response
                                    // could hang on the next window.
                                    let _ = server.poll.registry().register(
                                        stream,
                                        token,
                                        Interest::WRITABLE,
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }

        // Process signal flags
        if crate::signal::got_hup() {
            if let Err(e) = server.access_log.reopen() {
                eprintln!("thttpd: failed to reopen access log: {e}");
            }
            crate::signal::clear_hup();
        }

        // Graceful drain: exit when no active connections
        if server.draining && server.conns.is_empty() {
            break;
        }
    }

    Ok(())
}

/// Accept new connections from a listen socket.
fn handle_accept(server: &mut Server, listener_idx: usize) -> io::Result<()> {
    // If draining (SIGUSR1), stop accepting new connections
    if server.draining {
        return Ok(());
    }

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
        if let Err(e) = server
            .poll
            .registry()
            .register(stream, token, Interest::READABLE)
        {
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
        ConnState::Pausing => handle_pause(server, slab_key),
        ConnState::Free => Ok(()),
    }
}

fn next_pause_deadline(server: &Server) -> Option<Duration> {
    let now = Instant::now();
    server
        .conns
        .iter()
        .filter(|(_, slot)| slot.state == ConnState::Pausing)
        .filter_map(|(_, slot)| slot.throttle.pause_until)
        .map(|deadline| deadline.saturating_duration_since(now))
        .min()
}

/// Remaining time until the next throttle-average update is due.
///
/// Returns `Some(THROTTLE_TIME − elapsed)` when `server.throttles` is
/// configured, so the poll wakes in time to call `update_averages`. Clamped
/// to zero when already overdue. Returns `None` when throttling is not
/// configured, letting the 60 s fallback apply.
fn next_throttle_deadline(server: &Server, last_update: Instant) -> Option<Duration> {
    if server.throttles.is_some() {
        let elapsed = last_update.elapsed();
        let throttle_period = Duration::from_secs(THROTTLE_TIME as u64);
        Some(throttle_period.saturating_sub(elapsed))
    } else {
        None
    }
}

fn resume_ready_paused_connections(server: &mut Server) -> io::Result<()> {
    let now = Instant::now();
    let ready: Vec<usize> = server
        .conns
        .iter()
        .filter(|(_, slot)| slot.state == ConnState::Pausing)
        .filter(|(_, slot)| match slot.throttle.pause_until {
            Some(deadline) => now >= deadline,
            None => true,
        })
        .map(|(slab_key, _)| slab_key)
        .collect();

    for slab_key in ready {
        if server.conns.contains(slab_key) && server.conns[slab_key].state == ConnState::Pausing {
            handle_pause(server, slab_key)?;
        }
    }

    Ok(())
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

    // Resume a CGI POST dispatch that was deferred while the body was still
    // arriving.  The request FSM has already fired GotRequest and produced a
    // pending_cgi_body flag; once the full Content-Length body is buffered,
    // re-dispatch the CGI directly (do not re-run the FSM / process_request).
    if server.conns[slab_key].pending_cgi_body {
        let body_complete = {
            let http = &server.conns[slab_key].http;
            match http.content_length {
                Some(cl) if cl > 0 => {
                    let body_start = http.checked_idx;
                    body_start + (cl as usize) <= http.read_idx
                }
                _ => true,
            }
        };
        if body_complete {
            server.conns[slab_key].pending_cgi_body = false;
            dispatch_cgi(server, slab_key, Path::new(""));
        }
        return Ok(());
    }

    // Run the request-detection FSM
    let (result, new_checked, new_state) = {
        let http = &server.conns[slab_key].http;
        got_request(
            &http.read_buf,
            http.checked_idx,
            http.read_idx,
            http.parse_state,
        )
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

        let header_start = buf
            .iter()
            .position(|&b| b == b'\n')
            .map(|p| p + 1)
            .unwrap_or(0);
        let headers_bytes = &buf[header_start..];
        let host = extract_header(headers_bytes, "Host").unwrap_or_default();

        (
            url,
            version.clone().unwrap_or_else(|| "HTTP/0.9".to_string()),
            host,
            version.is_some(),
        )
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
        slot.http.one_one =
            has_version && !version_str.is_empty() && !version_str.eq_ignore_ascii_case("HTTP/1.0");
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
    if server.conns[slab_key].http.one_one && server.conns[slab_key].http.host.is_empty() {
        let user_agent = server.conns[slab_key].http.user_agent.clone();
        let v = server.conns[slab_key].http.http_version.clone();
        let body = error_page(
            400,
            "Bad Request",
            "Your request has bad syntax or is inherently impossible to satisfy.\n",
            &v,
            Some(&user_agent),
        );
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
            request_line
                .split_whitespace()
                .next()
                .unwrap_or("UNKNOWN")
                .to_string()
        };
        let user_agent = server.conns[slab_key].http.user_agent.clone();
        let body = error_page(
            501,
            "Not Implemented",
            "The requested method '%.80s' is not implemented by this server.\n",
            &method_str,
            Some(&user_agent),
        );
        let http_ref = &server.conns[slab_key].http;
        let response =
            build_full_response(http_ref, 501, "Not Implemented", "text/html", -1, 0, &[]);
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
        let header_start = buf
            .iter()
            .position(|&b| b == b'\n')
            .map(|p| p + 1)
            .unwrap_or(0);
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
                                        slot.http.last_byte_index =
                                            if last < 0 { -1 } else { last };
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
            let body = error_page(
                500,
                "Internal Error",
                "There was an unusual problem serving the requested URL '%.80s'.\n",
                &slot.http.encoded_url,
                Some(&user_agent),
            );
            let http_ref = &server.conns[slab_key].http;
            let response =
                build_full_response(http_ref, 500, "Internal Error", "text/html", -1, 0, &[]);
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
                    (
                        400,
                        "Bad Request",
                        "Your request has bad syntax or is inherently impossible to satisfy.\n",
                    )
                } else {
                    (
                        404,
                        "Not Found",
                        "The requested URL '%.80s' was not found on this server.\n",
                    )
                };
                let user_agent = server.conns[slab_key].http.user_agent.clone();
                let body = error_page(status, title, form_msg, decoded, Some(&user_agent));
                let http_ref = &server.conns[slab_key].http;
                let response =
                    build_full_response(http_ref, status, title, "text/html", -1, 0, &[]);
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
    let file_path = if server.config.vhost && !server.conns[slab_key].http.host.is_empty() {
        let host_lower: String = server.conns[slab_key].http.host.to_lowercase();
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
        let vhost_active = server.config.vhost && !server.conns[slab_key].http.vhost_dir.is_empty();
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
        if server.config.no_symlink_check {
            file_path.to_path_buf()
        } else {
            let canonical_root = match std::fs::canonicalize(&server.config.dir) {
                Ok(p) => p,
                Err(_) => {
                    let user_agent = server.conns[slab_key].http.user_agent.clone();
                    let body = error_page(
                        500,
                        "Internal Error",
                        "There was an unusual problem serving the requested URL '%.80s'.\n",
                        &server.config.dir.to_string_lossy(),
                        Some(&user_agent),
                    );
                    let http_ref = &server.conns[slab_key].http;
                    let response = build_full_response(
                        http_ref,
                        500,
                        "Internal Error",
                        "text/html",
                        -1,
                        0,
                        &[],
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
            };
            match std::fs::canonicalize(file_path) {
                Ok(canonical) => {
                    if !canonical.starts_with(&canonical_root) {
                        let url = server.conns[slab_key].http.encoded_url.clone();
                        let user_agent = server.conns[slab_key].http.user_agent.clone();
                        let body = error_page(
                            403,
                            "Forbidden",
                            "The requested URL '%.80s' resolves to a file outside the permitted web server directory tree.\n",
                            &url,
                            Some(&user_agent),
                        );
                        let http_ref = &server.conns[slab_key].http;
                        let response = build_full_response(
                            http_ref,
                            403,
                            "Forbidden",
                            "text/html",
                            -1,
                            0,
                            &[],
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
                    canonical
                }
                Err(_) => file_path.to_path_buf(),
            }
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

    // --- Basic Auth check (libhttpd.c:3732-3773) ---
    // If a .htpasswd file exists in the directory tree containing the
    // requested file, require Basic Auth. Returns 401 if missing or wrong.
    {
        let (authorization, encoded_url) = {
            let slot = &server.conns[slab_key];
            (
                slot.http.authorization.clone(),
                slot.http.encoded_url.clone(),
            )
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
                    http_ref,
                    401,
                    thttpd_http::auth::ERR_401_TITLE,
                    "text/html",
                    -1,
                    0,
                    &extra_headers,
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
            thttpd_http::auth::AuthResult::Forbidden => {
                let user_agent = server.conns[slab_key].http.user_agent.clone();
                let body = error_page(
                    403,
                    "Forbidden",
                    "The requested URL '%.80s' is not accessible.\n",
                    &encoded_url,
                    Some(&user_agent),
                );
                let http_ref = &server.conns[slab_key].http;
                let response =
                    build_full_response(http_ref, 403, "Forbidden", "text/html", -1, 0, &[]);
                let full_response = if http_ref.mime_flag {
                    let mut response = response;
                    response.extend_from_slice(&body);
                    response
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
        let mtime = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        match thttpd_http::dirlist::generate_listing(&dir, &url_path) {
            Ok(body) => {
                let http_ref = &server.conns[slab_key].http;
                let response =
                    build_full_response(http_ref, 200, "OK", "text/html", -1, mtime, &[]);
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
            Err(e) => {
                eprintln!("thttpd: directory listing error: {e}");
                let user_agent = server.conns[slab_key].http.user_agent.clone();
                let body = error_page(
                    500,
                    "Internal Error",
                    "There was an unusual problem serving the requested URL '%.80s'.\n",
                    &file_path.to_string_lossy(),
                    Some(&user_agent),
                );
                let http_ref = &server.conns[slab_key].http;
                let response =
                    build_full_response(http_ref, 500, "Internal Error", "text/html", -1, 0, &[]);
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

    // Check world-readable permission (Unix mode bits)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode();
        if (mode & 0o004) == 0 {
            let url = server.conns[slab_key].http.encoded_url.clone();
            let user_agent = server.conns[slab_key].http.user_agent.clone();
            let body = error_page(
                403,
                "Forbidden",
                "The requested URL '%.80s' resolves to a file that is not world-readable.\n",
                &url,
                Some(&user_agent),
            );
            let http_ref = &server.conns[slab_key].http;
            let response = build_full_response(http_ref, 403, "Forbidden", "text/html", -1, 0, &[]);
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

    let file_size = metadata.len() as i64;
    let file_mtime = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    // Fill in last_byte_index if needed
    {
        let slot = &mut server.conns[slab_key];
        if slot.http.got_range
            && (slot.http.last_byte_index == -1 || slot.http.last_byte_index >= file_size)
        {
            slot.http.last_byte_index = file_size - 1;
        }
    }

    // --- Throttle admission pre-check ---
    // Check throttle rules BEFORE early static responses and before mmap'ing /
    // copying the file body so over-limit requests get the legacy 503 and do
    // not pay the memory and I/O cost of reading a potentially large file.
    let request_path = server.conns[slab_key].http.orig_filename.clone();
    let match_path = throttle_match_path(&request_path);
    let (throttle_state, throttle_rejected) = if let Some(ref mut throttles) = server.throttles {
        match throttles.check_request(match_path) {
            ThrottleDecision::Allow {
                tnums,
                max_limit,
                min_limit,
            } => (Some((tnums, max_limit, min_limit)), false),
            ThrottleDecision::Reject => (None, true),
            ThrottleDecision::Unlimited => (None, false),
        }
    } else {
        (None, false)
    };

    if throttle_rejected {
        {
            let slot = &mut server.conns[slab_key];
            let user_agent = slot.http.user_agent.clone();
            let body = error_page(
                503,
                "Service Unavailable",
                "The requested URL '%.80s' is temporarily over its bandwidth limit.\n",
                &request_path,
                Some(&user_agent),
            );
            let http_ref = &slot.http;
            let response = build_full_response(
                http_ref,
                503,
                "Service Unavailable",
                "text/html",
                -1,
                0,
                &[],
            );
            let full_response = if http_ref.mime_flag {
                let mut r = response;
                r.extend_from_slice(&body);
                r
            } else {
                body
            };
            slot.http.response = full_response;
            slot.http.response_len = slot.http.response.len();
            slot.http.status_code = 503;
            slot.throttle.checked = true;
        }
        transition_to_sending(server, slab_key);
        return;
    }

    {
        let slot = &mut server.conns[slab_key];
        if let Some((tnums, max_limit, min_limit)) = throttle_state {
            slot.throttle.tnums = tnums;
            slot.throttle.max_limit = max_limit;
            slot.throttle.min_limit = min_limit;
        }
        slot.throttle.checked = true;
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
        let response = build_full_response(
            http_ref,
            200,
            "OK",
            content_type,
            file_size,
            file_mtime,
            &extra_headers,
        );
        let full_response = if http_ref.mime_flag {
            response
        } else {
            Vec::new()
        };
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
            let response = build_full_response(
                http_ref,
                304,
                "Not Modified",
                content_type,
                -1,
                file_mtime,
                &extra_headers,
            );
            let full_response = if http_ref.mime_flag {
                response
            } else {
                Vec::new()
            };
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
            let response = build_full_response(
                http_ref,
                200,
                "OK",
                content_type,
                file_size,
                file_mtime,
                &extra_headers,
            );
            let slot = &mut server.conns[slab_key];
            let header_len = if slot.http.mime_flag {
                response.len()
            } else {
                0
            };
            let full_response = if slot.http.mime_flag {
                let mut r = response;
                r.extend_from_slice(&body);
                r
            } else {
                body
            };
            slot.http.file_address = Some(mmap);
            slot.http.response = full_response;
            slot.http.response_len = slot.http.response.len();
            slot.http.response_header_len = header_len;
            slot.http.bytes_sent = 0;
            slot.http.status_code = if is_range { 206 } else { 200 };
            transition_to_sending(server, slab_key);
        }
        Err(_) => {
            let url = server.conns[slab_key].http.encoded_url.clone();
            let user_agent = server.conns[slab_key].http.user_agent.clone();
            let body = error_page(
                404,
                "Not Found",
                "The requested URL '%.80s' was not found on this server.\n",
                &url,
                Some(&user_agent),
            );
            let http_ref = &server.conns[slab_key].http;
            let response = build_full_response(http_ref, 404, "Not Found", "text/html", -1, 0, &[]);
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
        }
    }
}

/// Dispatch a CGI request.
fn dispatch_cgi(server: &mut Server, slab_key: usize, _script_path: &Path) {
    // Propagate the configured charset to the connection
    server.conns[slab_key].http.charset = server.config.charset.clone();

    // CGI POST bodies may arrive split across multiple TCP reads.  C hands the
    // raw socket fd to the CGI child, which reads the body directly; we buffer
    // it instead, so defer dispatch until the full Content-Length body is
    // buffered.  This early return happens before any counter mutation, so the
    // later resume via handle_read can re-enter dispatch_cgi cleanly.
    {
        let http = &server.conns[slab_key].http;
        if let Some(cl) = http.content_length {
            if cl > 0 {
                let body_start = http.checked_idx;
                if body_start + (cl as usize) > http.read_idx {
                    server.conns[slab_key].pending_cgi_body = true;
                    return;
                }
            }
        }
    }

    // --- CGI limit enforcement (matching C's cgi_limit check at thttpd.c:220) ---
    if server.cgi_limit() > 0 && server.active_cgis >= server.cgi_limit() {
        let url = server.conns[slab_key].http.encoded_url.clone();
        let user_agent = server.conns[slab_key].http.user_agent.clone();
        let body = error_page(
            503,
            "Service Unavailable",
            "Too many concurrent CGI requests. Please try again later.\n",
            &url,
            Some(&user_agent),
        );
        let http_ref = &server.conns[slab_key].http;
        let response = build_full_response(
            http_ref,
            503,
            "Service Unavailable",
            "text/html",
            -1,
            0,
            &[],
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
    server.active_cgis += 1;

    let (
        method,
        orig_filename,
        query,
        host,
        peer_addr,
        content_type,
        content_length,
        user_agent,
        referer,
        accept,
        accept_encoding,
        accept_language,
        cookie,
        path_info,
        x_forwarded_for,
    ) = {
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

    // Set CGI working directory to the script's parent (matching C behavior:
    // legacy/src/libhttpd.c:3497 chdir()s to the script directory before execve).
    let cgi_working_dir = resolved_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| server.config.dir.clone());

    // --- CGI not-found check ---
    if !resolved_path.exists() || resolved_path.is_dir() {
        server.active_cgis -= 1;
        let url = server.conns[slab_key].http.encoded_url.clone();
        let user_agent = server.conns[slab_key].http.user_agent.clone();
        let body = error_page(
            404,
            "Not Found",
            "The requested URL '%.80s' was not found on this server.\n",
            &url,
            Some(&user_agent),
        );
        let http_ref = &server.conns[slab_key].http;
        let response = build_full_response(http_ref, 404, "Not Found", "text/html", -1, 0, &[]);
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

    // --- CGI throttle check ---
    if let Some(ref mut throttles) = server.throttles {
        match throttles.check_request(throttle_match_path(&resolved_script)) {
            ThrottleDecision::Reject => {
                server.active_cgis -= 1;
                let url = server.conns[slab_key].http.encoded_url.clone();
                let user_agent = server.conns[slab_key].http.user_agent.clone();
                let body = error_page(
                    503,
                    "Service Unavailable",
                    "The requested URL '%.80s' is temporarily over its bandwidth limit.\n",
                    &url,
                    Some(&user_agent),
                );
                let http_ref = &server.conns[slab_key].http;
                let response = build_full_response(
                    http_ref,
                    503,
                    "Service Unavailable",
                    "text/html",
                    -1,
                    0,
                    &[],
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
            ThrottleDecision::Allow { tnums, .. } => {
                // Store tnums so we can account CGI_BYTECOUNT on completion
                server.conns[slab_key].throttle.tnums = tnums;
            }
            ThrottleDecision::Unlimited => {}
        }
    }

    // Build HTTP headers map
    let mut http_headers = std::collections::HashMap::new();
    if !host.is_empty() {
        http_headers.insert(
            "Host".to_string(),
            host.rsplit_once(':')
                .map(|(ip, _)| ip)
                .unwrap_or(&host)
                .to_string(),
        );
    }
    if !user_agent.is_empty() {
        http_headers.insert("User-Agent".to_string(), user_agent);
    }
    if !referer.is_empty() {
        http_headers.insert("Referer".to_string(), referer);
    }
    if !accept.is_empty() {
        http_headers.insert("Accept".to_string(), accept);
    }
    if !accept_encoding.is_empty() {
        http_headers.insert("Accept-Encoding".to_string(), accept_encoding);
    }
    if !accept_language.is_empty() {
        http_headers.insert("Accept-Language".to_string(), accept_language);
    }
    if !cookie.is_empty() {
        http_headers.insert("Cookie".to_string(), cookie);
    }

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
    let _host_clean = host
        .rsplit_once(':')
        .map(|(ip, _)| ip)
        .unwrap_or(&host)
        .to_string();

    // Get hostname via gethostname()
    let server_name = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "localhost".to_string());

    let path_translated = if final_path_info.is_empty() {
        None
    } else {
        Some(
            server
                .config
                .dir
                .join(&final_path_info[1..])
                .to_string_lossy()
                .to_string(),
        )
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
        content_type: if content_type.is_empty() {
            None
        } else {
            Some(content_type)
        },
        content_length,
        http_headers,
        path_info: if final_path_info.is_empty() {
            None
        } else {
            Some(final_path_info)
        },
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

    // Past all admission guards (limit, not-found, throttle-reject): this
    // request is a genuine CGI dispatch. Flag it so transition_to_sending
    // does not re-admit the response against the throttle table — CGI output
    // is already charged a flat CGI_BYTECOUNT on completion below.
    server.conns[slab_key].is_cgi = true;

    match thttpd_http::cgi::execute_cgi(
        &resolved_path,
        Some(&cgi_working_dir),
        env,
        post_body.as_deref(),
    ) {
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
                resp.extend_from_slice(
                    format!("HTTP/1.0 {} {}\r\n", status_code, status_text).as_bytes(),
                );
                resp.extend_from_slice(&output);
                resp
            };

            // CGI completion cleanup
            server.active_cgis -= 1;
            if server.conns[slab_key].throttle.is_throttled() {
                let tnums = server.conns[slab_key].throttle.tnums.clone();
                if let Some(ref mut throttles) = server.throttles {
                    throttles.add_bytes(&tnums, CGI_BYTECOUNT);
                    throttles.clear(&tnums);
                }
                // CGI output was charged a flat CGI_BYTECOUNT above; clear
                // the per-connection throttle so the response stream is sent
                // without re-admission or rate limiting.
                server.conns[slab_key].throttle.reset();
            }

            let slot = &mut server.conns[slab_key];
            slot.http.response = response;
            slot.http.response_len = slot.http.response.len();
            transition_to_sending(server, slab_key);
        }
        Err(e) => {
            eprintln!("thttpd: CGI error: {e}");

            // CGI completion cleanup
            server.active_cgis -= 1;
            if server.conns[slab_key].throttle.is_throttled() {
                let tnums = server.conns[slab_key].throttle.tnums.clone();
                if let Some(ref mut throttles) = server.throttles {
                    throttles.clear(&tnums);
                }
                server.conns[slab_key].throttle.reset();
            }

            let url = server.conns[slab_key].http.encoded_url.clone();
            let user_agent = server.conns[slab_key].http.user_agent.clone();
            let body = error_page(
                500,
                "Internal Error",
                "There was an unusual problem serving the requested URL '%.80s'.\n",
                &url,
                Some(&user_agent),
            );
            let http_ref = &server.conns[slab_key].http;
            let response =
                build_full_response(http_ref, 500, "Internal Error", "text/html", -1, 0, &[]);
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
    let blank_pos = output
        .windows(4)
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
                let code_str: String = value.chars().take_while(|c| c.is_ascii_digit()).collect();
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
///
/// Throttled connections never hand the kernel more body bytes than the
/// current time window allows: the write slice is capped at
/// `remaining_header_bytes + allowed_body_bytes` (headers are free; only
/// body bytes count against `max_limit * elapsed`). When the allowance is
/// spent and only body bytes remain, the connection pauses for the
/// one-second throttle window instead of writing.
fn handle_send(server: &mut Server, slab_key: usize) -> io::Result<()> {
    let response_len = server.conns[slab_key].http.response_len;
    let bytes_sent_before = server.conns[slab_key].http.bytes_sent;

    if bytes_sent_before >= response_len as i64 {
        transition_to_lingering(server, slab_key);
        return Ok(());
    }

    // Cap the bytes written this round when the connection is throttled.
    // Unthrottled connections (or those with no max limit) write the whole
    // remaining buffer.
    let write_limit: usize = {
        let slot = &server.conns[slab_key];
        if slot.throttle.is_throttled() && slot.throttle.max_limit > 0 {
            let header_len = slot.throttle.header_len as i64;
            let remaining_header_bytes = (header_len - bytes_sent_before).max(0) as usize;
            let elapsed = slot
                .throttle
                .started_at
                .map_or(1, |t| t.elapsed().as_secs().max(1)) as i64;
            let allowed_body_bytes =
                (slot.throttle.max_limit * elapsed - slot.throttle.body_bytes).max(0) as usize;
            remaining_header_bytes + allowed_body_bytes
        } else {
            (response_len as i64 - bytes_sent_before) as usize
        }
    };

    // The body allowance is exhausted and only body bytes remain: pause for
    // the throttle window instead of writing (or attempting a zero-byte write).
    if write_limit == 0 {
        let slot = &mut server.conns[slab_key];
        slot.throttle.pause_until = Some(Instant::now() + Duration::from_secs(1));
        slot.state = ConnState::Pausing;
        // Deregister from poll during the pause: resume is timer-driven
        // via next_pause_deadline(), so no socket events are useful.
        // Without deregistration, a readable/writable socket would cause
        // handle_pause to no-op repeatedly for the 1 s pause window.
        if let Some(ref mut stream) = slot.stream {
            let _ = server.poll.registry().deregister(stream);
        }
        return Ok(());
    }

    // Write bytes from the response buffer, capped at the throttle allowance.
    let n = {
        let slot = &mut server.conns[slab_key];
        let stream = match slot.stream.as_mut() {
            Some(s) => s,
            None => {
                close_connection(server, slab_key);
                return Ok(());
            }
        };

        let remaining = &slot.http.response[bytes_sent_before as usize..];
        let limit = write_limit.min(remaining.len());
        match stream.write(&remaining[..limit]) {
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

    let bytes_sent_after = bytes_sent_before + n as i64;
    server.conns[slab_key].http.bytes_sent = bytes_sent_after;
    server.stats.bytes_sent += n as u64;

    // Account body bytes against the throttle (header bytes are never charged).
    {
        let slot = &mut server.conns[slab_key];
        if slot.throttle.is_throttled() {
            let header_len = slot.throttle.header_len as i64;
            let body_before = (bytes_sent_before - header_len).max(0);
            let body_after = (bytes_sent_after - header_len).max(0);
            let body_delta = body_after - body_before;
            if body_delta > 0 {
                if let Some(ref mut throttles) = server.throttles {
                    throttles.add_bytes(&slot.throttle.tnums, body_delta);
                }
                slot.throttle.body_bytes += body_delta;
                slot.throttle.active_at = Some(Instant::now());
            }
        }
    }

    let response_len = server.conns[slab_key].http.response_len;
    let bytes_sent_after = server.conns[slab_key].http.bytes_sent;
    if bytes_sent_after >= response_len as i64 {
        transition_to_lingering(server, slab_key);
    } else {
        // More to send. The next writable event re-enters handle_send, which
        // caps the write at the remaining allowance and pauses once it is
        // exhausted.
        let token = conn_token(slab_key);
        let slot = &mut server.conns[slab_key];
        if let Some(ref mut stream) = slot.stream {
            let _ = server
                .poll
                .registry()
                .reregister(stream, token, Interest::WRITABLE);
        }
    }

    Ok(())
}

fn handle_pause(server: &mut Server, slab_key: usize) -> io::Result<()> {
    let now = Instant::now();
    let pause_until = server.conns[slab_key].throttle.pause_until;
    if matches!(pause_until, Some(deadline) if now < deadline) {
        return Ok(());
    }

    if let Some(ref throttles) = server.throttles {
        let tnums = server.conns[slab_key].throttle.tnums.clone();
        let new_max = throttles.fair_share_for(&tnums);
        server.conns[slab_key].throttle.max_limit = new_max;
    }

    {
        let slot = &mut server.conns[slab_key];
        slot.state = ConnState::Sending;
        slot.throttle.pause_until = None;
    }

    let token = conn_token(slab_key);
    let slot = &mut server.conns[slab_key];
    if let Some(ref mut stream) = slot.stream {
        // The stream was deregistered while paused (handle_send's
        // write_limit == 0 path). mio requires `register` after
        // `deregister`; `reregister` would fail (the error is ignored),
        // leaving no writable interest armed. With pause_until cleared
        // above, nothing would wake the connection, so a response needing
        // another throttled window would hang indefinitely.
        let _ = server
            .poll
            .registry()
            .register(stream, token, Interest::WRITABLE);
    } else {
        close_connection(server, slab_key);
        return Ok(());
    }

    handle_send(server, slab_key)
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

    // Capture filename for throttle check (avoids borrow conflict with slot)
    let filename = server.conns[slab_key].http.orig_filename.clone();

    // CGI responses are already charged a flat CGI_BYTECOUNT on completion;
    // do not re-admit them here, which would double-count the output (once
    // as the fixed CGI charge and again as live response bytes) and hold a
    // fresh num_sending slot until the client drains the response.
    let is_cgi = server.conns[slab_key].is_cgi;

    // Fill in the header length for responses that didn't set it explicitly
    // (the static-200 path does; error responses built via build_full_response
    // do not). This keeps throttle body accounting header-free and lets the
    // access log report entity (body) bytes rather than header-inclusive
    // bytes, matching the CERN/thttpd bytes field. CGI responses keep a zero
    // header length: they are logged as the full stream and are not
    // throttle-re-admitted.
    if !is_cgi {
        let mime_flag = server.conns[slab_key].http.mime_flag;
        let hdr_len = server.conns[slab_key].http.response_header_len;
        if mime_flag && hdr_len == 0 {
            let end = header_end_offset(&server.conns[slab_key].http.response);
            if let Some(end) = end {
                server.conns[slab_key].http.response_header_len = end;
            }
        }
    }

    // Throttle admission check — skipped when the caller already ran it
    // upstream (e.g. serve_static does a pre-check before mmap'ing the
    // file body, setting checked=true).
    let already_checked = server.conns[slab_key].throttle.checked;
    let throttle_action = if !is_cgi && !already_checked {
        if let Some(ref mut throttles) = server.throttles {
            match throttles.check_request(throttle_match_path(&filename)) {
                ThrottleDecision::Allow {
                    tnums,
                    max_limit,
                    min_limit,
                } => Some(ThrottleAction::Allow {
                    tnums,
                    max_limit,
                    min_limit,
                }),
                ThrottleDecision::Reject => Some(ThrottleAction::Reject),
                ThrottleDecision::Unlimited => None,
            }
        } else {
            None
        }
    } else {
        None
    };

    let slot = &mut server.conns[slab_key];
    slot.state = ConnState::Sending;
    slot.http.bytes_sent = 0;

    if already_checked && slot.throttle.is_throttled() {
        slot.throttle.header_len = slot.http.response_header_len;
        let now = Instant::now();
        slot.throttle.started_at = Some(now);
        slot.throttle.active_at = Some(now);
    }

    if let Some(action) = throttle_action {
        match action {
            ThrottleAction::Allow {
                tnums,
                max_limit,
                min_limit,
            } => {
                slot.throttle.tnums = tnums;
                slot.throttle.max_limit = max_limit;
                slot.throttle.min_limit = min_limit;
                slot.throttle.header_len = slot.http.response_header_len;
                let now = Instant::now();
                slot.throttle.started_at = Some(now);
                slot.throttle.active_at = Some(now);
            }
            ThrottleAction::Reject => {
                // Build 503 response inline
                let user_agent = slot.http.user_agent.clone();
                let body = error_page(
                    503,
                    "Service Unavailable",
                    "The requested URL '%.80s' is temporarily over its bandwidth limit.\n",
                    &filename,
                    Some(&user_agent),
                );
                let http_ref = &slot.http;
                let response = build_full_response(
                    http_ref,
                    503,
                    "Service Unavailable",
                    "text/html",
                    -1,
                    0,
                    &[],
                );
                let full_response = if http_ref.mime_flag {
                    let mut r = response;
                    r.extend_from_slice(&body);
                    r
                } else {
                    body
                };
                slot.http.response = full_response;
                slot.http.response_len = slot.http.response.len();
                slot.http.status_code = 503;
                slot.http.response_header_len = header_end_offset(&slot.http.response).unwrap_or(0);
            }
        }
    }

    if let Some(ref mut stream) = slot.stream {
        let _ = server
            .poll
            .registry()
            .reregister(stream, token, Interest::WRITABLE);
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
        let _ = server
            .poll
            .registry()
            .reregister(stream, token, Interest::READABLE);
    }
}

/// Close a connection and free its slot.
fn close_connection(server: &mut Server, slab_key: usize) {
    if !server.conns.contains(slab_key) {
        return;
    }

    // 1. Extract data for access logging and throttle cleanup before
    //    taking a mutable borrow on the slot (avoids borrow conflicts).
    let (
        status_code,
        bytes_sent,
        peer_addr_str,
        method_str,
        url_str,
        protocol_str,
        referer_str,
        ua_str,
        throttle_info,
    ) = {
        let slot = &server.conns[slab_key];
        let peer_addr_str = if !slot.http.x_forwarded_for.is_empty() {
            slot.http.x_forwarded_for.clone()
        } else {
            slot.peer_addr
                .map(|a| a.ip().to_string())
                .unwrap_or_else(|| "-".to_string())
        };
        let method_str = slot.http.method.as_str().to_string();
        let url_str = slot.http.encoded_url.clone();
        let protocol_str = if slot.http.mime_flag {
            slot.http.http_version.clone()
        } else {
            String::new()
        };
        let referer_str = slot.http.referer.clone();
        let ua_str = slot.http.user_agent.clone();
        let throttle_info: Option<Vec<usize>> = if slot.throttle.is_throttled() {
            Some(slot.throttle.tnums.clone())
        } else {
            None
        };
        (
            logged_status_code(&slot.http),
            entity_bytes_sent(slot.http.bytes_sent, slot.http.response_header_len),
            peer_addr_str,
            method_str,
            url_str,
            protocol_str,
            referer_str,
            ua_str,
            throttle_info,
        )
    };

    // 2. Access logging: log completed requests only
    if status_code > 0 {
        let entry = LogEntry {
            remote_addr: &peer_addr_str,
            remote_user: "-",
            method: &method_str,
            url: &url_str,
            protocol: &protocol_str,
            status: status_code,
            bytes_sent,
            referer: &referer_str,
            user_agent: &ua_str,
        };
        server.access_log.log_request(&entry);
    }

    // 3. Release throttle resources
    if let Some(tnums) = throttle_info {
        if let Some(ref mut throttles) = server.throttles {
            throttles.clear(&tnums);
        }
    }

    // 4. Deregister and clean up
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

fn logged_status_code(http: &thttpd_http::HttpConn) -> u16 {
    if http.status_code > 0 {
        return http.status_code;
    }

    status_code_from_response(&http.response).unwrap_or(0)
}

fn status_code_from_response(response: &[u8]) -> Option<u16> {
    let first_line_end = response.windows(2).position(|w| w == b"\r\n")?;
    let first_line = std::str::from_utf8(&response[..first_line_end]).ok()?;
    let mut parts = first_line.split_whitespace();
    let protocol = parts.next()?;
    if !protocol.starts_with("HTTP/") {
        return None;
    }
    parts.next()?.parse().ok()
}

/// Offset of the first byte *after* the blank-line header terminator
/// (`\r\n\r\n`, or `\n\n`), i.e. where the entity body begins. `None` when
/// the response has no header separator. build_full_response always ends its
/// header block with `\r\n\r\n`, so this yields the exact header length for
/// normal HTTP/1.x responses.
fn header_end_offset(response: &[u8]) -> Option<usize> {
    if let Some(pos) = response.windows(4).position(|w| w == b"\r\n\r\n") {
        return Some(pos + 4);
    }
    response
        .windows(2)
        .position(|w| w == b"\n\n")
        .map(|pos| pos + 2)
}

/// Bytes to report in the access log's CERN `bytes` field: the entity body
/// bytes actually sent, with the status line + headers excluded. `bytes_sent`
/// accumulates every byte written from `http.response`; subtract the header
/// length (clamped at 0 for partial sends / HTTP/0.9) to match thttpd's
/// header-excluding accounting (libhttpd.c write path subtracts `responselen`).
fn entity_bytes_sent(bytes_sent: i64, header_len: usize) -> i64 {
    (bytes_sent - header_len as i64).max(0)
}

/// Build a complete HTTP error response.
fn build_error_response(
    status_code: u16,
    status_text: &str,
    extra: &str,
    user_agent: Option<&str>,
) -> Vec<u8> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ServerConfig;
    use crate::logging::AccessLogger;
    use crate::throttle::ThrottleTable;
    use mio::net::TcpStream;
    use std::io::Read;
    use std::net::{TcpListener, TcpStream as StdTcpStream};

    fn test_server(config: ServerConfig) -> Server {
        let access_log = AccessLogger::open(&config).unwrap();
        Server::new(config, Vec::new(), access_log).unwrap()
    }

    fn throttle_table(contents: &str) -> ThrottleTable {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut file, contents.as_bytes()).unwrap();
        ThrottleTable::load(file.path()).unwrap()
    }

    fn static_file_server(
        file_name: &str,
        contents: &[u8],
    ) -> (tempfile::TempDir, Server, std::path::PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let file_path = root.path().join(file_name);
        std::fs::write(&file_path, contents).unwrap();

        let config = ServerConfig {
            dir: root.path().to_path_buf(),
            ..ServerConfig::default()
        };
        let server = test_server(config);
        (root, server, file_path)
    }

    #[test]
    fn throttled_static_response_uses_saved_header_length() {
        let mut server = test_server(ServerConfig::default());
        server.throttles = Some(throttle_table("**.html 1000\n"));
        let key = server.conns.insert(ConnSlot::new());
        let slot = &mut server.conns[key];
        slot.http.orig_filename = "/index.html".to_string();
        slot.http.response = b"header-body".to_vec();
        slot.http.response_len = slot.http.response.len();
        slot.http.response_header_len = 6;

        transition_to_sending(&mut server, key);

        let slot = &server.conns[key];
        assert_eq!(slot.throttle.header_len, 6);
        assert!(slot.throttle.is_throttled());
    }

    #[test]
    fn throttled_send_caps_body_bytes_to_allowance() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = StdTcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server_stream, _) = listener.accept().unwrap();
        server_stream.set_nonblocking(true).unwrap();

        let mut server = test_server(ServerConfig::default());
        // 1 byte/sec ceiling, so the cap is tiny relative to the 4 MiB body
        // and the socket buffer can accept far more than the allowance.
        server.throttles = Some(throttle_table("**.bin 1\n"));
        let key = server.conns.insert(ConnSlot::new());
        let slot = &mut server.conns[key];
        slot.stream = Some(TcpStream::from_std(server_stream));
        slot.http.orig_filename = "/big.bin".to_string();
        slot.http.response = vec![b'x'; 4 * 1024 * 1024];
        slot.http.response[..4].copy_from_slice(b"HEAD");
        slot.http.response_len = slot.http.response.len();
        slot.http.response_header_len = 4;
        transition_to_sending(&mut server, key);

        handle_send(&mut server, key).unwrap();

        let slot = &server.conns[key];
        // Headers (4 bytes) are free; only the 1-byte body allowance is sent.
        assert!(slot.throttle.body_bytes > 0);
        assert_eq!(
            server.throttles.as_ref().unwrap().entries()[0].bytes_since_avg,
            slot.throttle.body_bytes
        );
        // The cap keeps us far below the full response even though the socket
        // could accept the whole buffer at once.
        assert!(slot.http.bytes_sent < slot.http.response_len as i64);
        assert_eq!(slot.state, ConnState::Sending);

        // The body allowance for this window is now exhausted: the next send
        // pauses for the one-second throttle window instead of writing more.
        handle_send(&mut server, key).unwrap();
        assert_eq!(server.conns[key].state, ConnState::Pausing);
        drop(client);
    }

    #[test]
    fn throttle_resume_registers_stream_after_pause_deregister() {
        // Regression for the throttle-resume registration bug: handle_send
        // deregisters the stream when the body allowance is exhausted, so the
        // resume path must RE-REGISTER it. mio rejects `reregister` on a
        // deregistered source and the error was ignored, leaving the
        // connection with no writable interest and no pause deadline — any
        // response needing another throttled window hung indefinitely.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = StdTcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server_stream, _) = listener.accept().unwrap();
        server_stream.set_nonblocking(true).unwrap();

        let mut server = test_server(ServerConfig::default());
        let key = server.conns.insert(ConnSlot::new());
        let header_len = 4usize;
        {
            let slot = &mut server.conns[key];
            slot.stream = Some(TcpStream::from_std(server_stream));
            slot.state = ConnState::Sending;
            slot.http.response = vec![b'x'; 204];
            slot.http.response[..header_len].copy_from_slice(b"HEAD");
            slot.http.response_len = slot.http.response.len();
            slot.http.response_header_len = header_len;
            // Non-empty tnums marks the connection as throttled; the body
            // ceiling is 10 bytes/sec.
            slot.throttle.tnums = vec![0];
            slot.throttle.max_limit = 10;
            slot.throttle.header_len = header_len;
            slot.throttle.started_at = Some(Instant::now());
        }

        let token = conn_token(key);
        // Arm writable, mirroring transition_to_sending's initial interest.
        server
            .poll
            .registry()
            .register(
                server.conns[key].stream.as_mut().unwrap(),
                token,
                Interest::WRITABLE,
            )
            .unwrap();

        // Window 1: send the free header + the 10-byte body allowance, then
        // exhaust it and pause (handle_send deregisters the stream here).
        handle_send(&mut server, key).unwrap();
        assert_eq!(server.conns[key].state, ConnState::Sending);
        handle_send(&mut server, key).unwrap();
        assert_eq!(server.conns[key].state, ConnState::Pausing);

        // Simulate the pause window elapsing with enough elapsed send-time
        // that the resume write is throttle-capped short of completion (so
        // handle_send re-arms writable via the "more to send" path rather
        // than finishing the response).
        server.conns[key].throttle.started_at = Some(Instant::now() - Duration::from_secs(2));
        server.conns[key].throttle.pause_until = Some(Instant::now() - Duration::from_millis(1));

        handle_pause(&mut server, key).unwrap();

        // After resume the stream MUST be registered for writable; otherwise
        // no future event re-enters handle_send and the response hangs.
        // `reregister` succeeds only on an already-registered source.
        assert_eq!(
            server.conns[key].state,
            ConnState::Sending,
            "resume should leave the connection Sending"
        );
        let rereg = server.poll.registry().reregister(
            server.conns[key].stream.as_mut().unwrap(),
            token,
            Interest::WRITABLE,
        );
        assert!(
            rereg.is_ok(),
            "stream must be registered after resume; got {rereg:?}"
        );
        drop(client);
    }

    #[test]
    fn throttle_match_path_strips_a_single_leading_slash() {
        assert_eq!(throttle_match_path("/index.html"), "index.html");
        assert_eq!(throttle_match_path("/cgi-bin/foo"), "cgi-bin/foo");
        // A path with no leading slash is returned unchanged.
        assert_eq!(throttle_match_path("index.html"), "index.html");
        assert_eq!(throttle_match_path("/"), "");
    }

    #[test]
    fn single_star_rule_admits_slash_prefixed_path_after_normalization() {
        // Loaded patterns are slash-stripped (throttle.rs parse_line), and a
        // single `*` does not cross '/'. The raw request path keeps its
        // leading slash, so only the normalized form matches.
        let mut table = throttle_table("*.html 1000\n");
        assert!(matches!(
            table.check_request("/index.html"),
            ThrottleDecision::Unlimited
        ));
        assert!(matches!(
            table.check_request(throttle_match_path("/index.html")),
            ThrottleDecision::Allow { .. }
        ));
    }

    #[test]
    fn static_precheck_matches_request_path_and_initializes_throttle_state() {
        let root = tempfile::tempdir().unwrap();
        let file_path = root.path().join("index.html");
        std::fs::write(&file_path, b"hello").unwrap();

        let config = ServerConfig {
            dir: root.path().to_path_buf(),
            ..ServerConfig::default()
        };
        let mut server = test_server(config);
        server.throttles = Some(throttle_table("*.html 1000\n"));

        let key = server.conns.insert(ConnSlot::new());
        let slot = &mut server.conns[key];
        slot.http.orig_filename = "/index.html".to_string();
        slot.http.encoded_url = "/index.html".to_string();
        slot.http.method = Method::Get;

        serve_static(&mut server, key, &file_path);

        let slot = &server.conns[key];
        assert_eq!(slot.state, ConnState::Sending);
        assert!(slot.throttle.checked);
        assert!(slot.throttle.is_throttled());
        assert_eq!(slot.throttle.header_len, slot.http.response_header_len);
        assert!(slot.throttle.header_len > 0);
        assert!(slot.throttle.started_at.is_some());
        assert!(slot.throttle.active_at.is_some());
        assert_eq!(
            server.throttles.as_ref().unwrap().entries()[0].num_sending,
            1
        );
    }

    #[test]
    fn static_precheck_reject_uses_sending_transition_bookkeeping() {
        let (_root, mut server, file_path) = static_file_server("index.html", b"hello");
        server.throttles = Some(throttle_table("*.html 5000-0\n"));

        let key = server.conns.insert(ConnSlot::new());
        let slot = &mut server.conns[key];
        slot.http.orig_filename = "/index.html".to_string();
        slot.http.encoded_url = "/index.html".to_string();
        slot.http.method = Method::Get;

        serve_static(&mut server, key, &file_path);

        let slot = &server.conns[key];
        assert_eq!(slot.state, ConnState::Sending);
        assert_eq!(slot.http.status_code, 503);
        assert!(slot.throttle.checked);
        assert_eq!(slot.http.bytes_sent, 0);
        assert_eq!(server.stats.requests, 1);
        assert_eq!(slot.http.response_len, slot.http.response.len());
        assert_eq!(
            slot.http.response_header_len,
            header_end_offset(&slot.http.response).unwrap()
        );
    }

    #[test]
    fn throttled_head_static_request_rejects_before_early_response() {
        let (_root, mut server, file_path) = static_file_server("index.html", b"hello");
        server.throttles = Some(throttle_table("*.html 5000-0\n"));

        let key = server.conns.insert(ConnSlot::new());
        let slot = &mut server.conns[key];
        slot.http.orig_filename = "/index.html".to_string();
        slot.http.encoded_url = "/index.html".to_string();
        slot.http.method = Method::Head;

        serve_static(&mut server, key, &file_path);

        let slot = &server.conns[key];
        assert_eq!(slot.state, ConnState::Sending);
        assert_eq!(slot.http.status_code, 503);
        assert!(slot.throttle.checked);
        assert_eq!(slot.http.response_len, slot.http.response.len());
        assert_eq!(
            slot.http.response_header_len,
            header_end_offset(&slot.http.response).unwrap()
        );
    }

    #[test]
    fn throttled_not_modified_static_request_rejects_before_early_response() {
        let (_root, mut server, file_path) = static_file_server("index.html", b"hello");
        server.throttles = Some(throttle_table("*.html 5000-0\n"));

        let key = server.conns.insert(ConnSlot::new());
        let slot = &mut server.conns[key];
        slot.http.orig_filename = "/index.html".to_string();
        slot.http.encoded_url = "/index.html".to_string();
        slot.http.method = Method::Get;
        slot.http.if_modified_since = Some(i64::MAX);

        serve_static(&mut server, key, &file_path);

        let slot = &server.conns[key];
        assert_eq!(slot.state, ConnState::Sending);
        assert_eq!(slot.http.status_code, 503);
        assert!(slot.throttle.checked);
        assert_eq!(slot.http.response_len, slot.http.response.len());
        assert_eq!(
            slot.http.response_header_len,
            header_end_offset(&slot.http.response).unwrap()
        );
    }

    #[test]
    fn cgi_throttle_admission_uses_normalized_script_path() {
        // resolved_script arrives as "/cgi-bin/foo"; admission must match the
        // slash-stripped form against the loaded (slash-free) pattern.
        let mut table = throttle_table("cgi-bin/** 1000\n");
        assert!(matches!(
            table.check_request("/cgi-bin/foo"),
            ThrottleDecision::Unlimited
        ));
        assert!(matches!(
            table.check_request(throttle_match_path("/cgi-bin/foo")),
            ThrottleDecision::Allow { .. }
        ));
    }

    #[test]
    fn paused_connection_event_does_not_close_before_deadline() {
        let mut server = test_server(ServerConfig::default());
        let key = server.conns.insert(ConnSlot::new());
        let slot = &mut server.conns[key];
        slot.state = ConnState::Pausing;
        slot.throttle.pause_until = Some(Instant::now() + Duration::from_secs(30));

        handle_connection_event(&mut server, key).unwrap();

        assert!(server.conns.contains(key));
        assert_eq!(server.conns[key].state, ConnState::Pausing);
    }

    #[test]
    fn close_connection_logs_actual_bytes_sent() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("access.log");
        let config = ServerConfig {
            logfile: Some(log_path.clone()),
            ..ServerConfig::default()
        };
        let mut server = test_server(config);
        let key = server.conns.insert(ConnSlot::new());
        let slot = &mut server.conns[key];
        slot.http.status_code = 200;
        slot.http.bytes_sent = 321;
        slot.http.method = Method::Get;
        slot.http.encoded_url = "/index.html".to_string();
        slot.http.http_version = "HTTP/1.0".to_string();
        slot.http.mime_flag = true;

        close_connection(&mut server, key);

        let mut contents = String::new();
        std::fs::File::open(log_path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert!(
            contents.contains("\"GET /index.html HTTP/1.0\" 200 321"),
            "{contents}"
        );
    }

    #[test]
    fn close_connection_logs_status_from_response_when_unset() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("access.log");
        let config = ServerConfig {
            logfile: Some(log_path.clone()),
            ..ServerConfig::default()
        };
        let mut server = test_server(config);
        let key = server.conns.insert(ConnSlot::new());
        let slot = &mut server.conns[key];
        slot.http.response = b"HTTP/1.0 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec();
        slot.http.response_len = slot.http.response.len();
        slot.http.bytes_sent = slot.http.response_len as i64;
        slot.http.method = Method::Get;
        slot.http.encoded_url = "/missing.html".to_string();
        slot.http.http_version = "HTTP/1.0".to_string();
        slot.http.mime_flag = true;

        close_connection(&mut server, key);

        let mut contents = String::new();
        std::fs::File::open(log_path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert!(
            contents.contains("\"GET /missing.html HTTP/1.0\" 404 "),
            "{contents}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn no_symlink_check_allows_static_symlink_outside_root() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_file = outside.path().join("outside.txt");
        std::fs::write(&outside_file, b"outside").unwrap();
        let link_path = root.path().join("link.txt");
        symlink(&outside_file, &link_path).unwrap();

        let checked_config = ServerConfig {
            dir: root.path().to_path_buf(),
            no_symlink_check: false,
            ..ServerConfig::default()
        };
        let mut checked_server = test_server(checked_config);
        let checked_key = checked_server.conns.insert(ConnSlot::new());
        checked_server.conns[checked_key].http.method = Method::Get;
        checked_server.conns[checked_key].http.encoded_url = "/link.txt".to_string();
        serve_static(&mut checked_server, checked_key, &link_path);
        assert_eq!(
            logged_status_code(&checked_server.conns[checked_key].http),
            403
        );

        let unchecked_config = ServerConfig {
            dir: root.path().to_path_buf(),
            no_symlink_check: true,
            ..ServerConfig::default()
        };
        let mut unchecked_server = test_server(unchecked_config);
        let unchecked_key = unchecked_server.conns.insert(ConnSlot::new());
        unchecked_server.conns[unchecked_key].http.method = Method::Get;
        unchecked_server.conns[unchecked_key].http.encoded_url = "/link.txt".to_string();
        serve_static(&mut unchecked_server, unchecked_key, &link_path);
        assert_eq!(
            logged_status_code(&unchecked_server.conns[unchecked_key].http),
            200
        );
    }

    #[test]
    fn cgi_response_is_not_re_admitted_to_throttle_table() {
        // A response flagged is_cgi must bypass the throttle admission check
        // so its output is not double-counted: CGI output is already charged
        // a flat CGI_BYTECOUNT at CGI completion, and re-admitting it would
        // bump num_sending again and rate-limit the response stream.
        let mut server = test_server(ServerConfig::default());
        server.throttles = Some(throttle_table("**.html 1000\n"));
        let key = server.conns.insert(ConnSlot::new());
        let slot = &mut server.conns[key];
        slot.is_cgi = true;
        // Same pattern/filename that DOES admit an ordinary request (see
        // throttled_static_response_uses_saved_header_length), proving the
        // skip is what keeps num_sending at 0.
        slot.http.orig_filename = "/index.html".to_string();
        slot.http.response = b"HTTP/1.0 200 OK\r\n\r\nbody".to_vec();
        slot.http.response_len = slot.http.response.len();
        slot.http.mime_flag = true;

        transition_to_sending(&mut server, key);

        // No admission: the throttle slot is untouched and the connection is
        // not flagged throttled, so handle_send/close_connection won't act.
        assert_eq!(
            server.throttles.as_ref().unwrap().entries()[0].num_sending,
            0
        );
        assert!(!server.conns[key].throttle.is_throttled());
    }

    #[test]
    fn close_connection_logs_entity_bytes_excluding_headers() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("access.log");
        let config = ServerConfig {
            logfile: Some(log_path.clone()),
            ..ServerConfig::default()
        };
        let mut server = test_server(config);
        let key = server.conns.insert(ConnSlot::new());

        let headers =
            b"HTTP/1.0 404 Not Found\r\nContent-Type: text/html\r\nContent-Length: 12\r\n\r\n";
        let body = b"hello world\n"; // 12 bytes
        let mut response = headers.to_vec();
        response.extend_from_slice(body);

        let slot = &mut server.conns[key];
        slot.http.response = response;
        slot.http.response_len = slot.http.response.len();
        slot.http.mime_flag = true;
        slot.http.method = Method::Get;
        slot.http.encoded_url = "/missing.html".to_string();
        slot.http.http_version = "HTTP/1.0".to_string();

        // transition_to_sending derives response_header_len from the blank
        // line (error responses don't set it explicitly).
        transition_to_sending(&mut server, key);
        assert_eq!(server.conns[key].http.response_header_len, headers.len());

        // Simulate the entire response being written to the socket.
        server.conns[key].http.bytes_sent = server.conns[key].http.response_len as i64;

        close_connection(&mut server, key);

        let mut contents = String::new();
        std::fs::File::open(&log_path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        // The CERN bytes field must hold the entity body length, not the
        // header-inclusive total. Trailing " disambiguates from prefixes.
        let expected = format!("\"GET /missing.html HTTP/1.0\" 404 {} \"", body.len());
        assert!(
            contents.contains(&expected),
            "expected entity bytes {} in log: {contents}",
            body.len()
        );
        let total = headers.len() + body.len();
        let rejected = format!("\"GET /missing.html HTTP/1.0\" 404 {} \"", total);
        assert!(
            !contents.contains(&rejected),
            "log must not contain header-inclusive bytes ({total}): {contents}"
        );
    }

    #[test]
    fn throttle_deadline_included_when_throttles_configured() {
        let mut server = test_server(ServerConfig::default());
        server.throttles = Some(throttle_table("**.html 1000\n"));
        let last_update = Instant::now();
        let deadline = next_throttle_deadline(&server, last_update);
        assert!(deadline.is_some(), "throttle deadline must be present");
        let d = deadline.unwrap();
        // Should be approximately THROTTLE_TIME (2 s), not the 60 s fallback.
        assert!(
            d <= Duration::from_secs(THROTTLE_TIME as u64),
            "deadline {d:?} must not exceed THROTTLE_TIME"
        );
        assert!(
            d > Duration::from_secs(0),
            "deadline {d:?} should still have time remaining"
        );
    }

    #[test]
    fn throttle_deadline_clamps_to_zero_when_overdue() {
        let mut server = test_server(ServerConfig::default());
        server.throttles = Some(throttle_table("**.html 1000\n"));
        // Simulate an overdue update: last_update was well in the past.
        let stale = Instant::now() - Duration::from_secs(120);
        let deadline = next_throttle_deadline(&server, stale);
        assert_eq!(deadline, Some(Duration::ZERO));
    }

    #[test]
    fn throttle_deadline_absent_when_no_throttle_table() {
        let server = test_server(ServerConfig::default());
        // No throttle_file → server.throttles is None.
        let deadline = next_throttle_deadline(&server, Instant::now());
        assert!(deadline.is_none());
    }

    #[test]
    fn throttle_reject_logs_503_with_correct_header_length() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("access.log");
        let config = ServerConfig {
            logfile: Some(log_path.clone()),
            ..ServerConfig::default()
        };
        let mut server = test_server(config);
        // min_limit=5000, max_limit=0, rate=0 → rate < min_limit → Reject.
        server.throttles = Some(throttle_table("**.html 5000-0\n"));

        let key = server.conns.insert(ConnSlot::new());
        let slot = &mut server.conns[key];
        slot.http.orig_filename = "/index.html".to_string();
        // Prepare a 200 response with a known header block so we can verify
        // the 503 replacement recomputes header length from the new response.
        let orig_headers =
            b"HTTP/1.0 200 OK\r\nContent-Type: text/html\r\nContent-Length: 5\r\n\r\n";
        let orig_body = b"hello";
        slot.http.response = [orig_headers.as_ref(), orig_body].concat();
        slot.http.response_len = slot.http.response.len();
        slot.http.response_header_len = orig_headers.len();
        slot.http.status_code = 200;
        slot.http.mime_flag = true;
        slot.http.method = Method::Get;
        slot.http.encoded_url = "/index.html".to_string();
        slot.http.http_version = "HTTP/1.0".to_string();

        transition_to_sending(&mut server, key);

        let slot = &server.conns[key];
        // The 503 response must have replaced the 200.
        assert_eq!(slot.http.status_code, 503);
        // response_header_len must reflect the 503 header, not the original.
        let expected_hdr_len = header_end_offset(&slot.http.response).unwrap();
        assert_eq!(
            slot.http.response_header_len, expected_hdr_len,
            "header length must match the 503 replacement, not the original 200"
        );
        assert_eq!(slot.http.response_len, slot.http.response.len());

        // Simulate the full 503 response being sent, then close to log it.
        server.conns[key].http.bytes_sent = server.conns[key].http.response_len as i64;
        close_connection(&mut server, key);

        let mut contents = String::new();
        std::fs::File::open(&log_path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        // The access log must show 503, not the original 200.
        let expected = "\"GET /index.html HTTP/1.0\" 503";
        assert!(
            contents.contains(expected),
            "log must contain 503 status: {contents}"
        );
        assert!(
            !contents.contains("\"GET /index.html HTTP/1.0\" 200"),
            "log must not contain original 200 status: {contents}"
        );
    }

    #[test]
    fn throttle_reject_sets_correct_header_length_for_304() {
        // A 304 Not Modified response has no body; ensure the 503 replacement
        // still recomputes header length correctly.
        let mut server = test_server(ServerConfig::default());
        // min_limit=5000, max_limit=0, rate=0 → rate < min_limit → Reject.
        server.throttles = Some(throttle_table("**.html 5000-0\n"));

        let key = server.conns.insert(ConnSlot::new());
        let slot = &mut server.conns[key];
        slot.http.orig_filename = "/index.html".to_string();
        let orig_headers = b"HTTP/1.0 304 Not Modified\r\n\r\n";
        slot.http.response = orig_headers.to_vec();
        slot.http.response_len = slot.http.response.len();
        slot.http.response_header_len = orig_headers.len();
        slot.http.status_code = 304;
        slot.http.mime_flag = true;

        transition_to_sending(&mut server, key);

        let slot = &server.conns[key];
        assert_eq!(slot.http.status_code, 503);
        let expected_hdr_len = header_end_offset(&slot.http.response).unwrap();
        assert_eq!(slot.http.response_header_len, expected_hdr_len);
    }
    #[test]
    fn deregistered_listener_with_pending_conn_does_not_spin() {
        // A level-triggered readable listener keeps the event loop spinning.
        // Deregistering the listener on drain start must prevent this.
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = std_listener.local_addr().unwrap();
        let mut listener = mio::net::TcpListener::from_std(std_listener);

        let mut poll = mio::Poll::new().unwrap();
        poll.registry()
            .register(&mut listener, mio::Token(0), mio::Interest::READABLE)
            .unwrap();

        // Connect a client so the listener has a pending connection (readable).
        let _client = std::net::TcpStream::connect(addr).unwrap();

        // Before deregister: listener is reported as readable.
        let mut events = mio::Events::with_capacity(8);
        poll.poll(&mut events, Some(std::time::Duration::from_millis(100)))
            .unwrap();
        assert_eq!(
            events.iter().count(),
            1,
            "listener should be readable before deregister"
        );

        // Deregister (what the drain fix does in the event loop).
        poll.registry().deregister(&mut listener).unwrap();

        // After deregister: no events despite the pending connection.
        let mut events2 = mio::Events::with_capacity(8);
        poll.poll(&mut events2, Some(std::time::Duration::from_millis(100)))
            .unwrap();
        assert_eq!(
            events2.iter().count(),
            0,
            "deregistered listener must not produce events"
        );
    }

    #[test]
    fn close_connection_logs_x_forwarded_for() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("access.log");
        let config = ServerConfig {
            logfile: Some(log_path.clone()),
            ..ServerConfig::default()
        };
        let mut server = test_server(config);
        let key = server.conns.insert(ConnSlot::new());

        let slot = &mut server.conns[key];
        slot.http.status_code = 200;
        slot.http.bytes_sent = 100;
        slot.http.method = Method::Get;
        slot.http.encoded_url = "/index.html".to_string();
        slot.http.http_version = "HTTP/1.0".to_string();
        slot.http.mime_flag = true;
        slot.http.x_forwarded_for = "203.0.113.42".to_string();
        slot.peer_addr = Some("127.0.0.1:9999".parse().unwrap());

        close_connection(&mut server, key);

        let mut contents = String::new();
        std::fs::File::open(&log_path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert!(
            contents.starts_with("203.0.113.42 "),
            "log must start with XFF IP: {contents}"
        );
        assert!(
            !contents.contains("127.0.0.1"),
            "log must not contain socket peer: {contents}"
        );
    }

    #[test]
    fn close_connection_falls_back_to_peer_addr_without_xff() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("access.log");
        let config = ServerConfig {
            logfile: Some(log_path.clone()),
            ..ServerConfig::default()
        };
        let mut server = test_server(config);
        let key = server.conns.insert(ConnSlot::new());

        let slot = &mut server.conns[key];
        slot.http.status_code = 200;
        slot.http.bytes_sent = 100;
        slot.http.method = Method::Get;
        slot.http.encoded_url = "/index.html".to_string();
        slot.http.http_version = "HTTP/1.0".to_string();
        slot.http.mime_flag = true;
        // x_forwarded_for left empty
        slot.peer_addr = Some("127.0.0.1:9999".parse().unwrap());

        close_connection(&mut server, key);

        let mut contents = String::new();
        std::fs::File::open(&log_path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert!(
            contents.starts_with("127.0.0.1 "),
            "log must start with socket peer IP when no XFF: {contents}"
        );
    }
}
