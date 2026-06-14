//! Shadow-mode dispatcher.
//!
//! When routing mode is `shadow`, every request is served by the configured
//! primary backend and *mirrored* to the shadow backend. The shadow response
//! is captured and diffed against the primary response; divergences are logged
//! and metered but **never propagated to the client**.
//!
//! HTTP request/response bodies are one-shot streams, so shadow mode buffers
//! the inbound request body once (up to `shadow.max_body_bytes`), reuses it for
//! both the primary and shadow requests, and buffers the primary/shadow
//! response bodies up to the same cap for diffing. Above the cap, a truncation
//! divergence is recorded.

use crate::backend::BackendPool;
use crate::config::ShadowConfig;
use crate::diff::{self, Divergence, Field, RequestContext};
use crate::forwarder::{self, ProxyBody, ProxyClient};
use crate::router::RoutingDecision;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Body;
use hyper::{Method, Request, Response, Uri};
use std::sync::Arc;

/// Read up to `max_bytes` from `body`, returning the buffered prefix, the
/// leftover tail of any frame split across the cap boundary, and whether the
/// body was truncated. The remainder of the stream is left **unread** in
/// `body` so a caller can chain `prefix + tail + remainder` to reconstruct the
/// full body losslessly (used to serve the user/primary the complete body
/// while keeping only the capped copy for shadow diffing).
///
/// `tail` is the unbuffered portion of the frame that crossed the cap; it is
/// `None` when truncation landed on a frame boundary.
pub async fn read_capped<B>(
    body: &mut B,
    max_bytes: usize,
) -> Result<(Bytes, Option<Bytes>, bool), B::Error>
where
    B: Body<Data = Bytes> + Unpin,
{
    let mut buf = Vec::new();
    let mut tail = None;
    let mut truncated = false;
    while let Some(frame) = body.frame().await {
        let frame = frame?;
        if let Ok(data) = frame.into_data() {
            if buf.len() + data.len() <= max_bytes {
                buf.extend_from_slice(&data);
            } else {
                // This frame crosses the cap. Keep the head in the prefix, hand the
                // rest back as `tail`, and stop; the unconsumed stream stays in body.
                let remaining = max_bytes.saturating_sub(buf.len());
                buf.extend_from_slice(&data[..remaining]);
                tail = Some(data.slice(remaining..));
                truncated = true;
                break;
            }
        }
    }
    Ok((Bytes::from(buf), tail, truncated))
}

/// Rebuild a request for a different backend, reusing the buffered body.
/// (Mirrors [`crate::forwarder::rebuild_for_backend`] but takes owned types
/// so the caller can keep a copy of the original URI/method.)
pub fn rebuild_for_backend(
    original_uri: &Uri,
    method: &Method,
    headers: &[(String, String)],
    body: Bytes,
    backend_addr: &str,
) -> Request<ProxyBody> {
    forwarder::rebuild_for_backend(original_uri, method, headers, body, backend_addr)
}

/// Build shadow-request headers from the forwarded headers, reconciling the
/// HTTP framing with the (possibly capped) shadow body.
///
/// The forwarded `Content-Length` describes the *original, uncapped* client
/// body, but only a capped copy is mirrored to the shadow backend. Forwarding
/// the original `Content-Length` with a shorter body produces invalid framing:
/// the shadow backend waits for bytes that never arrive and hangs/fails. This
/// helper strips every `Content-Length` (and `Transfer-Encoding`, defensively —
/// `forwarded_headers` already drops hop-by-hop headers) and sets a single
/// `Content-Length` equal to the actual capped body length, so the shadow
/// request is well-formed regardless of truncation.
///
/// These headers are used **only** for the shadow request; the primary
/// request keeps the full body and its original headers untouched.
pub(crate) fn shadow_headers_for_body(
    headers: Vec<(String, String)>,
    body_len: usize,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = headers
        .into_iter()
        .filter(|(k, _)| {
            !k.eq_ignore_ascii_case("content-length")
                && !k.eq_ignore_ascii_case("transfer-encoding")
        })
        .collect();
    out.push(("content-length".into(), body_len.to_string()));
    out
}

/// Called from the server handler after the primary response is read.
///
/// Takes `decision` by value so the spawned future can own a copy of it
/// (spawns require `'static` futures; a borrowed `&RoutingDecision` would not).
///
/// `pool` records each shadow outcome against that backend's circuit breaker so
/// a failing shadow backend can trip its breaker and stop receiving mirrored
/// traffic (the router consults `breaker_can_route`/`breaker_admit` before
/// mirroring — never the side-effectful `breaker_allows`).
#[allow(clippy::too_many_arguments)]
pub fn dispatch_shadow(
    decision: RoutingDecision,
    method: Method,
    original_uri: Uri,
    headers: Vec<(String, String)>,
    body: Bytes,
    primary_status: u16,
    primary_headers: Vec<(String, String)>,
    primary_body: Bytes,
    primary_truncated: bool,
    client: ProxyClient,
    shadow_cfg: ShadowConfig,
    pool: Arc<BackendPool>,
) {
    let shadow = match decision.shadow.clone() {
        Some(s) => s,
        None => return,
    };
    let request_id = decision.request_id.clone();
    let path = original_uri.path().to_string();
    let method_str = method.to_string();
    tokio::spawn(async move {
        // Reconcile the shadow request framing with the (possibly capped)
        // body BEFORE rebuilding: the forwarded headers still carry the
        // original Content-Length, which would lie about a capped body and
        // hang the shadow backend. Only the shadow request is affected.
        let body_len = body.len();
        let shadow_headers = shadow_headers_for_body(headers, body_len);
        let shadow_req = rebuild_for_backend(
            &original_uri,
            &method,
            &shadow_headers,
            body,
            &shadow.config.read().address,
        );
        let result = forwarder::forward(&decision, shadow_req, &client).await;
        let ctx = RequestContext {
            path,
            method: method_str,
            request_id: request_id.clone(),
        };
        let divergences: Vec<Divergence> = match result {
            Ok(resp) => {
                let (parts, mut body_stream) = resp.into_parts();
                let shadow_status = parts.status.as_u16();
                let shadow_headers: Vec<(String, String)> = parts
                    .headers
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or_default().to_string()))
                    .collect();
                match read_capped(&mut body_stream, shadow_cfg.max_body_bytes).await {
                    Ok((shadow_body, _tail, shadow_truncated)) => {
                        // Record the shadow outcome against its breaker: a 5xx counts
                        // as a failure (mirrors active/canary semantics), everything
                        // else — including 4xx — as a success. The router consults
                        // this breaker before mirroring.
                        let shadow_success = !parts.status.is_server_error();
                        pool.record_outcome(&shadow.name, shadow_success);
                        diff::diff_responses(
                            primary_status,
                            &primary_headers,
                            &primary_body,
                            primary_truncated,
                            shadow_status,
                            &shadow_headers,
                            &shadow_body,
                            shadow_truncated,
                            &ctx,
                            shadow_cfg.max_body_bytes,
                        )
                        .await
                    }
                    Err(e) => {
                        pool.record_outcome(&shadow.name, false);
                        vec![Divergence {
                            field: Field::ConnectionLifecycle,
                            expected: "complete response body".into(),
                            actual: format!("body read error: {e}"),
                            path: ctx.path.clone(),
                            method: ctx.method.clone(),
                            truncated: false,
                        }]
                    }
                }
            }
            Err(e) => {
                // Connection / forwarding failure: count as a breaker failure
                // so the shadow backend can be backed off, not just logged as
                // a divergence. `NotRoutable` is excluded: forward() checks the
                // *primary* backend's routability (decision.backend), so it
                // reflects the primary's health, not a shadow failure, and must
                // not be charged to the shadow backend's breaker.
                if !matches!(e, forwarder::ForwardError::NotRoutable) {
                    pool.record_outcome(&shadow.name, false);
                }
                vec![Divergence {
                    field: Field::ConnectionLifecycle,
                    expected: "ok".into(),
                    actual: format!("error: {e}"),
                    path: ctx.path.clone(),
                    method: ctx.method.clone(),
                    truncated: false,
                }]
            }
        };
        for d in divergences {
            tracing::warn!(
                request_id = %request_id,
                backend = %shadow.name,
                field = d.field.as_str(),
                truncated = d.truncated,
                "shadow divergence"
            );
            metrics::counter!(
                "thttpd_migrate_shadow_divergences_total",
                "backend" => shadow.name.clone(),
                "field" => d.field.as_str(),
            )
            .increment(1);
        }
    });
}

/// Helper used by the server handler to build a static `Response<ProxyBody>`
/// from a captured status/headers/body (the buffered primary response).
#[allow(dead_code)]
pub fn rebuild_response(
    status: hyper::StatusCode,
    headers: &[(String, String)],
    body: Bytes,
) -> Response<ProxyBody> {
    let mut builder = Response::builder().status(status);
    for (k, v) in headers {
        builder = builder.header(k.as_str(), v.as_str());
    }
    builder
        .body(Full::new(body).map_err(|never| match never {}).boxed())
        .expect("valid response")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{Backend, BackendPool};
    use crate::config::{BackendConfig, CircuitConfig, ShadowConfig};
    use hyper::Response;
    use hyper::body::{Frame, Incoming};
    use hyper::server::conn::http1::Builder;
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use std::collections::{HashMap, VecDeque};
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use std::time::Instant;
    use tokio::net::TcpListener;

    fn decision(primary: Arc<Backend>, shadow: Arc<Backend>) -> RoutingDecision {
        RoutingDecision {
            backend: primary,
            shadow: Some(shadow),
            request_id: "rid".into(),
            started_at: Instant::now(),
        }
    }

    fn backend(name: &str, addr: &str) -> Arc<Backend> {
        Backend::new(
            name.into(),
            BackendConfig {
                address: addr.into(),
                weight: 1,
                health_path: "/".into(),
            },
        )
    }

    /// Build a pool with the given breaker config so shadow outcomes are
    /// recorded against per-backend breakers.
    fn pool_with_cfg(addr_map: &[(&str, &str)], cfg: CircuitConfig) -> Arc<BackendPool> {
        let mut backends = HashMap::new();
        for (name, addr) in addr_map {
            backends.insert(
                (*name).to_string(),
                BackendConfig {
                    address: (*addr).to_string(),
                    weight: 1,
                    health_path: "/".into(),
                },
            );
        }
        Arc::new(BackendPool::with_breaker_cfg(&backends, cfg))
    }

    async fn spawn_responder(
        status: u16,
        body: &'static str,
    ) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let io = TokioIo::new(stream);
                let body = body;
                let st = status;
                tokio::spawn(async move {
                    let svc = service_fn(move |_req: hyper::Request<Incoming>| {
                        let body = body;
                        async move {
                            Ok::<_, std::convert::Infallible>(
                                Response::builder()
                                    .status(st)
                                    .body(
                                        Full::new(Bytes::from(body))
                                            .map_err(|never| match never {})
                                            .boxed(),
                                    )
                                    .unwrap(),
                            )
                        }
                    });
                    let _ = Builder::new().serve_connection(io, svc).await;
                });
            }
        });
        (addr, handle)
    }

    #[tokio::test]
    async fn shadow_mode_always_serves_primary_backend() {
        // Primary returns "primary", shadow returns "shadow". The user always
        // sees the primary body — dispatch_shadow must never alter it.
        let (paddr, _p) = spawn_responder(200, "primary").await;
        let (saddr, _s) = spawn_responder(200, "shadow").await;
        let primary = backend("c", &paddr.to_string());
        let shadow = backend("rust", &saddr.to_string());
        let dec = decision(primary.clone(), shadow.clone());

        // Simulate the handler path: forward to primary, capture, then dispatch.
        let client = forwarder::build_client();
        let primary_req = rebuild_for_backend(
            &"/".parse().unwrap(),
            &Method::GET,
            &[],
            Bytes::new(),
            &paddr.to_string(),
        );
        let primary_resp = forwarder::forward(&dec, primary_req, &client)
            .await
            .unwrap();
        let (parts, mut pb) = primary_resp.into_parts();
        let primary_status = parts.status.as_u16();
        let (primary_body, _tail, _trunc) = read_capped(&mut pb, 1024).await.unwrap();

        // User-facing body is the primary's.
        assert_eq!(&primary_body[..], b"primary");

        // Dispatch a shadow request — it must not affect the primary body.
        dispatch_shadow(
            dec,
            Method::GET,
            "/".parse().unwrap(),
            vec![],
            Bytes::new(),
            primary_status,
            vec![],
            primary_body.clone(),
            false,
            client,
            ShadowConfig::default(),
            pool_with_cfg(
                &[("c", &paddr.to_string()), ("rust", &saddr.to_string())],
                CircuitConfig::default(),
            ),
        );
        // Give the spawned task a moment, then re-assert the user body.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(&primary_body[..], b"primary");
    }

    #[tokio::test]
    async fn divergence_does_not_affect_user() {
        // Shadow returns a different body than primary → divergence logged,
        // but the user-facing body stays the primary's.
        let (paddr, _p) = spawn_responder(200, "primary").await;
        let (saddr, _s) = spawn_responder(200, "DIFFERENT").await;
        let primary = backend("c", &paddr.to_string());
        let shadow = backend("rust", &saddr.to_string());
        let dec = decision(primary, shadow);
        let client = forwarder::build_client();
        let primary_req = rebuild_for_backend(
            &"/".parse().unwrap(),
            &Method::GET,
            &[],
            Bytes::new(),
            &paddr.to_string(),
        );
        let primary_resp = forwarder::forward(&dec, primary_req, &client)
            .await
            .unwrap();
        let (parts, mut pb) = primary_resp.into_parts();
        let (primary_body, _tail, _trunc) = read_capped(&mut pb, 1024).await.unwrap();

        dispatch_shadow(
            dec,
            Method::GET,
            "/".parse().unwrap(),
            vec![],
            Bytes::new(),
            parts.status.as_u16(),
            vec![],
            primary_body.clone(),
            false,
            client,
            ShadowConfig::default(),
            pool_with_cfg(
                &[("c", &paddr.to_string()), ("rust", &saddr.to_string())],
                CircuitConfig::default(),
            ),
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        // User body untouched.
        assert_eq!(&primary_body[..], b"primary");
    }

    #[tokio::test]
    async fn shadow_5xx_records_failure_and_opens_breaker() {
        // P2 (Record shadow failures in the shadow circuit breaker): a 5xx from
        // the shadow backend must be recorded as a failure so its breaker can
        // open and the router stops mirroring to it. 4xx is NOT a failure.
        let (paddr, _p) = spawn_responder(200, "primary").await;
        let (saddr, _s) = spawn_responder(500, "boom").await;
        let primary = backend("c", &paddr.to_string());
        let shadow = backend("rust", &saddr.to_string());
        let dec = decision(primary.clone(), shadow.clone());
        // Sensitive breaker: a single failure trips it.
        let cfg = CircuitConfig {
            error_rate_threshold: 0.5,
            window_secs: 30,
            min_requests: 1,
        };
        let pool = pool_with_cfg(
            &[("c", &paddr.to_string()), ("rust", &saddr.to_string())],
            cfg,
        );
        assert!(pool.breaker_allows("rust"), "breaker starts closed");

        let client = forwarder::build_client();
        dispatch_shadow(
            dec,
            Method::GET,
            "/".parse().unwrap(),
            vec![],
            Bytes::new(),
            200,
            vec![],
            Bytes::from_static(b"primary"),
            false,
            client,
            ShadowConfig::default(),
            pool.clone(),
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(
            !pool.breaker_allows("rust"),
            "shadow 5xx must open the breaker"
        );
    }

    #[tokio::test]
    async fn shadow_4xx_does_not_trip_breaker() {
        // A 4xx shadow response is a successful breaker outcome (mirrors
        // active/canary semantics): only 5xx and connection failures count.
        let (paddr, _p) = spawn_responder(200, "primary").await;
        let (saddr, _s) = spawn_responder(404, "nope").await;
        let primary = backend("c", &paddr.to_string());
        let shadow = backend("rust", &saddr.to_string());
        let dec = decision(primary.clone(), shadow.clone());
        let cfg = CircuitConfig {
            error_rate_threshold: 0.5,
            window_secs: 30,
            min_requests: 1,
        };
        let pool = pool_with_cfg(
            &[("c", &paddr.to_string()), ("rust", &saddr.to_string())],
            cfg,
        );

        let client = forwarder::build_client();
        dispatch_shadow(
            dec,
            Method::GET,
            "/".parse().unwrap(),
            vec![],
            Bytes::new(),
            200,
            vec![],
            Bytes::from_static(b"primary"),
            false,
            client,
            ShadowConfig::default(),
            pool.clone(),
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(
            pool.breaker_allows("rust"),
            "shadow 4xx must NOT open the breaker"
        );
    }

    #[tokio::test]
    async fn shadow_connection_failure_records_failure() {
        // Connection / forwarding failure to the shadow backend must record a
        // failed outcome (the Err branch), not just log a divergence.
        let (paddr, _p) = spawn_responder(200, "primary").await;
        let primary = backend("c", &paddr.to_string());
        let shadow = backend("rust", "127.0.0.1:1"); // nothing listening
        let dec = decision(primary.clone(), shadow.clone());
        let cfg = CircuitConfig {
            error_rate_threshold: 0.5,
            window_secs: 30,
            min_requests: 1,
        };
        let pool = pool_with_cfg(&[("c", &paddr.to_string()), ("rust", "127.0.0.1:1")], cfg);
        let client = forwarder::build_client();
        dispatch_shadow(
            dec,
            Method::GET,
            "/".parse().unwrap(),
            vec![],
            Bytes::new(),
            200,
            vec![],
            Bytes::from_static(b"primary"),
            false,
            client,
            ShadowConfig::default(),
            pool.clone(),
        );
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(
            !pool.breaker_allows("rust"),
            "shadow connection failure must open the breaker"
        );
    }

    #[tokio::test]
    async fn shadow_not_routable_does_not_charge_shadow_breaker() {
        // forward() checks the PRIMARY backend's routability (decision.backend).
        // If the primary is (or becomes) unroutable, forward returns
        // NotRoutable — which must NOT be charged as a failure to the shadow
        // backend's breaker, since the shadow never actually failed.
        let (paddr, _p) = spawn_responder(200, "primary").await;
        let (saddr, _s) = spawn_responder(200, "shadow").await;
        let primary = backend("c", &paddr.to_string());
        let shadow = backend("rust", &saddr.to_string());
        let dec = decision(primary.clone(), shadow.clone());
        // Flip the PRIMARY to unroutable after the decision was made.
        use crate::backend::Health;
        primary.health.store(
            Health::Unhealthy as u8,
            std::sync::atomic::Ordering::Relaxed,
        );
        let cfg = CircuitConfig {
            error_rate_threshold: 0.5,
            window_secs: 30,
            min_requests: 1,
        };
        let pool = pool_with_cfg(
            &[("c", &paddr.to_string()), ("rust", &saddr.to_string())],
            cfg,
        );
        let client = forwarder::build_client();
        dispatch_shadow(
            dec,
            Method::GET,
            "/".parse().unwrap(),
            vec![],
            Bytes::new(),
            200,
            vec![],
            Bytes::from_static(b"primary"),
            false,
            client,
            ShadowConfig::default(),
            pool.clone(),
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(
            pool.breaker_allows("rust"),
            "primary NotRoutable must not charge the shadow breaker"
        );
    }

    #[tokio::test]
    async fn large_body_over_cap_records_truncation() {
        // Feed read_capped a body larger than the cap; it must truncate.
        let big = Bytes::from(vec![b'x'; 1000]);
        let mut full = Full::new(big).map_err(|never| match never {}).boxed();
        let (got, tail, truncated) = read_capped(&mut full, 100).await.unwrap();
        assert!(truncated);
        assert_eq!(got.len(), 100);
        // The split frame's leftover is handed back so a caller can reconstruct
        // the full body losslessly.
        assert_eq!(tail.unwrap().len(), 900);
    }

    struct ErrorBody {
        frames: VecDeque<Result<Frame<Bytes>, &'static str>>,
    }

    impl hyper::body::Body for ErrorBody {
        type Data = Bytes;
        type Error = &'static str;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
            Poll::Ready(self.frames.pop_front())
        }
    }

    #[tokio::test]
    async fn read_capped_propagates_body_errors() {
        let mut body = ErrorBody {
            frames: VecDeque::from([
                Ok(Frame::data(Bytes::from_static(b"partial"))),
                Err("stream reset"),
            ]),
        };

        let err = read_capped(&mut body, 1024).await.unwrap_err();
        assert_eq!(err, "stream reset");
    }

    #[tokio::test]
    async fn large_primary_response_reconstructed_in_full() {
        // Regression (Claim 1): when the primary response exceeds the shadow
        // cap, the user must STILL receive the full body. handle_shadow uses
        // read_capped (prefix + split tail) + ChainedBody (streaming remainder)
        // to reconstruct it; this test exercises that exact pipeline against a
        // real backend and asserts the reassembled body is byte-identical to
        // the original — no truncation, no stale Content-Length.
        use crate::forwarder::ChainedBody;
        use std::convert::Infallible;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let expected = Bytes::from(vec![b'Z'; 5000]);
        let responder_body = expected.clone();
        tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let io = TokioIo::new(stream);
                let b = responder_body.clone();
                tokio::spawn(async move {
                    let svc = service_fn(move |_req: Request<Incoming>| {
                        let b = b.clone();
                        async move {
                            Ok::<_, Infallible>(
                                Response::builder()
                                    .status(200)
                                    .body(Full::new(b).map_err(|never| match never {}).boxed())
                                    .unwrap(),
                            )
                        }
                    });
                    let _ = Builder::new().serve_connection(io, svc).await;
                });
            }
        });

        let primary = backend("c", &addr.to_string());
        let shadow = backend("rust", "127.0.0.1:1"); // unused; we only test primary reconstruction
        let dec = decision(primary, shadow);
        let client = forwarder::build_client();
        let req = rebuild_for_backend(
            &"/".parse().unwrap(),
            &Method::GET,
            &[],
            Bytes::new(),
            &addr.to_string(),
        );
        let resp = forwarder::forward(&dec, req, &client).await.unwrap();
        let (_parts, mut body) = resp.into_parts();

        let cap = 1024;
        let (prefix, tail, truncated) = read_capped(&mut body, cap).await.unwrap();
        assert!(truncated, "5000B body must exceed the 1024 cap");

        // Reconstruct exactly as handle_shadow does for the user response.
        let user_body = ChainedBody::new([prefix, tail.unwrap_or_default()], Some(body));
        let collected = user_body.collect().await.unwrap().to_bytes();
        assert_eq!(collected.len(), expected.len(), "no byte may be lost");
        assert_eq!(
            collected, expected,
            "reassembled body must be byte-identical"
        );
    }

    #[test]
    fn shadow_headers_reconcile_content_length_with_capped_body() {
        // P2 regression (shadow body truncated but Content-Length forwarded):
        // forwarded headers carry the ORIGINAL Content-Length (5000), but the
        // mirrored shadow body is capped to 100 bytes. The shadow headers must
        // contain exactly one Content-Length == 100 and drop Transfer-Encoding
        // so the shadow backend receives a well-formed request.
        let orig = vec![
            ("host".into(), "example.com".into()),
            ("content-length".into(), "5000".into()),
            ("content-type".into(), "text/plain".into()),
            ("transfer-encoding".into(), "chunked".into()),
        ];
        let sh = shadow_headers_for_body(orig, 100);

        // Exactly one Content-Length, matching the capped body — never the
        // original 5000.
        let cls: Vec<&str> = sh
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(
            cls,
            vec!["100"],
            "Content-Length must match the capped body"
        );
        // Transfer-Encoding stripped defensively.
        assert!(
            sh.iter()
                .all(|(k, _)| !k.eq_ignore_ascii_case("transfer-encoding")),
            "Transfer-Encoding must be stripped"
        );
        // Other headers preserved.
        assert_eq!(
            sh.iter()
                .find(|(k, _)| k == "host")
                .map(|(_, v)| v.as_str()),
            Some("example.com")
        );
        assert_eq!(
            sh.iter()
                .find(|(k, _)| k == "content-type")
                .map(|(_, v)| v.as_str()),
            Some("text/plain")
        );
    }

    #[tokio::test]
    async fn shadow_capped_body_forwarded_with_reconciled_content_length() {
        // End-to-end (P2): a shadow request built from reconciled headers + a
        // capped body must be well-framed. The shadow backend reads exactly the
        // capped body (Content-Length matches) and responds — it must NOT hang
        // waiting for the original (larger) Content-Length's worth of bytes.
        // This exercises the exact framing dispatch_shadow now applies.
        use std::convert::Infallible;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let io = TokioIo::new(stream);
                tokio::spawn(async move {
                    let svc = service_fn(|req: Request<Incoming>| async move {
                        // Reads exactly Content-Length bytes; with the old
                        // framing (5000 vs 100 sent) this would hang.
                        let len = req
                            .into_body()
                            .collect()
                            .await
                            .map(|c| c.to_bytes().len())
                            .unwrap_or(0);
                        Ok::<_, Infallible>(
                            Response::builder()
                                .status(200)
                                .body(
                                    Full::new(Bytes::from(len.to_string()))
                                        .map_err(|never| match never {})
                                        .boxed(),
                                )
                                .unwrap(),
                        )
                    });
                    let _ = Builder::new().serve_connection(io, svc).await;
                });
            }
        });

        let shadow = backend("rust", &addr.to_string());
        let primary = backend("c", "127.0.0.1:1");
        let dec = decision(primary, shadow);

        // Original headers lie: Content-Length 5000 for a body of only 100.
        let orig_headers = vec![
            ("host".into(), "example.com".into()),
            ("content-length".into(), "5000".into()),
        ];
        let capped_body = Bytes::from(vec![b'x'; 100]);
        let shadow_headers = shadow_headers_for_body(orig_headers, capped_body.len());
        let req = rebuild_for_backend(
            &"/".parse().unwrap(),
            &Method::POST,
            &shadow_headers,
            capped_body,
            &addr.to_string(),
        );

        let client = forwarder::build_client();
        // Wrapped in a timeout: with the OLD buggy framing the backend would
        // hang waiting for 5000 bytes; reconciled framing must succeed fast.
        let resp = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            forwarder::forward(&dec, req, &client),
        )
        .await
        .expect("shadow request must not hang")
        .unwrap();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let received: usize = std::str::from_utf8(&body).unwrap().trim().parse().unwrap();
        assert_eq!(
            received, 100,
            "shadow backend must receive exactly the capped body"
        );
    }

    use std::sync::Arc;
}
