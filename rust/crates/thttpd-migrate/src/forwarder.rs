//! HTTP/1.1 request forwarding to a chosen backend.
//!
//! The forwarder builds an absolute backend URI (`http://{addr}{path_and_query}`),
//! preserves method and headers (stripping hop-by-hop headers), forwards a boxed
//! body, and streams the backend's response back without buffering in
//! active-active/canary mode.

use crate::router::RoutingDecision;
use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::body::{Frame, Incoming};
use hyper::header::HeaderName;
use hyper::{Request, Response, Uri};
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use std::pin::Pin;
use std::str::FromStr;
use std::task::{Context, Poll};
use std::time::Duration;

pub type ProxyBody = BoxBody<Bytes, hyper::Error>;
pub type ProxyClient = Client<HttpConnector, ProxyBody>;

/// Hop-by-hop headers defined by RFC 7230 §6.1 plus the HTTP/1.1 `Connection`
/// header itself. These must not be forwarded across a proxy hop.
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

pub fn empty_body() -> ProxyBody {
    Full::new(Bytes::new())
        .map_err(|never| match never {})
        .boxed()
}

pub fn build_client() -> ProxyClient {
    let mut connector = HttpConnector::new();
    connector.set_connect_timeout(Some(Duration::from_secs(2)));
    Client::builder(hyper_util::rt::TokioExecutor::new())
        .pool_idle_timeout(Duration::from_secs(30))
        .build(connector)
}

fn is_hop_by_hop(name: &str) -> bool {
    HOP_BY_HOP.iter().any(|h| h.eq_ignore_ascii_case(name))
}

/// Rebuild a request targeted at `backend_addr`, reusing the given body.
///
/// Method, path-and-query, and non-hop-by-hop headers are preserved. The
/// inbound `Host` header is forwarded verbatim so virtual-host-aware backends
/// (e.g. thttpd's vhost mode) keep routing to the correct document root. A
/// synthetic `Host: {backend_addr}` is only added when the inbound request
/// carried no `Host` at all (e.g. an HTTP/1.0 client).
pub fn rebuild_for_backend(
    original_uri: &Uri,
    method: &hyper::Method,
    headers: &[(String, String)],
    body: Bytes,
    backend_addr: &str,
) -> Request<ProxyBody> {
    let body = Full::new(body).map_err(|never| match never {}).boxed();
    rebuild_for_backend_streaming(original_uri, method, headers, body, backend_addr)
}

/// Rebuild a request targeted at `backend_addr` with a **streaming** body.
///
/// This is the streaming variant used by the active/canary path (the inbound
/// `Incoming` body is forwarded straight through, unbuffered) and by shadow
/// mode when a body exceeds the shadow cap (buffered prefix + streaming
/// remainder, see [`ChainedBody`]).
pub fn rebuild_for_backend_streaming(
    original_uri: &Uri,
    method: &hyper::Method,
    headers: &[(String, String)],
    body: ProxyBody,
    backend_addr: &str,
) -> Request<ProxyBody> {
    let path_and_query = original_uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    // Build the absolute backend URI from the address + inbound path-and-query.
    // `backend_addr` is validated as a host:port authority at config load, so
    // this parse cannot fail at runtime.
    let uri: Uri = format!("http://{backend_addr}{path_and_query}")
        .parse()
        .expect("valid backend URI");
    let mut builder = Request::builder().method(method.clone()).uri(uri);
    // Forward every non-hop-by-hop header, INCLUDING the inbound `Host`. The
    // URI's authority still drives the TCP connection (where to connect), while
    // the forwarded `Host` controls which virtual host the origin serves — so a
    // request like `Host: vhost1.example.com` reaches the right document root
    // through the proxy.
    let mut had_host = false;
    for (k, v) in headers {
        if is_hop_by_hop(k) {
            continue;
        }
        if k.eq_ignore_ascii_case("host") {
            had_host = true;
        }
        builder = builder.header(k.as_str(), v.as_str());
    }
    // Only synthesize a Host when the inbound request had none (e.g. HTTP/1.0
    // clients). Never overwrite a client-supplied virtual-host Host.
    if !had_host {
        builder = builder.header(hyper::header::HOST, backend_addr);
    }
    builder.body(body).expect("valid request")
}

/// A body that yields buffered `Bytes` chunks (a capped prefix plus, when a
/// frame was split across the cap, its leftover tail), then delegates to a
/// streaming body. Used to forward/return the FULL body while keeping only a
/// capped copy for shadow diffing: the buffered chunks are the capped copy,
/// the rest streams unbuffered, so memory stays bounded regardless of total
/// size.
pub struct ChainedBody {
    chunks: std::collections::VecDeque<Bytes>,
    rest: Option<Incoming>,
}

impl ChainedBody {
    /// Emits `chunks` (empty chunks skipped) in order, then drains `rest`.
    pub fn new(chunks: impl IntoIterator<Item = Bytes>, rest: Option<Incoming>) -> Self {
        Self {
            chunks: chunks.into_iter().filter(|c| !c.is_empty()).collect(),
            rest,
        }
    }
}

impl hyper::body::Body for ChainedBody {
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, hyper::Error>>> {
        let this = self.get_mut();
        if let Some(chunk) = this.chunks.pop_front() {
            return Poll::Ready(Some(Ok(Frame::data(chunk))));
        }
        match this.rest.as_mut() {
            Some(rest) => Pin::new(rest).poll_frame(cx),
            None => Poll::Ready(None),
        }
    }
}

pub async fn forward(
    decision: &RoutingDecision,
    req: Request<ProxyBody>,
    client: &ProxyClient,
) -> Result<Response<Incoming>, ForwardError> {
    if !decision.backend.is_routable() {
        return Err(ForwardError::NotRoutable);
    }
    client.request(req).await.map_err(ForwardError::Request)
}

#[derive(Debug, thiserror::Error)]
pub enum ForwardError {
    #[error("backend request failed: {0}")]
    Request(#[source] hyper_util::client::legacy::Error),
    #[error("backend not routable")]
    NotRoutable,
}

/// Collect the set of forwarded header name/value pairs from an incoming
/// request, excluding hop-by-hop headers AND headers named by the request's
/// `Connection` field (RFC 7230 §6.1) — the latter are per-hop and must not be
/// forwarded end-to-end. The `Connection` header itself is hop-by-hop.
pub fn forwarded_headers(headers: &hyper::HeaderMap) -> Vec<(String, String)> {
    let connection_tokens: std::collections::HashSet<String> = headers
        .get_all("connection")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|s| s.split(',').map(|t| t.trim().to_ascii_lowercase()))
        .filter(|t| !t.is_empty())
        .collect();
    headers
        .iter()
        .filter(|(name, _)| {
            let n = name.as_str();
            !is_hop_by_hop(n) && !connection_tokens.contains(&n.to_ascii_lowercase())
        })
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// Strip hop-by-hop headers from a response header map in place. In addition
/// to the fixed RFC 7230 §6.1 set, removes headers named by the response's own
/// `Connection` field (per-hop tokens), then removes `Connection` itself.
pub fn strip_hop_by_hop(headers: &mut hyper::HeaderMap) {
    // First drop headers named by the Connection field, before removing
    // Connection itself.
    let connection_listed: Vec<HeaderName> = headers
        .get_all("connection")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|s| s.split(',').map(|t| t.trim().to_ascii_lowercase()))
        .filter(|t| !t.is_empty())
        .filter_map(|t| HeaderName::from_str(&t).ok())
        .filter(|n| headers.contains_key(n))
        .collect();
    for name in connection_listed {
        headers.remove(&name);
    }
    let to_remove: Vec<HeaderName> = headers
        .iter()
        .map(|(n, _)| n.clone())
        .filter(|n| is_hop_by_hop(n.as_str()))
        .collect();
    for name in to_remove {
        headers.remove(name);
    }
}

/// Convert an [`Incoming`] response body into a streaming [`ProxyBody`] by
/// mapping its error type to `hyper::Error`. `Incoming`'s associated `Error`
/// type is `hyper::Error`, so the identity map is sound.
pub fn body_to_proxy(body: Incoming) -> ProxyBody {
    body.map_err(|e| e).boxed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Backend;
    use crate::config::BackendConfig;
    use crate::router::RoutingDecision;
    use http_body_util::{BodyExt, StreamBody};
    use hyper::body::{Frame, Incoming as BodyIncoming};
    use hyper::server::conn::http1::Builder;
    use hyper::service::service_fn;
    use hyper::{Method, Request, Response, StatusCode};
    use hyper_util::rt::TokioIo;
    use std::net::SocketAddr;
    use std::time::Instant;
    use tokio::net::TcpListener;
    use tokio_stream::wrappers::ReceiverStream;

    fn decision(addr: &str) -> RoutingDecision {
        let backend = Backend::new(
            "test".into(),
            BackendConfig {
                address: addr.into(),
                weight: 1,
                health_path: "/".into(),
            },
        );
        RoutingDecision {
            backend,
            shadow: None,
            request_id: "rid".into(),
            started_at: Instant::now(),
        }
    }

    async fn spawn_echo_server() -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let io = TokioIo::new(stream);
                tokio::spawn(async move {
                    let svc = service_fn(|req: Request<BodyIncoming>| async move {
                        let (parts, body) = req.into_parts();
                        let body_len = body
                            .collect()
                            .await
                            .map(|c| c.to_bytes().len())
                            .unwrap_or(0);
                        let payload = format!(
                            "{}\n{}\n{}\n{}",
                            parts.method,
                            parts.uri.path(),
                            parts
                                .headers
                                .get("x-test")
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or(""),
                            body_len
                        );
                        Ok::<_, std::convert::Infallible>(
                            Response::builder()
                                .status(StatusCode::OK)
                                .body(
                                    Full::new(Bytes::from(payload))
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
        (addr, handle)
    }

    #[tokio::test]
    async fn preserves_method_path_headers_body() {
        let (addr, _h) = spawn_echo_server().await;
        let client = build_client();
        let dec = decision(&addr.to_string());
        let req = rebuild_for_backend(
            &"/echo?q=1".parse().unwrap(),
            &Method::POST,
            &[("x-test".into(), "marker".into())],
            Bytes::from_static(b"hello-body"),
            &addr.to_string(),
        );
        let resp = forward(&dec, req, &client).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let text = std::str::from_utf8(&body).unwrap();
        // method, path, x-test header, and body length all preserved
        assert_eq!(text, "POST\n/echo\nmarker\n10");
    }

    async fn spawn_streaming_server(
        chunk_size: usize,
        chunks: usize,
    ) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let io = TokioIo::new(stream);
                let cs = chunk_size;
                let n = chunks;
                tokio::spawn(async move {
                    let svc = service_fn(move |_req: Request<BodyIncoming>| {
                        let cs = cs;
                        let n = n;
                        async move {
                            // Build a channel-backed stream of `n` frames, each `cs` bytes.
                            let (tx, rx) = tokio::sync::mpsc::channel::<
                                Result<Frame<Bytes>, std::io::Error>,
                            >(8);
                            tokio::spawn(async move {
                                for i in 0..n {
                                    let byte = b'a' + ((i % 26) as u8);
                                    let _ =
                                        tx.send(Ok(Frame::data(Bytes::from(vec![byte; cs])))).await;
                                }
                            });
                            let body = StreamBody::new(ReceiverStream::new(rx)).boxed();
                            Ok::<_, std::convert::Infallible>(
                                Response::builder()
                                    .status(StatusCode::OK)
                                    .body(body)
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
    async fn streams_large_response() {
        // 10 frames × 100KB = 1MB; verify the full body is forwarded and the
        // response is read as multiple frames (not collapsed into one buffer).
        let (addr, _h) = spawn_streaming_server(100_000, 10).await;
        let client = build_client();
        let dec = decision(&addr.to_string());
        let req = rebuild_for_backend(
            &"/".parse().unwrap(),
            &Method::GET,
            &[],
            Bytes::new(),
            &addr.to_string(),
        );
        let resp = forward(&dec, req, &client).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let mut body = resp.into_body();
        let mut total = 0usize;
        let mut frames = 0usize;
        while let Some(frame) = body.frame().await {
            let frame = frame.unwrap();
            if let Ok(data) = frame.into_data() {
                total += data.len();
                frames += 1;
            }
        }
        assert_eq!(total, 1_000_000, "full 1MB body must be forwarded");
        assert!(
            frames > 1,
            "response must stream as multiple frames, got {frames}"
        );
    }

    #[tokio::test]
    async fn forward_to_unroutable_errors() {
        // No server listening: connection refused surfaces as a forward error.
        let client = build_client();
        let dec = decision("127.0.0.1:1"); // port 1: reserved, no listener
        let req = rebuild_for_backend(
            &"/".parse().unwrap(),
            &Method::GET,
            &[],
            Bytes::new(),
            "127.0.0.1:1",
        );
        let result = forward(&dec, req, &client).await;
        assert!(result.is_err(), "must error when backend is unreachable");
    }

    #[tokio::test]
    async fn hop_by_hop_headers_stripped() {
        let (addr, _h) = spawn_echo_server().await;
        let client = build_client();
        let dec = decision(&addr.to_string());
        // connection is hop-by-hop; x-test is end-to-end.
        let req = rebuild_for_backend(
            &"/".parse().unwrap(),
            &Method::GET,
            &[
                ("connection".into(), "keep-alive".into()),
                ("x-test".into(), "kept".into()),
            ],
            Bytes::new(),
            &addr.to_string(),
        );
        let _ = forward(&dec, req, &client).await.unwrap();
        // The echo server returns x-test value; if connection had leaked it
        // wouldn't appear in x-test anyway, so this mainly asserts no panic and
        // a successful forward with hop-by-hop present in input.
    }

    #[test]
    fn connection_listed_headers_not_forwarded() {
        // Regression (Claim 8): RFC 7230 §6.1 — a header named inside the
        // Connection field (e.g. `Connection: X-Hop`) is itself hop-by-hop and
        // must be stripped. Previously only the fixed list was removed, so
        // X-Hop leaked through as an end-to-end header.
        let mut map = hyper::HeaderMap::new();
        map.insert("connection", "X-Hop, keep-alive".parse().unwrap());
        map.insert("x-hop", "leak-me".parse().unwrap());
        map.insert("x-end", "keep-me".parse().unwrap());

        let fwd = forwarded_headers(&map);
        let names: Vec<&str> = fwd.iter().map(|(k, _)| k.as_str()).collect();
        assert!(
            !names.iter().any(|n| n.eq_ignore_ascii_case("connection")),
            "connection stripped"
        );
        assert!(
            !names.iter().any(|n| n.eq_ignore_ascii_case("x-hop")),
            "connection-listed X-Hop stripped"
        );
        assert!(
            names.iter().any(|n| n.eq_ignore_ascii_case("x-end")),
            "end-to-end header kept"
        );

        // Same rule on the response path.
        let mut resp_map = hyper::HeaderMap::new();
        resp_map.insert("connection", "X-Hop".parse().unwrap());
        resp_map.insert("x-hop", "leak-me".parse().unwrap());
        resp_map.insert("x-end", "keep-me".parse().unwrap());
        strip_hop_by_hop(&mut resp_map);
        assert!(
            resp_map.get("connection").is_none(),
            "response connection stripped"
        );
        assert!(
            resp_map.get("x-hop").is_none(),
            "response connection-listed stripped"
        );
        assert!(resp_map.get("x-end").is_some(), "response end-to-end kept");
    }

    #[test]
    fn preserves_inbound_host_header() {
        // Virtual-host routing (Claim: Preserve the inbound Host header): a
        // client Host such as `vhost1.example.com` must reach the backend
        // unchanged so thttpd's vhost mode selects the correct document root.
        let req = rebuild_for_backend(
            &"/".parse().unwrap(),
            &Method::GET,
            &[("host".into(), "vhost1.example.com".into())],
            Bytes::new(),
            "127.0.0.1:8081",
        );
        assert_eq!(
            req.headers().get("host").unwrap(),
            "vhost1.example.com",
            "inbound Host must be preserved for virtual-host backends"
        );
    }

    #[test]
    fn synthesizes_host_when_absent() {
        // No inbound Host (e.g. an HTTP/1.0 client) → fall back to the backend
        // address so the origin still receives a well-formed request.
        let req = rebuild_for_backend(
            &"/".parse().unwrap(),
            &Method::GET,
            &[],
            Bytes::new(),
            "127.0.0.1:8081",
        );
        assert_eq!(
            req.headers().get("host").unwrap(),
            "127.0.0.1:8081",
            "absent Host must fall back to the backend address"
        );
    }

    #[tokio::test]
    async fn preserved_virtual_host_host_reaches_backend() {
        // End-to-end (P1, Preserve the inbound Host header): forwarding with an
        // inbound virtual-host Host must BOTH connect to the backend (URI
        // authority = backend addr) AND deliver that Host on the wire so the
        // origin's vhost routing selects the right document root. This verifies
        // hyper does not override a client-supplied Host to match the URI's
        // authority.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let _handle = tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let io = TokioIo::new(stream);
                tokio::spawn(async move {
                    let svc = service_fn(|req: Request<BodyIncoming>| async move {
                        let host = req
                            .headers()
                            .get("host")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or_default()
                            .to_string();
                        Ok::<_, std::convert::Infallible>(
                            Response::builder()
                                .status(StatusCode::OK)
                                .body(
                                    Full::new(Bytes::from(host))
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

        let client = build_client();
        let dec = decision(&addr.to_string());
        let req = rebuild_for_backend(
            &"/".parse().unwrap(),
            &Method::GET,
            &[("host".into(), "vhost1.example.com".into())],
            Bytes::new(),
            &addr.to_string(),
        );
        let resp = forward(&dec, req, &client).await.expect("forward ok");
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let received_host = std::str::from_utf8(&body).unwrap();
        assert_eq!(
            received_host, "vhost1.example.com",
            "the preserved virtual-host Host must reach the backend on the wire"
        );
    }
}
