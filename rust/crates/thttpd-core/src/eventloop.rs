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
use thttpd_http::response::{error_page, ResponseBuilder};
use thttpd_http::url::{normalize_path, percent_decode};
use thttpd_match::match_pattern;
use thttpd_mime::mime_type;
use thttpd_tdate::format_http_date;
use std::time::Duration;

/// Maximum number of connections we accept.
const MAX_CONNECTIONS: usize = 4096;

/// Size of the read buffer per connection — matches C's 60000.
const READ_BUF_SIZE: usize = 60000;

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
            // Re-open log file (no-op for now, log to stderr)
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
            let response = build_error_response(400, "Bad Request", None);
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
        got_request(&http.read_buf, http.checked_idx, http.read_idx)
    };

    {
        let http = &mut server.conns[slab_key].http;
        http.checked_idx = new_checked;
        http.parse_state = new_state;
    }

    match result {
        GotRequest::NoRequest => Ok(()),
        GotRequest::BadRequest => {
            let response = build_error_response(400, "Bad Request", None);
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
    // Parse request line and host header
    let (url_str, version_str, host_str) = {
        let slot = &server.conns[slab_key];
        let http = &slot.http;
        let buf = &http.read_buf[..http.checked_idx];

        let request_line_end = buf.iter().position(|&b| b == b'\r').unwrap_or(buf.len());
        let request_line = String::from_utf8_lossy(&buf[..request_line_end]);
        let mut parts = request_line.split_whitespace();

        let _method_str = parts.next().unwrap_or("GET");
        let url = parts.next().unwrap_or("/").to_string();
        let version = parts.next().unwrap_or("HTTP/1.0").to_string();

        let header_start = buf.iter().position(|&b| b == b'\n').map(|p| p + 1).unwrap_or(0);
        let headers_bytes = &buf[header_start..];
        let host = extract_header(headers_bytes, "Host").unwrap_or_default();

        (url, version, host)
    };

    // Update the HttpConn fields
    {
        let slot = &mut server.conns[slab_key];
        slot.http.method = parse_method(&slot.http.read_buf, slot.http.checked_idx);
        slot.http.http_version = version_str;
        slot.http.encoded_url = url_str.clone();
        slot.http.host = host_str.clone();

        slot.http.decoded_url = percent_decode(&url_str);

        if let Some(qpos) = slot.http.decoded_url.find('?') {
            slot.http.query = slot.http.decoded_url[qpos + 1..].to_string();
            slot.http.decoded_url.truncate(qpos);
        }
    }

    // Resolve the file path
    let file_path = {
        let slot = &server.conns[slab_key];
        let decoded = &slot.http.decoded_url;

        let normalized = match normalize_path(decoded) {
            Some(p) => p,
            None => {
                let response = build_error_response(403, "Forbidden", None);
                let slot = &mut server.conns[slab_key];
                slot.http.response = response;
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

    // Check CGI pattern
    let is_cgi = {
        let slot = &server.conns[slab_key];
        match &server.config.cgi_pattern {
            Some(pattern) => match_pattern(pattern, &slot.http.orig_filename),
            None => false,
        }
    };

    if is_cgi {
        dispatch_cgi(server, slab_key, &file_path);
        return;
    }

    // Static file serving
    serve_static(server, slab_key, &file_path);
}

/// Serve a static file.
fn serve_static(server: &mut Server, slab_key: usize, file_path: &Path) {
    // Check if the path is a directory — generate listing
    if file_path.is_dir() {
        let url_path = server.conns[slab_key].http.orig_filename.clone();
        let dir = file_path.to_path_buf();

        match thttpd_http::dirlist::generate_listing(&dir, &url_path) {
            Ok(body) => {
                let response = ResponseBuilder::new()
                    .status(200, "OK")
                    .header("Content-Type", "text/html")
                    .header("Content-Length", &body.len().to_string())
                    .body(body)
                    .build();
                let slot = &mut server.conns[slab_key];
                slot.http.response = response;
                slot.http.response_len = slot.http.response.len();
                transition_to_sending(server, slab_key);
                return;
            }
            Err(e) => {
                eprintln!("thttpd: directory listing error: {e}");
                let response = build_error_response(500, "Internal Server Error", None);
                let slot = &mut server.conns[slab_key];
                slot.http.response = response;
                slot.http.response_len = slot.http.response.len();
                transition_to_sending(server, slab_key);
                return;
            }
        }
    }

    // Try to mmap the file
    let file_path_owned = file_path.to_path_buf();
    let mmap_result = server.mmc.map(&file_path_owned);

    match mmap_result {
        Ok(mmap) => {
            let body = mmap.to_vec();
            let filename = file_path.to_string_lossy();
            let content_type = mime_type(&filename);
            let content_len = body.len();
            let now = format_http_date(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64,
            );

            let mut builder = ResponseBuilder::new()
                .status(200, "OK")
                .header("Content-Type", content_type)
                .header("Content-Length", &content_len.to_string())
                .header("Date", &now)
                .header("Server", "thttpd-rs");

            if server.config.max_age >= 0 {
                builder = builder.header("Cache-Control", &format!("max-age={}", server.config.max_age));
            }

            if let Some(ref p3p) = server.config.p3p {
                builder = builder.header("P3P", p3p);
            }

            let response = builder.body(body).build();

            let slot = &mut server.conns[slab_key];
            slot.http.file_address = Some(mmap);
            slot.http.response = response;
            slot.http.response_len = slot.http.response.len();
            slot.http.bytes_sent = 0;
            slot.http.status_code = 200;
            transition_to_sending(server, slab_key);
        }
        Err(_) => {
            let response = build_error_response(404, "Not Found", None);
            let slot = &mut server.conns[slab_key];
            slot.http.response = response;
            slot.http.response_len = slot.http.response.len();
            transition_to_sending(server, slab_key);
        }
    }
}

/// Dispatch a CGI request.
fn dispatch_cgi(server: &mut Server, slab_key: usize, script_path: &Path) {
    let (method, orig_filename, query, host, peer_addr, content_type, content_length,
         user_agent, referer, accept, accept_encoding, cookie, path_info) = {
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
            slot.http.cookie.clone(),
            slot.http.path_info.clone(),
        )
    };

    let mut http_headers = std::collections::HashMap::new();
    if !host.is_empty() {
        http_headers.insert("Host".to_string(), host);
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
    if !cookie.is_empty() {
        http_headers.insert("Cookie".to_string(), cookie);
    }

    let ctx = thttpd_http::cgi::CgiContext {
        server_software: "thttpd-rs/0.1".to_string(),
        server_name: server.config.hostname.clone().unwrap_or_else(|| "localhost".to_string()),
        gateway_interface: "CGI/1.1".to_string(),
        server_protocol: "HTTP/1.0".to_string(),
        server_port: server.config.port,
        request_method: method,
        script_name: orig_filename.clone(),
        query_string: query,
        remote_addr: peer_addr,
        content_type: if content_type.is_empty() { None } else { Some(content_type) },
        content_length,
        http_headers,
        path_info: if path_info.is_empty() { None } else { Some(path_info) },
        path_translated: None,
        remote_user: None,
        auth_type: None,
    };

    let env = thttpd_http::cgi::build_envp(&ctx, &orig_filename);

    // Read POST body if present
    let post_body = server.conns.get(slab_key).and_then(|slot| {
        slot.http.content_length.and_then(|len| {
            let body_start = slot.http.checked_idx;
            if body_start + (len as usize) <= slot.http.read_idx {
                Some(slot.http.read_buf[body_start..body_start + (len as usize)].to_vec())
            } else {
                None
            }
        })
    });

    match thttpd_http::cgi::execute_cgi(script_path, env, post_body.as_deref()) {
        Ok(mut cgi_result) => {
            let mut output = Vec::new();
            if let Some(stdout) = cgi_result.child.stdout.take() {
                let mut stdout = stdout;
                let _ = stdout.read_to_end(&mut output);
            }
            let _ = cgi_result.child.wait();

            let response = if cgi_result.is_nph {
                output
            } else {
                let (headers, body) = parse_cgi_output(&output);
                let mut builder = ResponseBuilder::new().status(200, "OK");
                for (name, value) in headers {
                    if name.eq_ignore_ascii_case("status") {
                        if let Some(space_pos) = value.find(' ') {
                            if let Ok(code) = value[..space_pos].parse::<u16>() {
                                builder = builder.status(code, &value[space_pos + 1..]);
                            }
                        }
                    } else {
                        builder = builder.header(&name, &value);
                    }
                }
                builder.body(body).build()
            };

            let slot = &mut server.conns[slab_key];
            slot.http.response = response;
            slot.http.response_len = slot.http.response.len();
            transition_to_sending(server, slab_key);
        }
        Err(e) => {
            eprintln!("thttpd: CGI error: {e}");
            let response = build_error_response(500, "Internal Server Error", None);
            let slot = &mut server.conns[slab_key];
            slot.http.response = response;
            slot.http.response_len = slot.http.response.len();
            transition_to_sending(server, slab_key);
        }
    }
}

/// Parse CGI output into headers and body.
fn parse_cgi_output(output: &[u8]) -> (Vec<(String, String)>, Vec<u8>) {
    let blank_pos = output.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .or_else(|| output.windows(2).position(|w| w == b"\n\n"));

    match blank_pos {
        Some(pos) => {
            let separator_len = if output.get(pos..pos + 4) == Some(b"\r\n\r\n") { 4 } else { 2 };
            let header_bytes = &output[..pos];
            let body = output[pos + separator_len..].to_vec();
            let headers = parse_cgi_headers(header_bytes);
            (headers, body)
        }
        None => (Vec::new(), output.to_vec()),
    }
}

/// Parse CGI header section into name/value pairs.
fn parse_cgi_headers(header_bytes: &[u8]) -> Vec<(String, String)> {
    let header_str = String::from_utf8_lossy(header_bytes);
    let mut headers = Vec::new();
    for line in header_str.lines() {
        if let Some(colon_pos) = line.find(':') {
            let name = line[..colon_pos].trim().to_string();
            let value = line[colon_pos + 1..].trim().to_string();
            if !name.is_empty() {
                headers.push((name, value));
            }
        }
    }
    headers
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
fn build_error_response(status_code: u16, status_text: &str, extra: Option<&str>) -> Vec<u8> {
    let body = error_page(status_text, extra);
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
