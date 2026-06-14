//! Active health checker.
//!
//! A periodic probe task hits each backend's `health_path` on a configurable
//! interval. A successful probe is a 2xx status; connection errors and non-2xx
//! responses (including 5xx) all count as failures. Passive health (per-request
//! failure counting) is layered on by the circuit breaker in [`crate::circuit`].

use crate::backend::{Backend, BackendPool, Health};
use crate::config::HealthConfig;
use crate::forwarder::{ProxyClient, empty_body};
use hyper::Request;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::task::JoinHandle;

pub fn spawn_checker(
    pool: Arc<BackendPool>,
    client: ProxyClient,
    cfg: HealthConfig,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let interval = cfg.interval();
        loop {
            for backend in pool.iter().cloned().collect::<Vec<_>>() {
                probe_backend(&backend, &client, &cfg).await;
            }
            tokio::time::sleep(interval).await;
        }
    })
}

async fn probe_backend(backend: &Arc<Backend>, client: &ProxyClient, cfg: &HealthConfig) {
    let url = format!(
        "http://{}{}",
        backend.config.read().address,
        backend.config.read().health_path
    );
    let req = match Request::get(&url).body(empty_body()) {
        Ok(r) => r,
        Err(_) => {
            update_health(backend, cfg, false);
            return;
        }
    };
    // A successful probe is a 2xx status. Connection errors and non-2xx
    // responses (including 5xx) all count as failures.
    let success = match tokio::time::timeout(cfg.timeout(), client.request(req)).await {
        Ok(Ok(resp)) => resp.status().is_success(),
        Ok(Err(_)) | Err(_) => false,
    };
    update_health(backend, cfg, success);
}

/// Update a backend's health from a single probe outcome.
pub fn update_health(backend: &Backend, cfg: &HealthConfig, success: bool) {
    if success {
        backend.consecutive_failures.store(0, Ordering::Relaxed);
        let n = backend
            .consecutive_successes
            .fetch_add(1, Ordering::Relaxed)
            + 1;
        if n >= cfg.success_threshold && backend.health() != Health::Healthy {
            backend
                .health
                .store(Health::Healthy as u8, Ordering::Relaxed);
            tracing::info!(backend = %backend.name, "backend healthy");
        }
    } else {
        backend.consecutive_successes.store(0, Ordering::Relaxed);
        let n = backend.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
        if n >= cfg.failure_threshold && backend.health() != Health::Unhealthy {
            backend
                .health
                .store(Health::Unhealthy as u8, Ordering::Relaxed);
            tracing::warn!(backend = %backend.name, "backend unhealthy");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BackendConfig;

    fn backend() -> Arc<Backend> {
        Backend::new(
            "t".into(),
            BackendConfig {
                address: "127.0.0.1:1".into(),
                weight: 1,
                health_path: "/".into(),
            },
        )
    }

    fn cfg() -> HealthConfig {
        HealthConfig {
            interval_ms: 1000,
            timeout_ms: 500,
            failure_threshold: 3,
            success_threshold: 2,
        }
    }

    #[test]
    fn three_consecutive_failures_marks_unhealthy() {
        let b = backend();
        let cfg = cfg();
        for _ in 0..3 {
            update_health(&b, &cfg, false);
        }
        assert_eq!(b.health(), Health::Unhealthy);
    }

    #[test]
    fn two_consecutive_successes_marks_healthy() {
        let b = backend();
        let cfg = cfg();
        // Drive to unhealthy first.
        for _ in 0..3 {
            update_health(&b, &cfg, false);
        }
        assert_eq!(b.health(), Health::Unhealthy);
        // Recover.
        for _ in 0..2 {
            update_health(&b, &cfg, true);
        }
        assert_eq!(b.health(), Health::Healthy);
    }

    #[test]
    fn intermittent_failures_do_not_flip() {
        let b = backend();
        let cfg = cfg();
        // fail, fail, success, fail, fail — never reaches 3 consecutive.
        update_health(&b, &cfg, false);
        update_health(&b, &cfg, false);
        update_health(&b, &cfg, true);
        update_health(&b, &cfg, false);
        update_health(&b, &cfg, false);
        assert_eq!(b.health(), Health::Healthy);
    }

    #[tokio::test]
    async fn timeout_counts_as_failure() {
        use tokio::net::TcpListener;
        // A listener that accepts but never responds → probe times out.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accept_task = tokio::spawn(async move {
            // accept and hold the connection open without replying
            let _ = listener.accept().await;
            std::future::pending::<()>().await;
        });
        let b = Backend::new(
            "t".into(),
            BackendConfig {
                address: addr.to_string(),
                weight: 1,
                health_path: "/".into(),
            },
        );
        let fast_cfg = HealthConfig {
            interval_ms: 1000,
            timeout_ms: 50, // very short
            failure_threshold: 1,
            success_threshold: 1,
        };
        let client = crate::forwarder::build_client();
        probe_backend(&b, &client, &fast_cfg).await;
        assert_eq!(
            b.health(),
            Health::Unhealthy,
            "a timed-out probe must count as a failure"
        );
        accept_task.abort();
    }
}
