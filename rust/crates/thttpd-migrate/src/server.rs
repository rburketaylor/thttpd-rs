//! Proxy listener and per-request handler.
//!
//! Phase 3 replaces the Phase 1 skeleton with real routing: exclude paths,
//! pick a backend via [`crate::router`], forward via [`crate::forwarder`], and
//! stream the response back. Per-request tasks are spawned into a `JoinSet`
//! so Phase 7 drain can await them.

use crate::backend::BackendPool;
use crate::config::{RoutingConfig, ShadowConfig};
use crate::forwarder::{ProxyBody, ProxyClient, body_to_proxy, forwarded_headers};
use crate::router;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{info, warn};

/// True if `path` matches any exclude pattern. Patterns ending in `/*` are
/// treated as directory-prefix matches (the trailing slash is kept, so
/// `/internal/*` matches `/internal/secret` but not `/internal` or
/// `/internalx`); everything else is an exact match.
pub fn is_excluded(path: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| {
        if let Some(prefix) = p.strip_suffix('*') {
            path.starts_with(prefix)
        } else {
            path == p
        }
    })
}

/// Record request counter + duration histogram for a forwarded request.
fn record_metrics(backend: &str, status: u16, started: std::time::Instant) {
    let status_class = match status {
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        _ => "5xx",
    };
    metrics::counter!("thttpd_migrate_requests_total", "backend" => backend.to_string())
        .increment(1);
    metrics::histogram!(
        "thttpd_migrate_request_duration_seconds",
        "backend" => backend.to_string(),
        "status_class" => status_class,
    )
    .record(started.elapsed().as_secs_f64());
}

fn not_found() -> Response<ProxyBody> {
    let mut resp = Response::new(
        Full::new(Bytes::from_static(b"not found"))
            .map_err(|never| match never {})
            .boxed(),
    );
    *resp.status_mut() = StatusCode::NOT_FOUND;
    resp
}

fn backend_unavailable() -> Response<ProxyBody> {
    let mut resp = Response::new(
        Full::new(Bytes::from_static(b"no available backend"))
            .map_err(|never| match never {})
            .boxed(),
    );
    *resp.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
    resp
}

fn bad_gateway() -> Response<ProxyBody> {
    let mut resp = Response::new(
        Full::new(Bytes::from_static(b"bad gateway"))
            .map_err(|never| match never {})
            .boxed(),
    );
    *resp.status_mut() = StatusCode::BAD_GATEWAY;
    resp
}

pub async fn handle(
    req: Request<Incoming>,
    pool: Arc<BackendPool>,
    routing: RoutingConfig,
    excluded: Vec<String>,
    shadow_cfg: ShadowConfig,
    client: ProxyClient,
) -> Response<ProxyBody> {
    if is_excluded(req.uri().path(), &excluded) {
        return not_found();
    }

    let mut decision = match router::decide(&req, &pool, &routing) {
        Some(d) => d,
        None => return backend_unavailable(),
    };
    let backend_name = decision.backend.name.clone();
    let started = std::time::Instant::now();

    // Collect forwarded headers, then determine the authoritative request-id.
    let (parts, body) = req.into_parts();
    let mut headers = forwarded_headers(&parts.headers);
    // Request-ID propagation: honor inbound X-Request-Id or generate one,
    // forward it to the backend, and return it in the response.
    let request_id = parts
        .headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    headers.retain(|(k, _)| !k.eq_ignore_ascii_case("x-request-id"));
    headers.push(("x-request-id".into(), request_id.clone()));
    // Reconcile the decision's id with the authoritative one so shadow
    // divergence logs correlate with the id the client/backends see.
    decision.request_id = request_id.clone();

    // Shadow mode: serve the primary, mirror to the shadow, diff, but never
    // affect the user. Both the request body and the primary response are
    // streamed to/from the primary IN FULL; only capped copies feed the shadow
    // comparison (bounded memory, no user-visible truncation).
    if decision.shadow.is_some() {
        return handle_shadow(
            decision,
            parts.method,
            parts.uri,
            headers,
            body,
            pool,
            backend_name,
            started,
            request_id,
            &client,
            shadow_cfg,
        )
        .await;
    }

    // Active-active / canary: stream the request body straight through to the
    // single backend (no buffering), and stream the response back.
    let proxy_req = crate::forwarder::rebuild_for_backend_streaming(
        &parts.uri,
        &parts.method,
        &headers,
        crate::forwarder::body_to_proxy(body),
        &decision.backend.config.read().address,
    );

    match crate::forwarder::forward(&decision, proxy_req, &client).await {
        Ok(resp) => {
            let (mut parts, body) = resp.into_parts();
            let is_5xx = parts.status.is_server_error();
            crate::forwarder::strip_hop_by_hop(&mut parts.headers);
            // Echo the request-id back to the client.
            parts
                .headers
                .insert("x-request-id", request_id.parse().unwrap());
            // Record outcome: 5xx counts as a failure; everything else (incl.
            // connection success with 2xx/3xx/4xx) is a success for the breaker.
            pool.record_outcome(&backend_name, !is_5xx);
            record_metrics(&backend_name, parts.status.as_u16(), started);
            if is_5xx {
                metrics::counter!("thttpd_migrate_5xx_responses_total", "backend" => backend_name.clone())
                    .increment(1);
            }
            let body = body_to_proxy(body);
            Response::from_parts(parts, body)
        }
        Err(e) => {
            pool.record_outcome(&backend_name, false);
            record_metrics(&backend_name, 502, started);
            metrics::counter!("thttpd_migrate_5xx_responses_total", "backend" => backend_name.clone())
                .increment(1);
            warn!(
                request_id = %request_id,
                error = %e,
                backend = %backend_name,
                "forward error"
            );
            bad_gateway()
        }
    }
}

/// Rebuild the user-facing response header map from the captured primary
/// headers in shadow mode.
///
/// Repeated fields (e.g. multiple `Set-Cookie`) are *appended* so none are
/// collapsed to a single value — `HeaderMap::insert` would drop all but one,
/// silently breaking apps that set several cookies specifically during shadow
/// verification. `x-request-id` is set with [`HeaderMap::insert`] because the
/// proxy owns that header (it overrides any the backend returned).
fn rebuild_shadow_response_headers(
    headers: &mut hyper::HeaderMap,
    primary_headers: &[(String, String)],
    request_id: &str,
) {
    for (k, v) in primary_headers {
        // x-request-id is owned by the proxy; set it once below.
        if k.eq_ignore_ascii_case("x-request-id") {
            continue;
        }
        if let (Ok(name), Ok(val)) = (
            k.parse::<hyper::header::HeaderName>(),
            v.parse::<hyper::header::HeaderValue>(),
        ) {
            headers.append(name, val);
        }
    }
    headers.insert("x-request-id", request_id.parse().unwrap());
}

/// Shadow-mode handler: serve the primary, mirror to the shadow, diff.
///
/// The user always receives the FULL primary response (status, headers, and
/// body): the body is read up to the shadow cap for diffing, then the capped
/// prefix + split tail + streaming remainder are reconstructed as the
/// user-facing body. The request body is forwarded to the primary in full the
/// same way; only capped copies feed the shadow comparison. Primary outcomes
/// and metrics ARE recorded (shadow mode still serves real user traffic).
#[allow(clippy::too_many_arguments)]
async fn handle_shadow(
    decision: crate::router::RoutingDecision,
    method: hyper::Method,
    uri: hyper::Uri,
    headers: Vec<(String, String)>,
    mut req_body: Incoming,
    pool: Arc<BackendPool>,
    backend_name: String,
    started: std::time::Instant,
    request_id: String,
    client: &ProxyClient,
    shadow_cfg: ShadowConfig,
) -> Response<ProxyBody> {
    let cap = shadow_cfg.max_body_bytes;
    let primary_addr = decision.backend.config.read().address.clone();

    // --- Request body: read up to cap (bounded), leave the rest streaming. ---
    let (req_prefix, req_tail, req_truncated) = match crate::shadow::read_capped(&mut req_body, cap)
        .await
    {
        Ok(body) => body,
        Err(e) => {
            pool.record_outcome(&backend_name, false);
            record_metrics(&backend_name, 502, started);
            metrics::counter!("thttpd_migrate_5xx_responses_total", "backend" => backend_name.clone())
                    .increment(1);
            warn!(
                request_id = %request_id,
                error = %e,
                backend = %backend_name,
                "request body read error"
            );
            return bad_gateway();
        }
    };
    let primary_req_body: ProxyBody = if req_truncated {
        // Primary gets the full body: capped prefix + split tail + remainder.
        let chunks = [req_prefix.clone(), req_tail.unwrap_or_default()];
        crate::forwarder::ChainedBody::new(chunks, Some(req_body)).boxed()
    } else {
        // Body fit within the cap; req_body is exhausted.
        Full::new(req_prefix.clone())
            .map_err(|never| match never {})
            .boxed()
    };
    let primary_req = crate::forwarder::rebuild_for_backend_streaming(
        &uri,
        &method,
        &headers,
        primary_req_body,
        &primary_addr,
    );

    match crate::forwarder::forward(&decision, primary_req, client).await {
        Ok(resp) => {
            let (mut parts, mut body) = resp.into_parts();
            let is_5xx = parts.status.is_server_error();
            crate::forwarder::strip_hop_by_hop(&mut parts.headers);
            let status = parts.status;
            let primary_headers: Vec<(String, String)> = parts
                .headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or_default().to_string()))
                .collect();
            // --- Response body: read up to cap (bounded), leave the rest streaming. ---
            let (resp_prefix, resp_tail, resp_truncated) = match crate::shadow::read_capped(
                &mut body, cap,
            )
            .await
            {
                Ok(body) => body,
                Err(e) => {
                    pool.record_outcome(&backend_name, false);
                    record_metrics(&backend_name, 502, started);
                    metrics::counter!("thttpd_migrate_5xx_responses_total", "backend" => backend_name.clone())
                            .increment(1);
                    warn!(
                        request_id = %request_id,
                        error = %e,
                        backend = %backend_name,
                        "primary response body read error"
                    );
                    return bad_gateway();
                }
            };

            // Mirror to shadow in a detached task with the CAPPED copies only.
            crate::shadow::dispatch_shadow(
                decision,
                method,
                uri,
                headers,
                req_prefix,
                status.as_u16(),
                primary_headers.clone(),
                resp_prefix.clone(),
                resp_truncated,
                client.clone(),
                shadow_cfg,
                pool.clone(),
            );

            // Record primary outcome + metrics (Claim 5): shadow mode still
            // serves real user traffic via the primary, so the breaker,
            // request counter, and 5xx counter must see it.
            pool.record_outcome(&backend_name, !is_5xx);
            record_metrics(&backend_name, status.as_u16(), started);
            if is_5xx {
                metrics::counter!("thttpd_migrate_5xx_responses_total", "backend" => backend_name.clone())
                    .increment(1);
            }

            // Rebuild the user-facing response: FULL primary body. The capped
            // prefix + split tail + streaming remainder reconstruct it
            // losslessly; memory stays bounded by the cap.
            let user_body: ProxyBody = if resp_truncated {
                let chunks = [resp_prefix, resp_tail.unwrap_or_default()];
                crate::forwarder::ChainedBody::new(chunks, Some(body)).boxed()
            } else {
                Full::new(resp_prefix)
                    .map_err(|never| match never {})
                    .boxed()
            };
            let mut resp = Response::new(user_body);
            *resp.status_mut() = status;
            rebuild_shadow_response_headers(resp.headers_mut(), &primary_headers, &request_id);
            resp
        }
        Err(e) => {
            pool.record_outcome(&backend_name, false);
            record_metrics(&backend_name, 502, started);
            metrics::counter!("thttpd_migrate_5xx_responses_total", "backend" => backend_name.clone())
                .increment(1);
            warn!(
                request_id = %request_id,
                error = %e,
                backend = %backend_name,
                "forward error"
            );
            bad_gateway()
        }
    }
}

/// Phase 1/2 skeleton entry: bind and return `200 ok` on every request.
/// Retained for `start()` before real routing is wired in Phase 3's
/// `run_proxy`. Real routing uses [`run_proxy`].
pub async fn run_skeleton(listen: SocketAddr) -> anyhow::Result<()> {
    let listener = TcpListener::bind(listen).await?;
    info!(addr = %listen, "thttpd-migrate skeleton listening");
    loop {
        let (stream, peer) = listener.accept().await?;
        let io = TokioIo::new(stream);
        tokio::spawn(async move {
            let svc = hyper::service::service_fn(|_req: Request<Incoming>| async {
                Ok::<_, std::convert::Infallible>(Response::new(
                    Full::new(Bytes::from_static(b"ok"))
                        .map_err(|never| match never {})
                        .boxed(),
                ))
            });
            if let Err(e) = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, svc)
                .await
            {
                tracing::warn!(?peer, error = %e, "connection error");
            }
        });
    }
}

/// Reap completed connection tasks from the JoinSet during steady-state
/// serving.
///
/// Per-connection tasks are spawned into the `JoinSet`, but `JoinSet`
/// retains finished tasks until they are joined. Without reaping, completed
/// connections accumulate for the entire process lifetime under normal
/// serving (they were previously only joined after drain started). This is a
/// non-blocking sweep (`try_join_next`): it never awaits the accept loop or
/// stalls new connections, and join errors never fail the server — a panicked
/// connection task is logged but the proxy keeps serving. The graceful drain
/// semantics (timeout, `abort_all`) are unchanged.
fn reap_finished_connections(set: &mut tokio::task::JoinSet<()>) {
    while let Some(res) = set.try_join_next() {
        match res {
            Ok(()) => {}
            Err(e) => {
                if e.is_panic() {
                    warn!(error = %e, "connection task panicked; continuing to serve");
                } else {
                    tracing::debug!(error = %e, "connection task join error");
                }
            }
        }
    }
}

/// Real proxy entry: bind and route every request through the active pool.
/// Stops accepting when `state.is_draining()` (Phase 7 graceful drain).
pub async fn run_proxy(
    listen: SocketAddr,
    pool: Arc<BackendPool>,
    routing: RoutingConfig,
    shadow_cfg: ShadowConfig,
    state: Arc<crate::state::LiveState>,
    client: ProxyClient,
) -> anyhow::Result<()> {
    let listener = TcpListener::bind(listen).await?;
    run_proxy_with_listener(listener, pool, routing, shadow_cfg, state, client).await
}

async fn run_proxy_with_listener(
    listener: TcpListener,
    pool: Arc<BackendPool>,
    _routing: RoutingConfig,
    _shadow_cfg: ShadowConfig,
    state: Arc<crate::state::LiveState>,
    client: ProxyClient,
) -> anyhow::Result<()> {
    let listen = listener.local_addr()?;
    info!(addr = %listen, "thttpd-migrate proxy listening");
    let mut in_flight: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
    loop {
        if state.is_draining() {
            info!("draining: stopped accepting new connections");
            break;
        }
        // Reap completed connection tasks during steady-state serving so they
        // do not accumulate for the whole process lifetime. Non-blocking; join
        // errors are logged, never fatal. (Drain joining below is unchanged.)
        reap_finished_connections(&mut in_flight);
        // Accept new connections unless draining; use a select so a drain
        // request that arrives while blocked on accept is noticed promptly.
        let accept = listener.accept();
        tokio::pin!(accept);
        tokio::select! {
            biased;
            _ = drain_signal(&state) => {
                info!("draining: drain signal observed, stopping accept loop");
                break;
            }
            res = &mut accept => {
                let (stream, peer) = match res {
                    Ok(s) => s,
                    Err(e) => {
                        warn!(error = %e, "accept error");
                        continue;
                    }
                };
                let io = TokioIo::new(stream);
                let pool = pool.clone();
                let client = client.clone();
                let state_conn = state.clone();
                let state_service = state.clone();
                in_flight.spawn(async move {
                    let svc = hyper::service::service_fn(move |req: Request<Incoming>| {
                        let pool = pool.clone();
                        let cfg = state_service.config.load_full();
                        let routing = cfg.routing.clone();
                        let excluded = routing.exclude_paths.clone();
                        let shadow_cfg = cfg.shadow.clone();
                        let client = client.clone();
                        async move {
                            Ok::<_, std::convert::Infallible>(
                                handle(req, pool, routing, excluded, shadow_cfg, client).await,
                            )
                        }
                    });
                    let conn = hyper::server::conn::http1::Builder::new()
                        .keep_alive(true)
                        .serve_connection(io, svc);
                    tokio::pin!(conn);
                    // On drain, disable keep-alive: the current request (if any)
                    // finishes, then the connection closes instead of idling for
                    // the next request. Without this an idle/persistent
                    // keep-alive client keeps the JoinSet from draining.
                    let result = tokio::select! {
                        biased;
                        r = &mut conn => r,
                        _ = drain_signal(&state_conn) => {
                            conn.as_mut().graceful_shutdown();
                            (&mut conn).await
                        }
                    };
                    if let Err(e) = result {
                        tracing::debug!(?peer, error = %e, "connection error");
                    }
                });
            }
        }
    }
    drop(listener);
    // Finish in-flight requests, bounded by the drain grace so a stuck or
    // slow client can't hold the process open past the operator's timeout.
    let grace = state.drain_grace();
    info!(
        grace_secs = grace.as_secs(),
        "draining: awaiting in-flight requests"
    );
    let drained = tokio::time::timeout(grace, async {
        while in_flight.join_next().await.is_some() {}
    })
    .await;
    if drained.is_err() {
        warn!(
            grace_secs = grace.as_secs(),
            "drain grace elapsed; force-closing remaining connections"
        );
        in_flight.abort_all();
        while in_flight.join_next().await.is_some() {}
    }
    info!("drain complete");
    Ok(())
}

/// Resolves when the live state's drain flag is set. Polled via `select!`.
async fn drain_signal(state: &Arc<crate::state::LiveState>) {
    loop {
        if state.is_draining() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BackendConfig, ProxyConfig, RoutingConfig, RoutingMode};
    use http_body_util::{BodyExt, Full};
    use std::collections::HashMap;
    use std::io::ErrorKind;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    use tokio::sync::oneshot;

    #[test]
    fn exact_path_excluded() {
        let patterns = vec!["/metrics".to_string()];
        assert!(is_excluded("/metrics", &patterns));
        assert!(!is_excluded("/metricsx", &patterns));
    }

    #[test]
    fn prefix_path_excluded() {
        let patterns = vec!["/internal/*".to_string()];
        assert!(is_excluded("/internal/secret", &patterns));
        assert!(is_excluded("/internal/", &patterns));
        assert!(!is_excluded("/internal", &patterns));
        assert!(!is_excluded("/public", &patterns));
    }

    #[tokio::test]
    async fn reaper_drains_completed_joinset_tasks() {
        // P1 regression (JoinSet leak during normal serving): completed
        // connection tasks must be reaped during steady-state operation so
        // they do not accumulate for the whole process lifetime. The reaper is
        // non-blocking and never fails the server — including when a task
        // panicked (the JoinError is logged, not propagated).
        let mut set: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
        for _ in 0..5 {
            set.spawn(async {});
        }
        // Also a panicking task: its JoinError must be swallowed, not panic.
        set.spawn(async {
            panic!("boom");
        });
        // Let the spawned tasks run to completion.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        reap_finished_connections(&mut set);

        // All completed tasks (including the panicked one) are drained.
        assert!(
            set.try_join_next().is_none(),
            "no completed tasks should remain after reaping"
        );
    }

    #[test]
    fn reaper_is_noop_on_empty_set() {
        // Reaping an empty/aborted set must be a harmless no-op.
        let mut set: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
        reap_finished_connections(&mut set);
        assert!(set.try_join_next().is_none());
    }

    fn test_proxy_config(backend_addr: SocketAddr) -> ProxyConfig {
        let mut backends = HashMap::new();
        backends.insert(
            "backend".to_string(),
            BackendConfig {
                address: backend_addr.to_string(),
                weight: 1,
                health_path: "/".into(),
            },
        );
        ProxyConfig {
            listen: "127.0.0.1:0".into(),
            log_level: "info".into(),
            state_path: "/tmp/thttpd-migrate-test-state.json".into(),
            control_socket: "/tmp/thttpd-migrate-test-control.sock".into(),
            metrics: Default::default(),
            shadow: Default::default(),
            backends,
            routing: RoutingConfig {
                mode: RoutingMode::ActiveActive,
                primary_backend: None,
                shadow_backend: None,
                exclude_paths: Vec::new(),
            },
            health: Default::default(),
            circuit_breaker: Default::default(),
        }
    }

    async fn spawn_blocking_backend() -> (
        SocketAddr,
        tokio::task::JoinHandle<()>,
        oneshot::Receiver<()>,
        oneshot::Sender<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let started_tx = Arc::new(tokio::sync::Mutex::new(Some(started_tx)));
        let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));

        let handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let io = TokioIo::new(stream);
            let svc = hyper::service::service_fn(move |_req: Request<Incoming>| {
                let started_tx = started_tx.clone();
                let release_rx = release_rx.clone();
                async move {
                    if let Some(tx) = started_tx.lock().await.take() {
                        let _ = tx.send(());
                    }
                    if let Some(rx) = release_rx.lock().await.take() {
                        let _ = rx.await;
                    }
                    Ok::<_, std::convert::Infallible>(Response::new(
                        Full::new(Bytes::from_static(b"ok"))
                            .map_err(|never| match never {})
                            .boxed(),
                    ))
                }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, svc)
                .await;
        });

        (addr, handle, started_rx, release_tx)
    }

    async fn spawn_body_backend(body: &'static str) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(accepted) => accepted,
                    Err(_) => break,
                };
                tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let svc =
                        hyper::service::service_fn(move |_req: Request<Incoming>| async move {
                            Ok::<_, std::convert::Infallible>(Response::new(
                                Full::new(Bytes::from_static(body.as_bytes()))
                                    .map_err(|never| match never {})
                                    .boxed(),
                            ))
                        });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(io, svc)
                        .await;
                });
            }
        });
        (addr, handle)
    }

    async fn raw_get_body(addr: SocketAddr) -> String {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        let split = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|idx| idx + 4)
            .expect("HTTP response must contain header terminator");
        String::from_utf8(response[split..].to_vec()).unwrap()
    }

    async fn connection_refused_before(addr: SocketAddr, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match tokio::time::timeout(Duration::from_millis(100), TcpStream::connect(addr)).await {
                Ok(Ok(stream)) => {
                    drop(stream);
                }
                Ok(Err(e)) if e.kind() == ErrorKind::ConnectionRefused => return true,
                Ok(Err(_)) => {}
                Err(_) => {}
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn drain_closes_listener_before_waiting_for_in_flight_requests() {
        let (backend_addr, _backend_handle, backend_started, release_backend) =
            spawn_blocking_backend().await;
        let cfg = test_proxy_config(backend_addr);
        let routing = cfg.routing.clone();
        let pool = Arc::new(BackendPool::from_config(&cfg.backends));
        let state = Arc::new(crate::state::LiveState::new(cfg));
        state.set_drain_grace(5);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();
        let proxy_task = tokio::spawn(run_proxy_with_listener(
            listener,
            pool,
            routing,
            Default::default(),
            state.clone(),
            crate::forwarder::build_client(),
        ));

        let client_task = tokio::spawn(async move {
            let mut stream = TcpStream::connect(proxy_addr).await.unwrap();
            stream
                .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            let mut response = Vec::new();
            stream.read_to_end(&mut response).await.unwrap();
            response
        });

        tokio::time::timeout(Duration::from_secs(2), backend_started)
            .await
            .expect("backend should receive the in-flight request")
            .expect("backend start signal should be delivered");

        state.start_drain();
        assert!(
            connection_refused_before(proxy_addr, Duration::from_secs(2)).await,
            "new TCP connections must be refused before the in-flight request is released"
        );

        release_backend
            .send(())
            .expect("backend release signal should be accepted");
        let response = tokio::time::timeout(Duration::from_secs(2), client_task)
            .await
            .expect("client should finish after backend release")
            .expect("client task should not panic");
        assert!(
            response.starts_with(b"HTTP/1.1 200 OK"),
            "in-flight request should still complete successfully: {}",
            String::from_utf8_lossy(&response)
        );

        tokio::time::timeout(Duration::from_secs(2), proxy_task)
            .await
            .expect("proxy should finish after in-flight request drains")
            .expect("proxy task should not panic")
            .expect("proxy should return ok");
    }

    #[tokio::test]
    async fn shadow_rollback_changes_live_primary_backend() {
        let (c_addr, c_task) = spawn_body_backend("c").await;
        let (rust_addr, rust_task) = spawn_body_backend("rust").await;

        let mut backends = HashMap::new();
        backends.insert(
            "c".to_string(),
            BackendConfig {
                address: c_addr.to_string(),
                weight: 100,
                health_path: "/".into(),
            },
        );
        backends.insert(
            "rust".to_string(),
            BackendConfig {
                address: rust_addr.to_string(),
                weight: 1,
                health_path: "/".into(),
            },
        );
        let cfg = ProxyConfig {
            listen: "127.0.0.1:0".into(),
            log_level: "info".into(),
            state_path: "/tmp/thttpd-migrate-test-state.json".into(),
            control_socket: "/tmp/thttpd-migrate-test-control.sock".into(),
            metrics: Default::default(),
            shadow: Default::default(),
            backends,
            routing: RoutingConfig {
                mode: RoutingMode::Shadow,
                primary_backend: Some("c".into()),
                shadow_backend: Some("rust".into()),
                exclude_paths: Vec::new(),
            },
            health: Default::default(),
            circuit_breaker: Default::default(),
        };
        let routing = cfg.routing.clone();
        let shadow_cfg = cfg.shadow.clone();
        let pool = Arc::new(BackendPool::from_config(&cfg.backends));
        let state = Arc::new(crate::state::LiveState::new(cfg));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();
        let proxy_task = tokio::spawn(run_proxy_with_listener(
            listener,
            pool.clone(),
            routing,
            shadow_cfg,
            state.clone(),
            crate::forwarder::build_client(),
        ));

        assert_eq!(raw_get_body(proxy_addr).await, "c");

        state.rollback(&pool, "rust").unwrap();

        assert_eq!(raw_get_body(proxy_addr).await, "rust");
        let snap = state.config.load();
        assert_eq!(snap.routing.primary_backend.as_deref(), Some("rust"));
        assert_eq!(snap.routing.shadow_backend.as_deref(), Some("c"));

        state.start_drain();
        tokio::time::timeout(Duration::from_secs(2), proxy_task)
            .await
            .expect("proxy should finish after drain")
            .expect("proxy task should not panic")
            .expect("proxy should return ok");
        c_task.abort();
        rust_task.abort();
    }

    #[test]
    fn shadow_response_preserves_repeated_set_cookie() {
        // P1 (Preserve repeated response headers in shadow mode): `insert`
        // collapses repeated fields like Set-Cookie to a single value. The
        // helper must append every value so multi-cookie apps keep working for
        // real users during shadow verification.
        let primary_headers = vec![
            ("set-cookie".to_string(), "session=abc; Path=/".to_string()),
            ("set-cookie".to_string(), "tracking=xyz; Path=/".to_string()),
            ("content-type".to_string(), "text/html".to_string()),
            // A backend x-request-id must be overridden by the proxy's own.
            ("x-request-id".to_string(), "backend-id".to_string()),
        ];
        let mut headers = hyper::HeaderMap::new();
        rebuild_shadow_response_headers(&mut headers, &primary_headers, "proxy-id");

        let cookies: Vec<&str> = headers
            .get_all("set-cookie")
            .iter()
            .map(|v| v.to_str().unwrap())
            .collect();
        assert_eq!(
            cookies,
            vec!["session=abc; Path=/", "tracking=xyz; Path=/"],
            "both Set-Cookie values must be preserved"
        );
        assert_eq!(
            headers.get("content-type").unwrap(),
            "text/html",
            "non-repeated header preserved"
        );
        assert_eq!(
            headers.get("x-request-id").unwrap(),
            "proxy-id",
            "proxy-owned x-request-id must win over the backend's"
        );
        assert_eq!(
            headers.get_all("x-request-id").iter().count(),
            1,
            "exactly one x-request-id"
        );
    }
}
