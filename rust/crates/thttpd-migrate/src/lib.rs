//! Strangler-fig migration proxy for thttpd → thttpd-rs.
//!
//! See `.rpiv/artifacts/plans/2026-06-12_16-30-00_strangler-fig-proxy.md`.

pub mod backend;
pub mod circuit;
pub mod config;
pub mod control;
pub mod diff;
pub mod forwarder;
pub mod health;
pub mod metrics;
pub mod router;
pub mod server;
pub mod shadow;
pub mod state;
pub mod tracing_setup;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

/// Resolve the effective log level: a CLI `--log-level` (Some) wins; otherwise
/// fall back to the config's `log_level`. Extracted so it can be unit-tested.
fn effective_log_level<'a>(cli: Option<&'a str>, config: &'a str) -> &'a str {
    cli.unwrap_or(config)
}

// Initialized in [`start`] via [`tracing_setup::init`].
fn init_tracing(log_level: &str) {
    let json = std::env::var("THTTPD_MIGRATE_LOG_FORMAT")
        .map(|v| v == "json")
        .unwrap_or(false);
    tracing_setup::init(log_level, json);
}

pub async fn start(
    config: Option<PathBuf>,
    _listen: SocketAddr,
    log_level: Option<String>,
) -> anyhow::Result<()> {
    // Load the config FIRST so a TOML `log_level = "debug"` is honored when the
    // CLI omits `--log-level`. Previously tracing was initialized from the
    // CLI's unconditional "info" default before the config was read, so the
    // documented `log_level` field had no effect. CLI `--log-level` still wins.
    let cfg_path = config.ok_or_else(|| anyhow::anyhow!("--config is required after Phase 2"))?;
    let cfg = config::load(&cfg_path)?;
    let level = effective_log_level(log_level.as_deref(), &cfg.log_level);
    init_tracing(level);
    let state = Arc::new(state::LiveState::new(cfg.clone()));
    let pool = Arc::new(backend::BackendPool::with_breaker_cfg(
        &cfg.backends,
        cfg.circuit_breaker.clone(),
    ));
    for b in pool.iter() {
        tracing::info!(
            backend = %b.name,
            address = %b.config.read().address,
            weight = b.weight(),
            health = ?b.health(),
            "backend registered"
        );
    }
    // After Phase 2, listen address comes from config.listen; --listen is ignored.
    let addr: SocketAddr = cfg.listen.parse()?;
    // Prometheus metrics on a separate listener (Phase 6).
    let metrics_addr: SocketAddr = cfg.metrics.listen.parse()?;
    if let Err(e) = crate::metrics::install(metrics_addr, &cfg.metrics.path) {
        tracing::warn!(error = %e, addr = %metrics_addr, "metrics exporter install failed");
    }
    let client = forwarder::build_client();
    // Active health probing.
    let _health = health::spawn_checker(pool.clone(), client.clone(), cfg.health.clone());
    // Control plane + periodic state.json writer (Phase 7).
    let control_socket = Arc::new(std::path::PathBuf::from(cfg.control_socket.clone()));
    let state_path = Arc::new(std::path::PathBuf::from(cfg.state_path.clone()));
    let _control = control::spawn_server(
        state.clone(),
        pool.clone(),
        control_socket,
        state_path.clone(),
    )?;
    let _writer = control::spawn_state_writer(state.clone(), pool.clone(), state_path);
    server::run_proxy(addr, pool, cfg.routing, cfg.shadow, state, client).await
}

pub fn status(state: PathBuf) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(&state)
        .map_err(|e| anyhow::anyhow!("failed to read state file {}: {e}", state.display()))?;
    let snap: state::StateSnapshot = serde_json::from_str(&text)?;
    println!("Uptime:  {}s", snap.uptime_secs);
    println!("Draining: {}", snap.draining);
    println!("Backends:");
    for b in &snap.backends {
        println!(
            "  {:<16} {:<22} weight={:<4} health={}",
            b.name, b.address, b.weight, b.health
        );
    }
    Ok(())
}

/// Parse `backend=weight` pairs from the CLI.
fn parse_weight_pairs(pairs: &[String]) -> anyhow::Result<std::collections::HashMap<String, u32>> {
    let mut weights = std::collections::HashMap::new();
    for p in pairs {
        let (name, val) = p
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("expected backend=weight, got {p}"))?;
        let weight: u32 = val.parse()?;
        weights.insert(name.to_string(), weight);
    }
    Ok(weights)
}

pub async fn set_weight(control_socket: PathBuf, pairs: Vec<String>) -> anyhow::Result<()> {
    let weights = parse_weight_pairs(&pairs)?;
    let resp = control::client_set_weight(&control_socket, weights).await?;
    if !resp.ok {
        anyhow::bail!(resp.message);
    }
    println!("{}", resp.message);
    Ok(())
}

pub async fn drain(control_socket: PathBuf, timeout_secs: u64) -> anyhow::Result<()> {
    let resp = control::client_drain(&control_socket, timeout_secs).await?;
    if !resp.ok {
        anyhow::bail!(resp.message);
    }
    println!("{}", resp.message);
    Ok(())
}

pub async fn rollback(control_socket: PathBuf, to: &str) -> anyhow::Result<()> {
    let resp = control::client_rollback(&control_socket, to).await?;
    if !resp.ok {
        anyhow::bail!(resp.message);
    }
    println!("{}", resp.message);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_log_level_cli_overrides_config() {
        // P2 (Honor the configured log level): --log-level on the CLI wins.
        assert_eq!(effective_log_level(Some("debug"), "info"), "debug");
        assert_eq!(effective_log_level(Some("error"), "debug"), "error");
    }

    #[test]
    fn effective_log_level_uses_config_when_cli_absent() {
        // No --log-level → the TOML `log_level` field takes effect (previously
        // the unconditional "info" default masked it).
        assert_eq!(effective_log_level(None, "warn"), "warn");
        assert_eq!(effective_log_level(None, "debug"), "debug");
    }
}
