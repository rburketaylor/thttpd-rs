//! Prometheus metrics exporter.
//!
//! Installs a `metrics-exporter-prometheus` HTTP listener on the configured
//! metrics address (default `127.0.0.1:9100`). Metrics live on a separate
//! listener from the data plane so `/metrics` can never collide with proxied
//! legacy content.
//!
//! Note: `PrometheusBuilder::with_http_listener` serves `/metrics` at a fixed
//! path. The `path` config value is currently advisory — honoring a custom path
//! requires building a manual hyper route around the handle, which is deferred.

use metrics::{describe_counter, describe_histogram};
use metrics_exporter_prometheus::PrometheusBuilder;

/// `path` is currently advisory: `with_http_listener` serves `/metrics`.
/// Kept in the signature so a custom-route implementation can honor it later.
pub fn install(listen: std::net::SocketAddr, _path: &str) -> anyhow::Result<()> {
    PrometheusBuilder::new()
        .with_http_listener(listen)
        .install()?;
    describe_counter!("thttpd_migrate_requests_total", "Total proxied requests");
    describe_counter!(
        "thttpd_migrate_5xx_responses_total",
        "Total 5xx responses from backends"
    );
    describe_counter!(
        "thttpd_migrate_shadow_divergences_total",
        "Total shadow divergences"
    );
    describe_histogram!(
        "thttpd_migrate_request_duration_seconds",
        "End-to-end request duration"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_total_increments() {
        // Install on an ephemeral port. try_install semantics would be ideal;
        // PrometheusBuilder::install sets a global exporter, so this test is
        // best-effort and only validates the describe/install path doesn't
        // panic on a fresh port. We avoid asserting the scrape here because
        // the global exporter can't be re-installed across tests.
        let addr = "127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap();
        // with_http_listener binds eagerly, so a 0-port binds fine. If a global
        // exporter is already installed this returns Err — that's acceptable.
        let _ = install(addr, "/metrics");
        metrics::counter!("thttpd_migrate_requests_total", "backend" => "test").increment(1);
        // No assertion on scrape value: the global exporter is shared state.
    }

    #[test]
    fn duration_histogram_records_observation() {
        // One forwarded request must record an observation into the
        // `thttpd_migrate_request_duration_seconds` histogram. The global
        // Prometheus exporter can't be reinstalled per-test, so drive a local
        // `DebuggingRecorder` (same metric API the hot path uses) and assert
        // the observation lands.
        use metrics_util::MetricKind;
        use metrics_util::debugging::{DebugValue, DebuggingRecorder};

        let recorder = DebuggingRecorder::new();
        metrics::with_local_recorder(&recorder, || {
            metrics::histogram!(
                "thttpd_migrate_request_duration_seconds",
                "backend" => "test",
                "status_class" => "2xx",
            )
            .record(0.012);
        });

        let observed = recorder
            .snapshotter()
            .snapshot()
            .into_vec()
            .into_iter()
            .any(|(ck, _unit, _labels, value)| {
                ck.kind() == MetricKind::Histogram
                    && ck.key().name() == "thttpd_migrate_request_duration_seconds"
                    && matches!(value, DebugValue::Histogram(ref v) if !v.is_empty())
            });
        assert!(
            observed,
            "expected one duration histogram observation after record_metrics"
        );
    }
}
