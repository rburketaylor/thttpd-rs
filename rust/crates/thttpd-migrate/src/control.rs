//! Control plane: JSON RPC over a Unix domain socket.
//!
//! The running proxy binds `config.control_socket` and accepts length-prefixed
//! JSON [`ControlRequest`]s. The CLI subcommands (`set-weight`, `rollback`,
//! `drain`) are thin clients that connect to that socket, send a request, and
//! read the [`ControlResponse`].
//!
//! Wire format: 4-byte big-endian length prefix, then UTF-8 JSON bytes. This
//! is a versioned, local-only protocol (see `docs/CONTROL_PROTOCOL.md`).

use crate::state::{LiveState, StateSnapshot, snapshot, write_state_atomic};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum ControlRequest {
    SetWeight { weights: HashMap<String, u32> },
    Rollback { to: String },
    Drain { timeout_secs: u64 },
    Snapshot,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ControlResponse {
    pub ok: bool,
    pub message: String,
    pub snapshot: Option<StateSnapshot>,
    pub version: u32,
}

impl ControlResponse {
    pub fn ok(message: impl Into<String>, snap: Option<StateSnapshot>) -> Self {
        Self {
            ok: true,
            message: message.into(),
            snapshot: snap,
            version: PROTOCOL_VERSION,
        }
    }
    pub fn err(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: message.into(),
            snapshot: None,
            version: PROTOCOL_VERSION,
        }
    }
}

/// Spawn the control-socket server. Runs until the proxy exits.
pub fn spawn_server(
    state: std::sync::Arc<LiveState>,
    pool: std::sync::Arc<crate::backend::BackendPool>,
    socket_path: std::sync::Arc<std::path::PathBuf>,
    state_path: std::sync::Arc<std::path::PathBuf>,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    // Ensure the path is bindable: create the parent dir, refuse to clobber a
    // live socket owned by another instance, and only clean up stale socket
    // files left behind by a crashed process.
    prepare_control_socket_path(socket_path.as_ref())?;
    let listener = UnixListener::bind(socket_path.as_ref())?;
    tracing::info!(path = %socket_path.display(), "control socket listening");
    let handle = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let state = state.clone();
                    let pool = pool.clone();
                    let sp = state_path.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_conn(stream, state, pool, sp).await {
                            tracing::warn!(error = %e, "control connection error");
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!(error = %e, "control accept error");
                    break;
                }
            }
        }
    });
    Ok(handle)
}

async fn handle_conn(
    mut stream: UnixStream,
    state: std::sync::Arc<LiveState>,
    pool: std::sync::Arc<crate::backend::BackendPool>,
    state_path: std::sync::Arc<std::path::PathBuf>,
) -> anyhow::Result<()> {
    let req = read_request(&mut stream).await?;
    let resp = match req {
        ControlRequest::SetWeight { weights } => match state.set_weights(&pool, &weights) {
            Ok(()) => {
                let snap = snapshot(&state, &pool);
                let _ = write_state_atomic(&state_path, &snap);
                ControlResponse::ok(format!("weights updated: {:?}", weights), Some(snap))
            }
            Err(e) => ControlResponse::err(format!("set-weight failed: {e}")),
        },
        ControlRequest::Rollback { to } => match state.rollback(&pool, &to) {
            Ok(()) => {
                let snap = snapshot(&state, &pool);
                let _ = write_state_atomic(&state_path, &snap);
                ControlResponse::ok(format!("rolled back to {to}"), Some(snap))
            }
            Err(e) => ControlResponse::err(format!("rollback failed: {e}")),
        },
        ControlRequest::Drain { timeout_secs } => {
            state.set_drain_grace(timeout_secs);
            state.start_drain();
            let snap = snapshot(&state, &pool);
            let _ = write_state_atomic(&state_path, &snap);
            tracing::info!(timeout_secs, "drain requested via control socket");
            ControlResponse::ok(
                format!("drain started, timeout {timeout_secs}s"),
                Some(snap),
            )
        }
        ControlRequest::Snapshot => ControlResponse::ok("ok", Some(snapshot(&state, &pool))),
    };
    write_response(&mut stream, &resp).await?;
    Ok(())
}

/// Make `path` safe to bind a control [`UnixListener`] on.
///
/// - Creates the parent directory if needed.
/// - If nothing exists at `path`, returns `Ok(())` (normal bind).
/// - If a live socket is there (a peer can `connect`), returns an error so a
///   second proxy instance cannot unlink the first instance's active
///   rollback/drain socket.
/// - If a stale socket file is there (connect refused — left by a crashed
///   process), removes it and returns `Ok(())`.
/// - If `path` exists but is not a socket, returns an error rather than
///   deleting an arbitrary file.
fn prepare_control_socket_path(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::FileTypeExt as _;

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    match std::fs::metadata(path) {
        // Nothing here yet — normal bind.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(anyhow::anyhow!(
            "control socket path {} unreadable: {e}",
            path.display()
        )),
        Ok(meta) => {
            // Only an existing unix socket is something we're willing to clean
            // up; never unlink a regular file / directory / device.
            if !meta.file_type().is_socket() {
                return Err(anyhow::anyhow!(
                    "control socket path {} exists and is not a socket; refusing to overwrite",
                    path.display()
                ));
            }
            // Probe liveness: a successful connect means another instance owns
            // the socket. The probe connection is immediately dropped; the
            // owning server will see a short-lived connection it fails to read
            // a framed request from (logged as a control connection error).
            match std::os::unix::net::UnixStream::connect(path) {
                Ok(_) => Err(anyhow::anyhow!(
                    "control socket {} already in use by another instance",
                    path.display()
                )),
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                    // Stale socket file — safe to remove.
                    std::fs::remove_file(path)?;
                    Ok(())
                }
                Err(e) => Err(anyhow::anyhow!(
                    "control socket {} could not be probed for liveness: {e}",
                    path.display()
                )),
            }
        }
    }
}
pub fn spawn_state_writer(
    state: std::sync::Arc<LiveState>,
    pool: std::sync::Arc<crate::backend::BackendPool>,
    state_path: std::sync::Arc<std::path::PathBuf>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            tick.tick().await;
            let snap = snapshot(&state, &pool);
            if let Err(e) = write_state_atomic(&state_path, &snap) {
                tracing::warn!(error = %e, "state file write failed");
            }
        }
    })
}

// ===== client side (used by the CLI subcommands) =====

async fn send_request(socket: &Path, req: &ControlRequest) -> anyhow::Result<ControlResponse> {
    let mut stream = UnixStream::connect(socket).await?;
    write_request(&mut stream, req).await?;
    let resp = read_response(&mut stream).await?;
    Ok(resp)
}

pub async fn client_set_weight(
    socket: &Path,
    weights: HashMap<String, u32>,
) -> anyhow::Result<ControlResponse> {
    send_request(socket, &ControlRequest::SetWeight { weights }).await
}

pub async fn client_rollback(socket: &Path, to: &str) -> anyhow::Result<ControlResponse> {
    send_request(socket, &ControlRequest::Rollback { to: to.to_string() }).await
}

pub async fn client_drain(socket: &Path, timeout_secs: u64) -> anyhow::Result<ControlResponse> {
    send_request(socket, &ControlRequest::Drain { timeout_secs }).await
}

pub async fn client_snapshot(socket: &Path) -> anyhow::Result<ControlResponse> {
    send_request(socket, &ControlRequest::Snapshot).await
}

// ===== length-prefixed framing =====

async fn write_request<W: AsyncWriteExt + Unpin>(
    w: &mut W,
    req: &ControlRequest,
) -> anyhow::Result<()> {
    let json = serde_json::to_vec(req)?;
    write_frame(w, &json).await
}

async fn write_response<W: AsyncWriteExt + Unpin>(
    w: &mut W,
    resp: &ControlResponse,
) -> anyhow::Result<()> {
    let json = serde_json::to_vec(resp)?;
    write_frame(w, &json).await
}

async fn write_frame<W: AsyncWriteExt + Unpin>(w: &mut W, bytes: &[u8]) -> anyhow::Result<()> {
    let len = (bytes.len() as u32).to_be_bytes();
    w.write_all(&len).await?;
    w.write_all(bytes).await?;
    w.flush().await?;
    Ok(())
}

async fn read_request<R: AsyncReadExt + Unpin>(r: &mut R) -> anyhow::Result<ControlRequest> {
    let bytes = read_frame(r).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

async fn read_response<R: AsyncReadExt + Unpin>(r: &mut R) -> anyhow::Result<ControlResponse> {
    let bytes = read_frame(r).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

async fn read_frame<R: AsyncReadExt + Unpin>(r: &mut R) -> anyhow::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    anyhow::ensure!(len <= 16 * 1024 * 1024, "control frame too large: {len}");
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BackendConfig, ProxyConfig};
    use crate::state::LiveState;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn two_backends() -> (ProxyConfig, crate::backend::BackendPool) {
        let mut backends = HashMap::new();
        backends.insert(
            "c".into(),
            BackendConfig {
                address: "127.0.0.1:8081".into(),
                weight: 100,
                health_path: "/".into(),
            },
        );
        backends.insert(
            "rust".into(),
            BackendConfig {
                address: "127.0.0.1:8082".into(),
                weight: 0,
                health_path: "/".into(),
            },
        );
        let cfg = ProxyConfig {
            listen: "127.0.0.1:8080".into(),
            log_level: "info".into(),
            state_path: "/tmp/s.json".into(),
            control_socket: "/tmp/c.sock".into(),
            metrics: Default::default(),
            shadow: Default::default(),
            backends,
            routing: Default::default(),
            health: Default::default(),
            circuit_breaker: Default::default(),
        };
        let pool = crate::backend::BackendPool::from_config(&cfg.backends);
        (cfg, pool)
    }

    async fn spawn_test_server(
        dir: &Path,
    ) -> (Arc<LiveState>, Arc<crate::backend::BackendPool>, PathBuf) {
        let (cfg, pool) = two_backends();
        let state = Arc::new(LiveState::new(cfg));
        let pool = Arc::new(pool);
        let sock = dir.join("control.sock");
        let state_path = dir.join("state.json");
        let _handle = spawn_server(
            state.clone(),
            pool.clone(),
            Arc::new(sock.clone()),
            Arc::new(state_path),
        )
        .unwrap();
        // Brief yield so the listener binds.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        (state, pool, sock)
    }

    use std::path::PathBuf;

    #[tokio::test]
    async fn set_weight_updates_live_state() {
        let dir = tempfile::tempdir().unwrap();
        let (state, pool, sock) = spawn_test_server(dir.path()).await;
        let mut w = HashMap::new();
        w.insert("rust".into(), 100u32);
        w.insert("c".into(), 0u32);
        let resp = client_set_weight(&sock, w).await.unwrap();
        assert!(resp.ok, "{}", resp.message);
        assert_eq!(pool.get("rust").unwrap().weight(), 100);
        assert_eq!(pool.get("c").unwrap().weight(), 0);
        drop(state);
    }

    #[tokio::test]
    async fn rollback_over_control_socket() {
        let dir = tempfile::tempdir().unwrap();
        let (state, pool, sock) = spawn_test_server(dir.path()).await;
        let resp = client_rollback(&sock, "rust").await.unwrap();
        assert!(resp.ok, "{}", resp.message);
        assert_eq!(pool.get("rust").unwrap().weight(), 100);
        assert_eq!(pool.get("c").unwrap().weight(), 0);
        // Roll back to c.
        let resp = client_rollback(&sock, "c").await.unwrap();
        assert!(resp.ok);
        assert_eq!(pool.get("c").unwrap().weight(), 100);
        drop(state);
    }

    #[tokio::test]
    async fn rollback_is_semantic_not_u32_max_weight() {
        let dir = tempfile::tempdir().unwrap();
        let (state, pool, sock) = spawn_test_server(dir.path()).await;
        client_rollback(&sock, "rust").await.unwrap();
        // No weight is u32::MAX; target is exactly 100, others exactly 0.
        for b in pool.iter() {
            assert!(
                b.weight() == 0 || b.weight() == 100,
                "weight {} not semantic",
                b.weight()
            );
        }
        drop(state);
    }

    #[tokio::test]
    async fn drain_sets_flag_via_socket() {
        let dir = tempfile::tempdir().unwrap();
        let (state, _pool, sock) = spawn_test_server(dir.path()).await;
        let resp = client_drain(&sock, 30).await.unwrap();
        assert!(resp.ok);
        assert!(state.is_draining());
    }

    #[tokio::test]
    async fn snapshot_returns_backends() {
        let dir = tempfile::tempdir().unwrap();
        let (state, _pool, sock) = spawn_test_server(dir.path()).await;
        let resp = client_snapshot(&sock).await.unwrap();
        assert!(resp.ok);
        let snap = resp.snapshot.unwrap();
        assert_eq!(snap.backends.len(), 2);
        drop(state);
    }

    #[tokio::test]
    async fn unknown_backend_set_weight_errors() {
        let dir = tempfile::tempdir().unwrap();
        let (state, _pool, sock) = spawn_test_server(dir.path()).await;
        let mut w = HashMap::new();
        w.insert("nope".into(), 1u32);
        let resp = client_set_weight(&sock, w).await.unwrap();
        assert!(!resp.ok);
        assert!(resp.message.contains("unknown backend"));
        drop(state);
    }

    #[tokio::test]
    async fn second_instance_on_live_socket_errors_and_first_survives() {
        // P1 (Avoid unlinking a live control socket): a second instance must
        // NOT unlink the first instance's active control socket. It must error,
        // and the first instance must keep answering rollback/drain/snapshot.
        let dir = tempfile::tempdir().unwrap();
        let (state, _pool, sock) = spawn_test_server(dir.path()).await;

        let (cfg, pool) = two_backends();
        let state2 = Arc::new(LiveState::new(cfg));
        let pool2 = Arc::new(pool);
        let second = spawn_server(
            state2,
            pool2,
            Arc::new(sock.clone()),
            Arc::new(dir.path().join("state2.json")),
        );
        assert!(
            second.is_err(),
            "second instance must not steal a live control socket"
        );
        let msg = second.unwrap_err().to_string();
        assert!(
            msg.contains("already in use"),
            "expected 'already in use', got: {msg}"
        );

        // The first instance is untouched and still answers control commands.
        let resp = client_snapshot(&sock).await.unwrap();
        assert!(
            resp.ok,
            "first instance must still answer: {}",
            resp.message
        );
        drop(state);
    }

    #[tokio::test]
    async fn stale_socket_file_is_removed_and_rebound() {
        // A stale socket file left behind by a crashed process must be detected
        // (connect refused) and removed so the next start can bind cleanly.
        let dir = tempfile::tempdir().unwrap();
        let stale = dir.path().join("stale.sock");
        {
            // UnixListener leaves the socket path on drop.
            let listener = std::os::unix::net::UnixListener::bind(&stale).unwrap();
            drop(listener);
        }
        assert!(stale.exists(), "stale socket file should exist");

        prepare_control_socket_path(&stale).expect("stale socket should be removable");
        assert!(!stale.exists(), "stale socket file should be removed");
    }

    #[tokio::test]
    async fn refuses_to_overwrite_non_socket_file() {
        // Never delete an arbitrary file that happens to sit at the socket path.
        let dir = tempfile::tempdir().unwrap();
        let regular = dir.path().join("not-a-socket");
        std::fs::write(&regular, b"data").unwrap();

        let err = prepare_control_socket_path(&regular)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("not a socket"),
            "expected 'not a socket', got: {err}"
        );
        assert!(regular.exists(), "non-socket file must not be deleted");
    }
}
