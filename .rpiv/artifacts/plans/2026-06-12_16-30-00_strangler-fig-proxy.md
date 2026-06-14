---
date: 2026-06-12T16:30:00-0300
author: Burke T
commit: e500de0
branch: main
repository: thttpd-rs
topic: "Strangler-Fig Migration Proxy (thttpd-migrate)"
tags: [plan, migration, proxy, strangler-fig, canary, shadow, rollout, rollback]
status: in-progress
parent: null
phase_count: 9
phases:
  - { n: 1, title: Workspace + CLI skeleton }
  - { n: 2, title: Config schema + backend registry }
  - { n: 3, title: Request routing + active/active }
  - { n: 4, title: Shadow mode + response diffing }
  - { n: 5, title: Health checks + circuit breaker }
  - { n: 6, title: Observability (tracing + Prometheus /metrics) }
  - { n: 7, title: Graceful drain + rollback }
  - { n: 8, title: Integration tests }
  - { n: 9, title: Documentation + runbooks }
last_updated: 2026-06-13T23:55:00-0300
last_updated_by: pi
last_updated_note: "Third-party review: fixed 4 compile blockers and 9 concerns; dead params, control plane, tracing wiring, config authority."
---

# Strangler-Fig Migration Proxy — Implementation Plan

## Overview

This plan adds a new migration proxy binary, **`thttpd-migrate`**, that sits in front of the existing C thttpd and the new Rust `thttpd-rs` and implements the canonical *strangler fig* migration pattern:

1. **Active/active** routing: send N% of traffic to Rust, (100-N)% to C
2. **Shadow** mode: mirror 100% of live traffic to Rust, log divergences, never affect users
3. **Canary** mode: gradually ramp Rust traffic 1% → 10% → 50% → 100%
4. **Hot weight adjustment** with one-command promote / rollback
5. **Circuit breaker** on Rust backend failure
6. **Graceful drain** for planned cutover
7. **Full rollback** in 30 seconds, no traffic loss

The proxy is a **new component** — it does not modify the existing `thttpd-rs` server, and the server keeps its `mio`-based single-threaded architecture.

### Implementation-readiness review notes

This plan was reviewed against the current repository state on 2026-06-13:

- Workspace membership currently lives in `rust/Cargo.toml:1-12`; adding `crates/thttpd-migrate` makes `make build`, `make check`, and CI include the proxy automatically because they already operate on the workspace.
- Existing dual-server fixtures live in `harness/conftest.py:345-650`; Phase 8 should extend those fixtures rather than inventing a parallel process harness.
- Existing comparison logic lives in `harness/diff_engine.py:32-325`; the Rust `diff.rs` port must preserve the current normalized profile rather than the older `1-180` line range.
- The existing `Makefile:31-48` does not yet run proxy-specific integration tests; Phase 8 must add a `proxy` target and include it in `integration` once the proxy tests exist.
- thttpd does not expose a built-in `/__healthz`; use `/` as the default `health_path` in examples/tests unless the fixture creates a real health file.
- Hyper 1 response/request bodies must use concrete body types such as `http_body_util::Full<bytes::Bytes>` or boxed bodies; snippets below avoid `Response<String>` and `Client<..., Empty<Bytes>>` for proxied requests.

### Architectural decision: async runtime

The existing `thttpd-rs` server deliberately uses `mio` directly with a manual event loop to match thttpd's C architecture. The proxy is a different kind of system — it manages many concurrent connections to multiple backends, and re-implementing that on `mio` would be busywork that hides the actual proxy logic.

**Decision:** the proxy uses `tokio` + `hyper`. Rationale:
- Proxy logic is naturally concurrent (many connections × multiple backends)
- The proxy is migration tooling, not part of the server's hot path
- Production proxies (Envoy, nginx, HAProxy) are async
- `tokio`'s task model maps cleanly to per-request proxying

**Trade-off:** introduces a new runtime to the project. ADR-0002 records this (created in Phase 9).

### Architecture

```
            ┌────────────────────────────────────────────────────────────┐
            │                  thttpd-migrate (proxy)                   │
            │                                                            │
   client ──┤  ┌──────────┐   ┌──────────────┐   ┌──────────────────┐    │
            │  │ listener │──▶│   router     │──▶│ active backend   │    │
            │  │ (hyper)  │   │  (weighted)  │   │   (C or Rust)    │    │
            │  └──────────┘   └──────┬───────┘   └──────────────────┘    │
            │                        │                                   │
            │                        │  ┌──────────────────┐             │
            │                        └─▶│  shadow backend │             │
            │                           │   (Rust only)    │             │
            │                           └────────┬─────────┘             │
            │                                    │                       │
            │                                    ▼                       │
            │                           ┌─────────────────┐              │
            │                           │  diff_engine    │              │
            │                           │  (port Phase 4) │              │
            │                           └─────────────────┘              │
            │                                                            │
            │  ┌──────────┐   ┌──────────────┐   ┌──────────────────┐    │
            │  │ health   │   │   circuit    │   │  /metrics        │    │
            │  │ checker  │   │   breaker    │   │  (Prometheus)    │    │
            │  └──────────┘   └──────────────┘   └──────────────────┘    │
            └────────────────────────────────────────────────────────────┘
                            │                                │
                            ▼                                ▼
                   ┌──────────────┐                 ┌──────────────┐
                   │  thttpd (C)  │                 │ thttpd-rs    │
                   │  :8081       │                 │ :8082        │
                   └──────────────┘                 └──────────────┘
```

## Desired End State

```bash
# Install
cargo install --path rust/crates/thttpd-migrate

# Create config (config/thttpd-migrate.example.toml is checked in)
cat > /etc/thttpd-migrate.toml <<EOF
listen = "127.0.0.1:8080"
log_level = "info"
state_path = "/var/run/thttpd-migrate/state.json"
control_socket = "/var/run/thttpd-migrate/control.sock"

[metrics]
listen = "127.0.0.1:9100"
path = "/metrics"

[shadow]
max_body_bytes = 1048576

[backends.c-thttpd]
address = "127.0.0.1:8081"
weight = 95
health_path = "/"

[backends.rust-thttpd]
address = "127.0.0.1:8082"
weight = 5
health_path = "/"

[routing]
mode = "active-active"  # | "shadow" | "canary"
primary_backend = "c-thttpd"     # required for shadow mode; ignored by weighted active-active
shadow_backend = "rust-thttpd"
exclude_paths = ["/internal/*", "/metrics"]

[health]
interval_ms = 1000
timeout_ms = 500
failure_threshold = 3
success_threshold = 2

[circuit_breaker]
error_rate_threshold = 0.5
window_secs = 30
min_requests = 20
EOF

# Start in canary mode
thttpd-migrate start --config /etc/thttpd-migrate.toml

# Promote Rust to 100% in 30 seconds (no downtime)
thttpd-migrate --control-socket /var/run/thttpd-migrate/control.sock set-weight rust-thttpd=100 c-thttpd=0

# Roll back to C in 30 seconds (one command, no traffic loss)
thttpd-migrate --control-socket /var/run/thttpd-migrate/control.sock rollback --to c-thttpd

# Inspect runtime state
thttpd-migrate status
# Backends:
#   c-thttpd     (primary)  :8081  healthy  1247 req/s  circuit=closed
#   rust-thttpd  (canary)   :8082  healthy     8 req/s  circuit=closed
# Routing: active-active 5% rust / 95% c
# Shadow:  rust-thttpd   mirror=100%  divergences_last_1h=0
# Uptime:  4h 12m
```

A successful end-to-end demo:
- Start both C and Rust servers
- Start proxy at 100% C, 0% Rust (control)
- Ramp 0% → 5% → 50% → 100% Rust over 5 minutes
- No 5xx errors attributable to the proxy
- `curl -H 'Host: example.com' localhost:8080/` returns identical headers and body to `curl localhost:8081/` (up to Date header)
- `thttpd-migrate rollback` restores all traffic to C within 30 seconds with zero failed requests
- `curl http://localhost:9100/metrics` shows `thttpd_migrate_requests_total{backend="..."}` incrementing

## What We're NOT Doing

- **L7 features** beyond routing (no body rewriting, header injection beyond pass-through)
- **TLS termination** — terminate TLS at a separate proxy (envoy/nginx/caddy) in front
- **Service discovery** — backends are configured statically; DNS-based discovery is deferred
- **Multi-region / global load balancing** — single-region only
- **Admin UI** — CLI and Prometheus metrics only; no web UI in this phase
- **Full hot config reload** — weight changes use the control socket; broader config changes still require restart
- **HTTP/2 to backends** — HTTP/1.1 only, matching thttpd-rs and thttpd capabilities
- **Unbounded request/response buffering** — active routing streams; shadow diffing buffers request/response bodies only up to a documented cap and records a truncation divergence above that cap
- **Modifying thttpd-rs** — the server stays as-is
- **Async runtime adoption in the server** — ADR-0002 records the deliberate split

---

## Phase 1: Workspace + CLI skeleton

### Overview
Scaffold the new `thttpd-migrate` crate as a workspace member, wire up the CLI with `clap`, and produce a hello-world binary that responds to `--help` and starts an HTTP server on a configurable port returning `200 OK` on every request. Foundation for all subsequent phases.

### Changes Required:

#### 1. Add crate to workspace
**File**: `rust/Cargo.toml`
**Changes**: MODIFY — add `crates/thttpd-migrate` to `members`.

```toml
members = [
    "crates/thttpd-core",
    "crates/thttpd-http",
    "crates/thttpd-fdwatch",
    "crates/thttpd-timers",
    "crates/thttpd-mmc",
    "crates/thttpd-match",
    "crates/thttpd-tdate",
    "crates/thttpd-mime",
    "crates/thttpd-migrate",  # NEW
]
```

#### 2. New crate manifest
**File**: `rust/crates/thttpd-migrate/Cargo.toml`
**Changes**: NEW — binary crate with async deps.

```toml
[package]
name = "thttpd-migrate"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true
description = "Strangler-fig migration proxy for thttpd → thttpd-rs"

[[bin]]
name = "thttpd-migrate"
path = "src/main.rs"

[dependencies]
tokio = { version = "1", features = ["full"] }
hyper = { version = "1", features = ["server", "client", "http1"] }
hyper-util = { version = "0.1", features = ["tokio", "client-legacy", "http1"] }
clap = { workspace = true }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
anyhow = "1"
thiserror = { workspace = true }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
metrics = "0.23"
metrics-exporter-prometheus = { version = "0.15", default-features = false, features = ["http-listener"] }
uuid = { version = "1", features = ["v4"] }
arc-swap = "1"
http-body-util = "0.1"
bytes = "1"
rand = "0.8"
parking_lot = "0.12"

[dev-dependencies]
wiremock = "0.6"
tempfile = { workspace = true }
```

#### 3. CLI entry
**File**: `rust/crates/thttpd-migrate/src/main.rs`
**Changes**: NEW — clap subcommand dispatch with `tokio::main`.

```rust
use clap::{Parser, Subcommand};
use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "thttpd-migrate",
    version,
    about = "Strangler-fig proxy for thttpd → thttpd-rs migration"
)]
struct Cli {
    /// Control socket used by mutating commands once Phase 7 lands.
    #[arg(long, global = true, default_value = "/var/run/thttpd-migrate/control.sock")]
    control_socket: PathBuf,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start the proxy
    Start {
        /// Full TOML config. Optional in Phase 1; required once Phase 2 lands.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Phase 1 skeleton bind address. Phase 2 reads this from config.
        #[arg(long, default_value = "127.0.0.1:8080")]
        listen: SocketAddr,
        /// Override log level (trace, debug, info, warn, error)
        #[arg(long, default_value = "info")]
        log_level: String,
    },
    /// Print current runtime state (backends, weights, divergences)
    Status {
        #[arg(long, default_value = "/var/run/thttpd-migrate/state.json")]
        state: PathBuf,
    },
    /// Hot-weight adjustment: thttpd-migrate set-weight BACKEND=WEIGHT ...
    SetWeight {
        /// backend=new_weight pairs, e.g. rust-thttpd=100 c-thttpd=0
        #[arg(required = true)]
        pairs: Vec<String>,
    },
    /// Graceful drain: stop accepting, finish in-flight, exit
    Drain {
        #[arg(long, default_value = "30")]
        timeout_secs: u64,
    },
    /// Emergency rollback: redirect all traffic to named backend
    Rollback {
        /// Backend name to roll back to (must be a configured backend)
        #[arg(long)]
        to: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Start { config, listen, log_level } => thttpd_migrate::start(config, listen, log_level).await,
        Cmd::Status { state } => thttpd_migrate::status(state),
        Cmd::SetWeight { pairs } => thttpd_migrate::set_weight(cli.control_socket, pairs),
        Cmd::Drain { timeout_secs } => thttpd_migrate::drain(cli.control_socket, timeout_secs).await,
        Cmd::Rollback { to } => thttpd_migrate::rollback(cli.control_socket, &to),
    }
}
```

#### 4. Lib root with module stubs
**File**: `rust/crates/thttpd-migrate/src/lib.rs`
**Changes**: NEW — only declare modules that exist in Phase 1. Later phases add their modules when files are created; do not declare future modules without stub files, or the crate will not compile.

```rust
//! Strangler-fig migration proxy for thttpd → thttpd-rs.
//!
//! See `.rpiv/artifacts/plans/2026-06-12_16-30-00_strangler-fig-proxy.md`.

pub mod server;

use std::net::SocketAddr;
use std::path::PathBuf;

pub async fn start(config: Option<PathBuf>, listen: SocketAddr, _log_level: String) -> anyhow::Result<()> {
    if let Some(path) = config {
        anyhow::ensure!(path.exists(), "config file not found: {}", path.display());
    }
    server::run_skeleton(listen).await
}

pub fn status(_state: PathBuf) -> anyhow::Result<()> {
    anyhow::bail!("status is not available until Phase 7 state file support is implemented")
}
pub fn set_weight(_control_socket: PathBuf, _pairs: Vec<String>) -> anyhow::Result<()> {
    anyhow::bail!("set-weight is not available until Phase 7 control socket support is implemented")
}
pub async fn drain(_control_socket: PathBuf, _timeout_secs: u64) -> anyhow::Result<()> {
    anyhow::bail!("drain is not available until Phase 7 control socket support is implemented")
}
pub fn rollback(_control_socket: PathBuf, _to: &str) -> anyhow::Result<()> {
    anyhow::bail!("rollback is not available until Phase 7 control socket support is implemented")
}
```

#### 5. Hello-world hyper server
**File**: `rust/crates/thttpd-migrate/src/server.rs`
**Changes**: NEW — minimal hyper server returning 200 OK on every request. Real routing comes in Phase 3.

```rust
use bytes::Bytes;
use http_body_util::Full;
use hyper::{Request, Response, body::Incoming, service::service_fn};
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tracing::info;

type SkeletonBody = Full<Bytes>;

pub async fn run_skeleton(listen: SocketAddr) -> anyhow::Result<()> {
    let listener = TcpListener::bind(listen).await?;
    info!(addr = %listen, "thttpd-migrate skeleton listening");
    loop {
        let (stream, peer) = listener.accept().await?;
        let io = TokioIo::new(stream);
        tokio::spawn(async move {
            let svc = service_fn(|_req: Request<Incoming>| async {
                Ok::<_, Infallible>(Response::<SkeletonBody>::new(Full::new(Bytes::from_static(b"ok"))))
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
```

### Success Criteria:

#### Automated Verification:
- [x] `cargo build --manifest-path rust/Cargo.toml --workspace` succeeds
- [x] `cargo clippy --manifest-path rust/Cargo.toml -p thttpd-migrate --all-targets -- -D warnings` passes
- [x] `cargo run --manifest-path rust/Cargo.toml -p thttpd-migrate -- --help` lists `start`, `status`, `set-weight`, `drain`, `rollback`
- [x] `cargo run --manifest-path rust/Cargo.toml -p thttpd-migrate -- start --config /nonexistent.toml` exits non-zero with `config file not found: /nonexistent.toml`
- [x] `cargo test --manifest-path rust/Cargo.toml -p thttpd-migrate` passes (placeholder test in `lib.rs`)
- [x] `make security` passes after adding the new dependencies — if `cargo deny` rejects a transitive license variant, add it to `deny.toml` `[licenses] allow` with justification (tokio/hyper/metrics pull in several transitive crates)

#### Manual Verification:
- [ ] `cargo run --manifest-path rust/Cargo.toml -p thttpd-migrate -- start --listen 127.0.0.1:<port>` binds to the port; `curl localhost:<port>/` returns `200 OK` with body `ok`
- [ ] Ctrl-C terminates the skeleton process without leaving a listening socket behind (structured graceful shutdown is added in Phase 7)
- [ ] Log output goes to stderr at the configured level; JSON log format is added in Phase 6

---

## Phase 2: Config schema + backend registry

### Overview
Add the TOML config schema, validation, and the in-memory backend registry. Config is loaded once at startup; weights are hot-swappable via `arc-swap` in Phase 7, but the schema is fixed in this phase.

### Changes Required:

#### 1. Config types
**File**: `rust/crates/thttpd-migrate/src/config.rs`
**Changes**: NEW — typed config with serde + thiserror validation.

```rust
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Deserialize, Clone)]
pub struct ProxyConfig {
    pub listen: String,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default = "default_state_path")]
    pub state_path: String,
    #[serde(default = "default_control_socket")]
    pub control_socket: String,
    #[serde(default)]
    pub metrics: MetricsConfig,
    #[serde(default)]
    pub shadow: ShadowConfig,
    #[serde(default)]
    pub backends: HashMap<String, BackendConfig>,
    #[serde(default)]
    pub routing: RoutingConfig,
    #[serde(default)]
    pub health: HealthConfig,
    #[serde(default)]
    pub circuit_breaker: CircuitConfig,
}

fn default_log_level() -> String { "info".into() }
fn default_state_path() -> String { "/var/run/thttpd-migrate/state.json".into() }
fn default_control_socket() -> String { "/var/run/thttpd-migrate/control.sock".into() }

#[derive(Debug, Deserialize, Clone)]
pub struct MetricsConfig {
    #[serde(default = "default_metrics_listen")]
    pub listen: String,
    #[serde(default = "default_metrics_path")]
    pub path: String,
}
fn default_metrics_listen() -> String { "127.0.0.1:9100".into() }
fn default_metrics_path() -> String { "/metrics".into() }

#[derive(Debug, Deserialize, Clone)]
pub struct ShadowConfig {
    #[serde(default = "default_shadow_max_body_bytes")]
    pub max_body_bytes: usize,
}
fn default_shadow_max_body_bytes() -> usize { 1_048_576 }

#[derive(Debug, Deserialize, Clone)]
pub struct BackendConfig {
    pub address: String,
    #[serde(default = "default_weight")]
    pub weight: u32,
    #[serde(default = "default_health_path")]
    pub health_path: String,
}
fn default_weight() -> u32 { 1 }
fn default_health_path() -> String { "/".into() }

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RoutingMode { #[default] ActiveActive, Shadow, Canary }

#[derive(Debug, Deserialize, Clone)]
pub struct RoutingConfig {
    #[serde(default)]
    pub mode: RoutingMode,
    /// Live backend in shadow mode. This prevents shadow mode from accidentally serving Rust.
    pub primary_backend: Option<String>,
    /// Backend that receives mirrored requests in shadow mode.
    pub shadow_backend: Option<String>,
    #[serde(default)]
    pub exclude_paths: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct HealthConfig {
    #[serde(default = "default_health_interval")]
    pub interval_ms: u64,
    #[serde(default = "default_health_timeout")]
    pub timeout_ms: u64,
    #[serde(default = "default_failure_threshold")]
    pub failure_threshold: u32,
    #[serde(default = "default_success_threshold")]
    pub success_threshold: u32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct CircuitConfig {
    #[serde(default = "default_error_rate")]
    pub error_rate_threshold: f64,
    #[serde(default = "default_window")]
    pub window_secs: u64,
    #[serde(default = "default_min_requests")]
    pub min_requests: u32,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            mode: RoutingMode::default(),
            primary_backend: None,
            shadow_backend: None,
            exclude_paths: Vec::new(),
        }
    }
}
impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            listen: default_metrics_listen(),
            path: default_metrics_path(),
        }
    }
}
impl Default for ShadowConfig {
    fn default() -> Self {
        Self { max_body_bytes: default_shadow_max_body_bytes() }
    }
}
impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            interval_ms: default_health_interval(),
            timeout_ms: default_health_timeout(),
            failure_threshold: default_failure_threshold(),
            success_threshold: default_success_threshold(),
        }
    }
}
impl Default for CircuitConfig {
    fn default() -> Self {
        Self {
            error_rate_threshold: default_error_rate(),
            window_secs: default_window(),
            min_requests: default_min_requests(),
        }
    }
}

fn default_health_interval() -> u64 { 1000 }
fn default_health_timeout() -> u64 { 500 }
fn default_failure_threshold() -> u32 { 3 }
fn default_success_threshold() -> u32 { 2 }
fn default_error_rate() -> f64 { 0.5 }
fn default_window() -> u64 { 30 }
fn default_min_requests() -> u32 { 20 }

pub fn load(path: &Path) -> anyhow::Result<ProxyConfig> {
    let text = std::fs::read_to_string(path)?;
    let cfg: ProxyConfig = toml::from_str(&text)?;
    validate(&cfg)?;
    Ok(cfg)
}

pub fn validate(cfg: &ProxyConfig) -> anyhow::Result<()> {
    anyhow::ensure!(!cfg.backends.is_empty(), "at least one backend required");
    let total_weight: u32 = cfg.backends.values().map(|b| b.weight).sum();
    anyhow::ensure!(total_weight > 0, "at least one backend must have weight > 0");
    if matches!(cfg.routing.mode, RoutingMode::Shadow) {
        let primary = cfg.routing.primary_backend.as_deref();
        let shadow = cfg.routing.shadow_backend.as_deref();
        anyhow::ensure!(primary.is_some(), "routing.mode = shadow requires routing.primary_backend");
        anyhow::ensure!(shadow.is_some(), "routing.mode = shadow requires routing.shadow_backend");
        anyhow::ensure!(primary != shadow, "routing.primary_backend and routing.shadow_backend must differ");
        anyhow::ensure!(cfg.backends.contains_key(primary.unwrap()), "routing.primary_backend names an unknown backend");
        anyhow::ensure!(cfg.backends.contains_key(shadow.unwrap()), "routing.shadow_backend names an unknown backend");
    }
    cfg.listen.parse::<std::net::SocketAddr>()
        .map_err(|e| anyhow::anyhow!("listen must be host:port, e.g. 127.0.0.1:8080: {e}"))?;
    cfg.metrics.listen.parse::<std::net::SocketAddr>()
        .map_err(|e| anyhow::anyhow!("metrics.listen must be host:port, e.g. 127.0.0.1:9100: {e}"))?;
    Ok(())
}

impl HealthConfig {
    pub fn interval(&self) -> Duration { Duration::from_millis(self.interval_ms) }
    pub fn timeout(&self) -> Duration { Duration::from_millis(self.timeout_ms) }
}
```

#### 2. Example config (checked in)
**File**: `config/thttpd-migrate.example.toml`
**Changes**: NEW — fully documented example.

```toml
# thttpd-migrate example configuration.
# Copy to /etc/thttpd-migrate.toml and edit.

listen = "127.0.0.1:8080"
log_level = "info"
state_path = "/var/run/thttpd-migrate/state.json"
control_socket = "/var/run/thttpd-migrate/control.sock"

[metrics]
listen = "127.0.0.1:9100"
path = "/metrics"

[shadow]
max_body_bytes = 1048576

[backends.c-thttpd]
address = "127.0.0.1:8081"
weight = 95                # 95% of traffic in active-active
health_path = "/"          # thttpd has no built-in /__healthz

[backends.rust-thttpd]
address = "127.0.0.1:8082"
weight = 5                 # 5% canary
health_path = "/"

[routing]
mode = "active-active"     # | "shadow" | "canary"
primary_backend = "c-thttpd"      # required only for shadow mode
shadow_backend = "rust-thttpd"
exclude_paths = ["/internal/*", "/metrics"]   # never proxied, returned 404

[health]
interval_ms = 1000
timeout_ms = 500
failure_threshold = 3      # consecutive failures → mark unhealthy
success_threshold = 2      # consecutive successes → mark healthy

[circuit_breaker]
error_rate_threshold = 0.5 # 50% errors in window → open circuit
window_secs = 30
min_requests = 20          # don't trip below this volume
```

#### 3. Backend registry (in-memory)
**File**: `rust/crates/thttpd-migrate/src/backend.rs`
**Changes**: NEW — backend handle, pool, and lookup.

```rust
use crate::config::BackendConfig;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health { Healthy = 0, Degraded = 1, Unhealthy = 2 }

impl Health {
    pub fn from_u8(value: u8) -> Self {
        match value {
            0 => Health::Healthy,
            1 => Health::Degraded,
            _ => Health::Unhealthy,
        }
    }
}

pub struct Backend {
    pub name: String,
    pub config: BackendConfig,
    pub health: AtomicU8,        // Health
    pub consecutive_failures: AtomicU32,
    pub consecutive_successes: AtomicU32,
}

impl Backend {
    pub fn new(name: String, config: BackendConfig) -> Arc<Self> {
        Arc::new(Self {
            name,
            config,
            health: AtomicU8::new(Health::Healthy as u8),
            consecutive_failures: AtomicU32::new(0),
            consecutive_successes: AtomicU32::new(0),
        })
    }
    pub fn health(&self) -> Health { Health::from_u8(self.health.load(Ordering::Relaxed)) }
    pub fn is_routable(&self) -> bool { self.health() != Health::Unhealthy }
}

pub struct BackendPool {
    pub backends: HashMap<String, Arc<Backend>>,
}

impl BackendPool {
    pub fn from_config(backends: &HashMap<String, BackendConfig>) -> Self {
        let backends = backends.iter()
            .map(|(name, cfg)| (name.clone(), Backend::new(name.clone(), cfg.clone())))
            .collect();
        Self { backends }
    }
    pub fn get(&self, name: &str) -> Option<Arc<Backend>> { self.backends.get(name).cloned() }
    pub fn iter(&self) -> impl Iterator<Item = &Arc<Backend>> { self.backends.values() }
}
```

#### 4. Create stub files for future modules
**File**: `rust/crates/thttpd-migrate/src/{router,forwarder,diff,shadow,health,circuit,tracing_setup,metrics,state,control,drain}.rs`
**Changes**: NEW — each file is empty (or contains a single `// Phase N stub` comment). This allows lib.rs to declare all modules from Phase 2 without compilation errors. Each phase fills in its module.

#### 5. Wire config into `start()`
**File**: `rust/crates/thttpd-migrate/src/lib.rs`
**Changes**: MODIFY — declare all modules that will exist across the entire plan (`config`, `backend`, `server`, `router`, `forwarder`, `diff`, `shadow`, `health`, `circuit`, `tracing_setup`, `metrics`, `state`, `control`, `drain`). Only `config`, `backend`, and `server` have real code in Phase 2; the rest are empty stub files. This avoids modifying lib.rs again until Phase 6 (which replaces the tracing stub). Add a stub `init_tracing()` (real implementation arrives in Phase 6); `start()` loads config, builds the pool, calls `server::run_skeleton`. Other commands still bail with actionable errors.

```rust
//! Strangler-fig migration proxy for thttpd → thttpd-rs.
//!
//! See `.rpiv/artifacts/plans/2026-06-12_16-30-00_strangler-fig-proxy.md`.

pub mod backend;
pub mod circuit;
pub mod config;
pub mod control;
pub mod diff;
pub mod drain;
pub mod forwarder;
pub mod health;
pub mod metrics;
pub mod router;
pub mod server;
pub mod shadow;
pub mod state;
pub mod tracing_setup;

// Stub: replaced by tracing_setup::init in Phase 6.
fn init_tracing(_level: &str) {
    // Phase 2: tracing-subscriber not yet wired; default stderr logging suffices.
}

pub async fn start(config: Option<std::path::PathBuf>, _listen: std::net::SocketAddr, _log_level: String) -> anyhow::Result<()> {
    init_tracing(&_log_level);
    let cfg_path = config.ok_or_else(|| anyhow::anyhow!("--config is required after Phase 2"))?;
    let cfg = config::load(&cfg_path)?;
    let _pool = backend::BackendPool::from_config(&cfg.backends);
    // After Phase 2, listen address comes from config.listen; --listen is ignored.
    let addr: std::net::SocketAddr = cfg.listen.parse()?;
    server::run_skeleton(addr).await
}

pub fn status(_state: std::path::PathBuf) -> anyhow::Result<()> {
    anyhow::bail!("status is not available until Phase 7 state file support is implemented")
}
pub fn set_weight(_control_socket: std::path::PathBuf, _pairs: Vec<String>) -> anyhow::Result<()> {
    anyhow::bail!("set-weight is not available until Phase 7 control socket support is implemented")
}
pub async fn drain(_control_socket: std::path::PathBuf, _timeout_secs: u64) -> anyhow::Result<()> {
    anyhow::bail!("drain is not available until Phase 7 control socket support is implemented")
}
pub fn rollback(_control_socket: std::path::PathBuf, _to: &str) -> anyhow::Result<()> {
    anyhow::bail!("rollback is not available until Phase 7 control socket support is implemented")
}
```

### Success Criteria:

#### Automated Verification:
- [x] `cargo test --manifest-path rust/Cargo.toml -p thttpd-migrate config::tests::loads_example_config` passes
- [x] `config::tests::rejects_empty_backends` passes
- [x] `config::tests::rejects_zero_total_weight` passes
- [x] `config::tests::shadow_requires_primary_and_shadow_backends` passes
- [x] `config::tests::rejects_unknown_primary_or_shadow_backend` passes
- [x] Loading `config/thttpd-migrate.example.toml` parses without error
- [x] Invalid config (e.g. `listen = ":8080"` or `weight = 0` for all backends) returns a clear error message naming the field

#### Manual Verification:
- [ ] `thttpd-migrate start --config config/thttpd-migrate.example.toml` starts; log line shows backends registered with their weights
- [ ] Backend health default state is `Healthy` in startup logs

---

## Phase 3: Request routing + active/active

### Overview
Replace the hello-world server with a real router: weighted random selection across healthy backends, request forwarding (method, headers, body, query string preserved), response streaming. This is the core proxy functionality.

### Changes Required:

#### 1. Router (weighted selection)
**File**: `rust/crates/thttpd-migrate/src/router.rs`
**Changes**: NEW — given a request and the live pool, pick a backend.

```rust
use crate::backend::{Backend, BackendPool};
use hyper::Request;
use std::sync::Arc;
use std::time::Instant;

// Clone so a shadow task can take its own copy into a `tokio::spawn` future.
#[derive(Clone)]
pub struct RoutingDecision {
    pub backend: Arc<Backend>,
    pub shadow: Option<Arc<Backend>>,   // for shadow mode
    pub request_id: String,
    pub started_at: Instant,
}

// `req` is intentionally unused: path-exclusion is decided by the server handler.
// It is kept in the signature so future request-affinity routing has a hook.
pub fn decide<B>(
    _req: &Request<B>,
    pool: &BackendPool,
    routing: &crate::config::RoutingConfig,
) -> Option<RoutingDecision> {
    let request_id = uuid::Uuid::new_v4().to_string();

    let chosen = match routing.mode {
        // Shadow mode must always serve the configured primary backend. Rust receives mirrors only.
        crate::config::RoutingMode::Shadow => {
            let name = routing.primary_backend.as_deref()?;
            let backend = pool.get(name)?;
            if !backend.is_routable() { return None; }
            backend
        }
        // Canary is operationally distinct but mechanically the same weighted selection as active-active.
        crate::config::RoutingMode::ActiveActive | crate::config::RoutingMode::Canary => {
            let candidates: Vec<&Arc<Backend>> = pool.iter()
                .filter(|b| b.is_routable() && b.config.weight > 0)
                .collect();
            if candidates.is_empty() { return None; }
            let total: u32 = candidates.iter().map(|b| b.config.weight).sum();
            let pick = rand::random::<u32>() % total;
            let mut acc = 0u32;
            candidates.iter().find(|b| {
                acc += b.config.weight;
                pick < acc
            }).copied().cloned()?
        }
    };

    let shadow = if matches!(routing.mode, crate::config::RoutingMode::Shadow) {
        routing.shadow_backend.as_deref()
            .and_then(|n| pool.get(n))
            .filter(|b| b.name != chosen.name && b.is_routable())
    } else {
        None
    };

    Some(RoutingDecision {
        backend: chosen.clone(),
        shadow,
        request_id,
        started_at: Instant::now(),
    })
}
```

#### 2. Forwarder (HTTP/1.1 to backend)
**File**: `rust/crates/thttpd-migrate/src/forwarder.rs`
**Changes**: NEW — opens (or reuses) a connection to the chosen backend, forwards the request, streams the response back.

```rust
use crate::router::RoutingDecision;
use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Request, Response, Uri};
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use std::time::Duration;

pub type ProxyBody = BoxBody<Bytes, hyper::Error>;
pub type ProxyClient = Client<HttpConnector, ProxyBody>;

pub fn empty_body() -> ProxyBody {
    Full::new(Bytes::new()).map_err(|never| match never {}).boxed()
}

pub fn build_client() -> ProxyClient {
    let mut connector = HttpConnector::new();
    connector.set_connect_timeout(Some(Duration::from_secs(2)));
    Client::builder(hyper_util::rt::TokioExecutor::new())
        .pool_idle_timeout(Duration::from_secs(30))
        .build(connector)
}

pub async fn forward(
    decision: &RoutingDecision,
    req: Request<ProxyBody>,
    client: &ProxyClient,
) -> Result<Response<Incoming>, ForwardError> { /* … */ }

#[derive(Debug, thiserror::Error)]
pub enum ForwardError {
    #[error("backend connection failed: {0}")]
    Connect(#[source] hyper_util::client::legacy::Error),
    #[error("backend request failed: {0}")]
    Request(#[source] hyper_util::client::legacy::Error),
    #[error("backend response timed out")]
    Timeout,
    #[error("backend not routable")]
    NotRoutable,
}
```

The forwarder constructs a backend absolute URI (`http://{backend.address}{path_and_query}`), preserves method and headers (except hop-by-hop headers), forwards the boxed body, and streams the response back without buffering in active-active/canary mode.

#### 3. Server loop with routing
**File**: `rust/crates/thttpd-migrate/src/server.rs`
**Changes**: MODIFY — replace skeleton with real handler. Returns `Response<ProxyBody>` because `Response<Incoming>` cannot be constructed for error responses (Incoming is a network stream, not buildable from bytes). Spawns request handlers into a `JoinSet` so Phase 7 drain can await them.

```rust
use crate::config::RoutingConfig;
use crate::forwarder::{ProxyBody, ProxyClient};
use crate::backend::BackendPool;
use crate::router;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Request, Response};
use std::sync::Arc;
use tokio::task::JoinSet;

async fn handle(
    req: Request<Incoming>,
    pool: Arc<BackendPool>,
    routing: RoutingConfig,
    excluded: Vec<String>,
    client: ProxyClient,
) -> Response<ProxyBody> {
    // 1. Exclude paths
    if is_excluded(req.uri().path(), &excluded) {
        return not_found();
    }
    // 2. Decide
    let decision = match router::decide(&req, &pool, &routing) {
        Some(d) => d,
        None => return backend_unavailable(),
    };
    // 3. Forward — convert the request body from Incoming to ProxyBody first
    //    (forwarder::forward takes Request<ProxyBody>), then map the backend's
    //    Incoming response body back to ProxyBody for the success path.
    //
    //    Implementation note: verify `Incoming: Body<Error = E>` at impl time.
    //    If the associated Error type is `hyper::Error`, `.map_err(|e| e).boxed()`
    //    yields `BoxBody<Bytes, hyper::Error>` = ProxyBody directly. If it is
    //    `Box<dyn Error + Send + Sync>` (hyper's BoxError), map with
    //    `.map_err(|e| hyper::Error::new(crate::forwarder::ForwardError::from(e)))`
    //    or switch ProxyBody to `BoxBody<Bytes, hyper::Error>` via an adapter.
    let (parts, body) = req.into_parts();
    let req = Request::from_parts(parts, body.map_err(|e| e).boxed());
    match forwarder::forward(&decision, req, &client).await {
        Ok(resp) => resp.map(|body| body.map_err(|e| e).boxed()),
        Err(e) => {
            tracing::error!(error = %e, backend = %decision.backend.name, "forward error");
            bad_gateway()
        }
    }
}

// In the accept loop, collect handles for Phase 7 drain:
// let mut in_flight = JoinSet::new();
// in_flight.spawn(handle(req, pool, routing, excluded, client));
```

### Success Criteria:

#### Automated Verification:
- [x] `router::tests::weighted_selection_respects_weights` (run 10k iterations; distribution within 5% of weight ratios)
- [x] `router::tests::unhealthy_backends_excluded` (set backend to Unhealthy, verify never picked)
- [x] `forwarder::tests::preserves_method_path_headers_body` against `wiremock`
- [x] `forwarder::tests::streams_large_response` (1MB body) — `body.frame().next()` returns chunks, not a single buffer
- [x] `cargo test --manifest-path rust/Cargo.toml -p thttpd-migrate` all pass

#### Manual Verification:
- [ ] Start two `nc -l` listeners on 8081 and 8082 (both respond with `Backend: c` or `Backend: rust`); proxy on 8080; 1000 curl requests → roughly the configured ratio lands on each
- [ ] Kill the rust backend mid-flight; subsequent requests all go to c
- [ ] `curl -X POST -d 'hello' localhost:8080/echo` — the request body reaches the backend (`nc` logs the body)

---

## Phase 4: Shadow mode + response diffing

### Overview
When shadow mode is enabled, every request is served by `routing.primary_backend` and mirrored to `routing.shadow_backend`. The shadow response is captured and diffed against the primary response using the same comparison logic that `harness/diff_engine.py` implements. Divergences are logged, never propagated to the user.

Implementation constraint: HTTP request bodies are one-shot streams. Shadow mode must buffer the inbound request body once, up to `shadow.max_body_bytes`, then rebuild equivalent primary and shadow requests from that buffer. Active-active/canary mode continues to stream without buffering. Primary and shadow response bodies are buffered up to the same cap for diffing; truncation is logged as a divergence field so the operator knows the comparison was partial.

### Changes Required:

#### 1. Port diff logic to Rust
**File**: `rust/crates/thttpd-migrate/src/diff.rs`
**Changes**: NEW — port `harness/diff_engine.py:32-325` (normalizers, body hashing, profile-aware response comparison). This avoids a Python subprocess on the hot path.

```rust
use hyper::{Response, body::Incoming};
use hyper::body::Bytes;
use http_body_util::BodyExt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field { Status, Headers, Body, ContentLength, ConnectionLifecycle }

pub struct Divergence {
    pub field: Field,
    pub expected: String,
    pub actual: String,
    pub path: String,
    pub method: String,
    pub truncated: bool,
}

/// Context from the inbound request needed by the diff engine.
pub struct RequestContext {
    pub path: String,
    pub method: String,
    pub request_id: String,
}

pub async fn diff_responses(
    primary_status: u16,
    primary_headers: &[(String, String)],
    primary_body: &Bytes,
    shadow_status: u16,
    shadow_headers: &[(String, String)],
    shadow_body: &Bytes,
    primary_truncated: bool,
    shadow_truncated: bool,
    ctx: &RequestContext,
    max_body_bytes: usize,
) -> Vec<Divergence> { /* … port of harness/diff_engine.py:32-325 */ }
```

The port preserves the normalizers from `diff_engine.py` (timestamp format-only, path temp-dir substitution, etc.) — these are deliberate engineering choices and we don't re-litigate them in the proxy. If either body was truncated (primary_truncated or shadow_truncated), the comparison records a `Body` divergence with `truncated: true` so the operator knows the comparison was partial.

#### 2. Shadow dispatcher
**File**: `rust/crates/thttpd-migrate/src/shadow.rs`
**Changes**: NEW — buffer the inbound request body and the primary response body, fire a shadow request via `tokio::spawn`, diff and log result. Uses the existing `forwarder::forward()` function (no separate `forward_raw` needed — the forwarder already accepts `Request<ProxyBody>`). Defines `rebuild_for_backend()` to clone method/URI/headers and reuse the buffered body.

```rust
use crate::config::ShadowConfig;
use crate::diff::{self, Divergence, Field, RequestContext};
use crate::forwarder::{self, ProxyBody, ProxyClient};
use crate::router::RoutingDecision;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Request, Response};

/// Rebuild a request for a different backend, reusing the buffered body.
fn rebuild_for_backend(original_uri: &hyper::Uri, method: &hyper::Method, headers: &[(String, String)], body: Bytes, backend_addr: &str) -> Request<ProxyBody> {
    let path_and_query = original_uri.path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    let uri: hyper::Uri = format!("http://{}{}", backend_addr, path_and_query).parse().unwrap();
    let mut req = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        req = req.header(k.as_str(), v.as_str());
    }
    req.body(Full::new(body).map_err(|never| match never {}).boxed()).expect("valid request")
}

/// Called from the server handler after the primary response is fully read.
/// Buffers the request body and primary response, spawns the shadow request.
/// Takes `decision` by value so the spawned future can own a copy of it
/// (spawns require `'static` futures; a borrowed `&RoutingDecision` would not).
pub fn dispatch_shadow(
    decision: RoutingDecision,
    method: hyper::Method,
    original_uri: hyper::Uri,
    headers: Vec<(String, String)>,
    body: Bytes,
    primary_status: u16,
    primary_headers: Vec<(String, String)>,
    primary_body: Bytes,
    primary_truncated: bool,
    client: ProxyClient,
    shadow_cfg: ShadowConfig,
) {
    let shadow = decision.shadow.clone().unwrap();
    let request_id = decision.request_id.clone();
    let path = original_uri.path().to_string();
    let method_str = method.to_string();
    tokio::spawn(async move {
        let shadow_req = rebuild_for_backend(&original_uri, &method, &headers, body, &shadow.config.address);
        let result = forwarder::forward(&decision, shadow_req, &client).await;
        let ctx = RequestContext { path, method: method_str, request_id: request_id.clone() };
        let divergences = match result {
            Ok(resp) => {
                let (parts, body_stream) = resp.into_parts();
                let shadow_status = parts.status.as_u16();
                let shadow_headers: Vec<(String, String)> = parts.headers.iter()
                    .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or_default().to_string()))
                    .collect();
                let (shadow_body, shadow_truncated) = read_with_cap(body_stream, shadow_cfg.max_body_bytes).await;
                diff::diff_responses(
                    primary_status, &primary_headers, &primary_body, primary_truncated,
                    shadow_status, &shadow_headers, &shadow_body, shadow_truncated,
                    &ctx, shadow_cfg.max_body_bytes,
                ).await
            }
            Err(e) => vec![Divergence {
                field: Field::ConnectionLifecycle,
                expected: "ok".into(),
                actual: format!("error: {e}"),
                path: ctx.path.clone(), method: ctx.method.clone(),
                truncated: false,
            }],
        };
        for d in divergences {
            tracing::warn!(
                request_id = %request_id,
                backend = %shadow.name,
                field = ?d.field,
                truncated = d.truncated,
                "shadow divergence"
            );
            metrics::counter!("thttpd_migrate_shadow_divergences_total",
                "backend" => shadow.name.clone(),
                "field" => format!("{:?}", d.field),
            ).increment(1);
        }
    });
}

/// Read a response body up to `max_bytes`. Returns (body, truncated).
async fn read_with_cap<B>(body: B, max_bytes: usize) -> (Bytes, bool)
where B: http_body::Body<Data = Bytes> {
    // … collect frames up to max_bytes, set truncated=true if exceeded
    todo!("implement frame collection with cap")
}
```

#### 3. Wire shadow into server loop
**File**: `rust/crates/thttpd-migrate/src/server.rs`
**Changes**: MODIFY — if `decision.shadow.is_some()`, buffer the inbound request body up to `shadow.max_body_bytes`, rebuild two requests, forward the primary request, buffer the primary response up to the cap, spawn the shadow request/diff, then return the primary response to the user. The user is not blocked on the shadow backend, but is blocked on reading the primary response body in shadow mode. This is acceptable for migration verification mode and is explicitly not used in active-active/canary streaming mode.

### Success Criteria:

#### Automated Verification:
- [x] `diff::tests::timestamp_headers_match_format_only` (proves the timestamp normalizer ported correctly)
- [x] `diff::tests::status_mismatch_caught`, `headers_mismatch_caught`, `body_mismatch_caught`
- [x] `diff::tests::temp_path_substitution` (e.g. `/tmp/thttpd_golden_xyz` vs `/tmp/pytest-abc` → no divergence)
- [x] `shadow::tests::shadow_mode_always_serves_primary_backend` (even when Rust weight is nonzero)
- [x] `shadow::tests::divergence_does_not_affect_user` (intentionally diverge the shadow backend; user response is unchanged)
- [x] `shadow::tests::large_body_over_cap_records_truncation_divergence` (no unbounded buffering)
- [x] `diff::tests::match_known_differential_test_outputs` — replay 5 synthetic records shaped like `harness/conftest.py:599-615` / `compare_responses_v2`, or generate a temporary baseline via `pipeline/run_golden_capture.py`; do not depend on a checked-in `harness/golden/baseline.json` because none exists today

#### Manual Verification:
- [ ] Configure shadow mode; start C on 8081, Rust on 8082; proxy on 8080
- [ ] Curl 100 times; check `/var/log/thttpd-migrate/shadow.log` for divergences (expect zero on identical binaries)
- [ ] Introduce a known Rust divergence (e.g. temporary wrong `Server` header); verify shadow log shows the divergence with request_id, field, expected, actual
- [ ] User-facing response is the primary's, never the shadow's

---

## Phase 5: Health checks + circuit breaker

### Overview
Active health probes hit each backend's `health_path` on a configurable interval. Passive health updates the consecutive-fail counter on every 5xx or connection error. Circuit breaker trips when the error rate over the rolling window exceeds the threshold and the minimum request volume is met.

### Changes Required:

#### 1. Active health checker
**File**: `rust/crates/thttpd-migrate/src/health.rs`
**Changes**: NEW — periodic probe task per backend.

```rust
use crate::backend::{Backend, BackendPool, Health};
use crate::config::HealthConfig;
use crate::forwarder::{empty_body, ProxyClient};
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
            for backend in pool.iter() {
                let url = format!("http://{}{}", backend.config.address, backend.config.health_path);
                let req = Request::get(url).body(empty_body()).expect("health request");
                // A successful probe is a 2xx status. Connection errors and
                // non-2xx responses (including 5xx) all count as failures.
                let success = match tokio::time::timeout(cfg.timeout(), client.request(req)).await {
                    Ok(Ok(resp)) => resp.status().is_success(),
                    Ok(Err(_)) | Err(_) => false,
                };
                update_health(backend, &cfg, success);
            }
            tokio::time::sleep(interval).await;
        }
    })
}

fn update_health(backend: &Backend, cfg: &HealthConfig, success: bool) {
    if success {
        backend.consecutive_failures.store(0, Ordering::Relaxed);
        let n = backend.consecutive_successes.fetch_add(1, Ordering::Relaxed) + 1;
        if n >= cfg.success_threshold && backend.health() != Health::Healthy {
            backend.health.store(Health::Healthy as u8, Ordering::Relaxed);
            tracing::info!(backend = %backend.name, "backend healthy");
        }
    } else {
        backend.consecutive_successes.store(0, Ordering::Relaxed);
        let n = backend.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
        if n >= cfg.failure_threshold && backend.health() != Health::Unhealthy {
            backend.health.store(Health::Unhealthy as u8, Ordering::Relaxed);
            tracing::warn!(backend = %backend.name, "backend unhealthy");
        }
    }
}
```

#### 2. Circuit breaker
**File**: `rust/crates/thttpd-migrate/src/circuit.rs`
**Changes**: NEW — rolling window of outcomes, per-backend state.

```rust
use crate::config::CircuitConfig;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State { Closed, Open, HalfOpen }

pub struct Breaker {
    pub state: AtomicU8,
    pub opened_at: parking_lot::Mutex<Option<Instant>>,
    pub cfg: CircuitConfig,
    pub window: parking_lot::Mutex<Window>,
}

pub struct Window {
    pub errors: u32,
    pub total: u32,
    pub started: Instant,
}

impl Window {
    pub fn new() -> Self { Self { errors: 0, total: 0, started: Instant::now() } }
}

impl Breaker {
    pub fn record(&self, success: bool) {
        let mut w = self.window.lock();
        if w.started.elapsed() > Duration::from_secs(self.cfg.window_secs) {
            *w = Window::new();
        }
        w.total += 1;
        if !success { w.errors += 1; }
        if w.total >= self.cfg.min_requests {
            let rate = w.errors as f64 / w.total as f64;
            if rate > self.cfg.error_rate_threshold {
                self.trip();
            }
        }
    }
    pub fn allows(&self) -> bool { /* Closed → yes; Open → no; HalfOpen → probe */ }
    fn trip(&self) { /* transition to Open, set opened_at */ }
}
```

#### 3. Wire breaker into forwarder
**File**: `rust/crates/thttpd-migrate/src/forwarder.rs`
**Changes**: MODIFY — after each forward, call `breaker.record(success)`. Router consults `breaker.allows()` before picking a backend.

### Success Criteria:

#### Automated Verification:
- [x] `health::tests::three_consecutive_failures_marks_unhealthy`
- [x] `health::tests::two_consecutive_successes_marks_healthy` (after recovery)
- [x] `health::tests::timeout_counts_as_failure`
- [x] `circuit::tests::below_min_requests_does_not_trip`
- [x] `circuit::tests::error_rate_above_threshold_trips`
- [x] `circuit::tests::half_open_probe_recovers`

#### Manual Verification:
- [ ] Start C on 8081 (responding), Rust on 8082 (not running); proxy on 8080
- [ ] Within 5s, logs report `rust-thttpd` as `Unhealthy` (the `status` command exposes the same state after Phase 7)
- [ ] All requests routed to C; zero 5xx from proxy
- [ ] Start Rust on 8082; within 5s logs report healthy again (and `status` reports it after Phase 7)
- [ ] Kill C under load (50 req/s); proxy circuit trips within window; all traffic shifts to Rust; no error responses to clients
- [ ] Health check overhead: with `failure_threshold=3` and `interval_ms=1000`, health probes add <1 req/s per backend; confirm via metrics scrape at 1, 10, and 50 backends

---

## Phase 6: Observability (tracing + Prometheus /metrics)

### Overview
Add structured logging via `tracing` and a Prometheus metrics endpoint. The metrics surface is what makes the proxy operable in production — every backend has a request counter, a duration histogram, and divergence/error counters.

### Changes Required:

#### 1. Tracing setup
**File**: `rust/crates/thttpd-migrate/src/tracing_setup.rs`
**Changes**: NEW — init tracing-subscriber with env filter, JSON output in prod, pretty in dev.

```rust
pub fn init(level: &str, json: bool) {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(level));
    let fmt_layer = if json {
        fmt::layer().json().with_current_span(true).with_span_list(false)
    } else {
        fmt::layer().compact().with_target(true)
    };
    tracing_subscriber::registry().with(filter).with(fmt_layer).init();
}
```

#### 2. Prometheus metrics
**File**: `rust/crates/thttpd-migrate/src/metrics.rs`
**Changes**: NEW — declare metrics and expose them on the configured metrics listener (`127.0.0.1:9100/metrics` by default). Keep metrics off the data-plane listener so `/metrics` cannot collide with proxied legacy content. Note: `PrometheusBuilder::with_http_listener` serves `/metrics` at a fixed path; the `metrics.path` config value is documented but not yet honored — supporting a custom path requires building a manual `axum`/`hyper` route around the handle, which is deferred.

```rust
use metrics::{counter, histogram, describe_counter, describe_histogram};
use metrics_exporter_prometheus::PrometheusBuilder;

// `path` is currently advisory: with_http_listener serves /metrics.
// Kept in the signature so a custom-route implementation can honor it later.
pub fn install(listen: std::net::SocketAddr, _path: &str) -> anyhow::Result<()> {
    PrometheusBuilder::new()
        .with_http_listener(listen)
        .install()?;
    describe_counter!("thttpd_migrate_requests_total", "Total proxied requests");
    describe_counter!("thttpd_migrate_5xx_responses_total", "Total 5xx responses from backends");
    describe_counter!("thttpd_migrate_shadow_divergences_total", "Total shadow divergences");
    describe_histogram!("thttpd_migrate_request_duration_seconds", "End-to-end request duration");
    Ok(())
}
```

#### 3. Instrument the forwarder
**File**: `rust/crates/thttpd-migrate/src/forwarder.rs`
**Changes**: MODIFY — record counters and histograms around every forward.

```rust
let started = Instant::now();
let result = forward_inner(req, backend, client).await;
let elapsed = started.elapsed().as_secs_f64();
counter!("thttpd_migrate_requests_total",
    "backend" => backend.name.clone()).increment(1);
histogram!("thttpd_migrate_request_duration_seconds",
    "backend" => backend.name.clone(),
    "status_class" => status_class(result.as_ref().ok()),
).record(elapsed);
if let Ok(ref r) = result {
    if r.status().is_server_error() {
        counter!("thttpd_migrate_5xx_responses_total", "backend" => backend.name.clone()).increment(1);
    }
}
```

#### 4. Request-ID propagation
**File**: `rust/crates/thttpd-migrate/src/server.rs`
**Changes**: MODIFY — accept inbound `X-Request-Id` or generate one, propagate to backends as `X-Request-Id`, include in every log line, return in response.

#### 5. Wire real tracing into `start()`
**File**: `rust/crates/thttpd-migrate/src/lib.rs`
**Changes**: MODIFY — replace the Phase 2 `init_tracing` stub with a call to the real subscriber. Without this edit the stub stays a no-op and tracing never initializes.

```rust
// Replace the Phase 2 stub body of init_tracing with:
fn init_tracing(log_level: &str) {
    let json = std::env::var("THTTPD_MIGRATE_LOG_FORMAT")
        .map(|v| v == "json")
        .unwrap_or(false);
    tracing_setup::init(log_level, json);
}
```

The `start()` function already calls `init_tracing(&_log_level)`, so no change to the call site is needed; only the stub body changes.

### Success Criteria:

#### Automated Verification:
- [x] `tracing_setup::tests::json_format_emits_valid_json` (one log line parses as JSON)
- [x] `metrics::tests::requests_total_increments` (fire one request, scrape configured `/metrics`, assert counter == 1)
- [x] `metrics::tests::duration_histogram_records_observation` (one request → at least one bucket)
- [x] Every log line in a test run includes the `request_id` field

#### Manual Verification:
- [ ] `curl http://localhost:9100/metrics` returns Prometheus exposition format
- [ ] `curl -H 'X-Request-Id: my-test' ...` → response carries `X-Request-Id: my-test`; backends receive the same header
- [ ] `thttpd_migrate_request_duration_seconds_bucket{backend="c-thttpd",le="0.01"}` is non-zero
- [ ] `cargo run` with `RUST_LOG=thttpd_migrate=debug` shows structured debug lines; with `THTTPD_MIGRATE_LOG_FORMAT=json` they're valid JSON

---

## Phase 7: Graceful drain + rollback

### Overview
Two operator actions that make the proxy safe to deploy:
- **Drain**: stop accepting new connections, finish in-flight (with timeout), exit
- **Rollback**: change the routing decision so 100% of traffic goes to the named backend, no process restart

Drain is for planned cutover; rollback is for emergencies. Both must complete in under 30 seconds with zero failed requests.

### Changes Required:

#### 1. Shared state via `arc-swap`
**File**: `rust/crates/thttpd-migrate/src/state.rs`
**Changes**: NEW — live config behind `arc-swap` for hot weight updates.

```rust
use arc_swap::ArcSwap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use crate::config::ProxyConfig;

pub struct LiveState {
    pub config: Arc<ArcSwap<ProxyConfig>>,
    pub draining: Arc<AtomicBool>,
}

impl LiveState {
    pub fn new(cfg: ProxyConfig) -> Self {
        Self {
            config: Arc::new(ArcSwap::from_pointee(cfg)),
            draining: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn start_drain(&self) { self.draining.store(true, Ordering::SeqCst); }
    pub fn is_draining(&self) -> bool { self.draining.load(Ordering::SeqCst) }
}

/// Weight propagation: when a weight update arrives via the control socket,
/// update the ArcSwap AND call `pool.update_weights()` which iterates
/// the pool's `Arc<Backend>` entries and swaps each backend's config.weight
/// to the new value. The router reads weight from the pool, not from ProxyConfig,
/// so both must stay in sync. Alternatively, have the router read weights
/// directly from `LiveState::config.load().backends[name].weight`.
```

#### 2. Server respects drain
**File**: `rust/crates/thttpd-migrate/src/server.rs`
**Changes**: MODIFY — accept loop checks `state.is_draining()`; if true, break and let in-flight finish. The `JoinSet` of in-flight request handles was introduced in Phase 3's server loop; drain awaits it here.

```rust
loop {
    if state.is_draining() { break; }
    tokio::select! {
        accept = listener.accept() => { /* ... */ }
        _ = shutdown_signal() => { state.start_drain(); }
    }
}
// JoinSet was populated in Phase 3's accept loop.
while let Some(_) = joinset.join_next().await {}
```

#### 3. CLI/control plane: set-weight, drain, rollback
**Files**: `rust/crates/thttpd-migrate/src/lib.rs`, `rust/crates/thttpd-migrate/src/control.rs`, `docs/CONTROL_PROTOCOL.md`
**Changes**: MODIFY/NEW — wire the subcommands. They communicate with the running proxy via a Unix domain socket at `config.control_socket` (default `/var/run/thttpd-migrate/control.sock`). The command line also accepts global `--control-socket` for tests and non-root demos.

```rust
pub fn set_weight(control_socket: PathBuf, pairs: Vec<String>) -> anyhow::Result<()> {
    // parse "backend=weight" pairs
    // connect to control socket
    // send JSON RPC {"command":"set_weight","weights":{...}}
}

pub fn rollback(control_socket: PathBuf, to: &str) -> anyhow::Result<()> {
    // query configured backends or send a semantic rollback command; do not rely on u32::MAX weights
    // send JSON RPC {"command":"rollback","to":to}
}

pub async fn drain(control_socket: PathBuf, timeout_secs: u64) -> anyhow::Result<()> {
    // connect to control socket
    // send JSON RPC {"command":"drain","timeout_secs":timeout_secs}
    // wait for ack
}
```

Create the control protocol spec in this phase, not Phase 9, because tests and external tooling need a contract before the protocol is implemented. Phase 9 links and explains the spec in operator-facing docs.

**Implementation note:** The bodies above are intentionally pseudocode — the real implementation is the largest single lift in Phase 7. Each function must: connect to the Unix domain socket at `control_socket`, serialize the JSON RPC, send it length-prefixed, read the ack/response, and deserialize. Define a shared `ControlRequest` / `ControlResponse` enum with `serde` in `control.rs` and use it on both the client side (these functions) and the server side (the socket listener added in the same phase). The Phase 7 automated criteria (`control::tests::set_weight_updates_live_state`) require this to be working code, not stubs.

#### 4. State file
**File**: `rust/crates/thttpd-migrate/src/state.rs`
**Changes**: MODIFY — atomically write `state.json` every 5s with backends, weights, divergence count, uptime. `thttpd-migrate status` reads this file (no need to query the running process; the file is the contract).

### Success Criteria:

#### Automated Verification:
- [x] `state::tests::weight_update_visible_to_router` (update via arc-swap, router sees new weights)
- [x] `state::tests::drain_flag_propagates_within_100ms`
- [x] `state::tests::state_file_written_atomically` (read mid-write doesn't see partial file)
- [x] `control::tests::set_weight_updates_live_state` using a temp `control.sock`
- [x] `control::tests::rollback_is_semantic_not_u32_max_weight` (all other backend weights become 0, target becomes 100)
- [x] End-to-end: start proxy, fire sustained requests for 5s, send DRAIN, in-flight requests complete; new connections receive `503` or connection refusal after listener shutdown, but no in-flight request is reset

#### Manual Verification:
- [ ] Start proxy with C and Rust; send `thttpd-migrate --control-socket <sock> set-weight rust-thttpd=100 c-thttpd=0`; within 1s, all traffic is on Rust
- [ ] Send `thttpd-migrate --control-socket <sock> rollback --to c-thttpd`; within 1s, all traffic is back on C
- [ ] Send `thttpd-migrate --control-socket <sock> drain --timeout 30`; existing requests finish, new connections fail; process exits within 30s
- [ ] `thttpd-migrate status` mid-run shows live counts; counts are not stale

---

## Phase 8: Integration tests

### Overview
End-to-end tests that spin up real C and Rust thttpd binaries on ephemeral ports, point the proxy at them, and exercise every routing mode. These tests live in `harness/tests/test_proxy.py` alongside the existing differential suite — they share `conftest.py` fixtures.

### Changes Required:

#### 1. Python integration tests
**File**: `harness/tests/test_proxy.py`
**Changes**: NEW — 30 test cases across 5 categories.

```python
# Categories (mirroring differential suite shape):
#   test_active_routing.py    — weight ratios, exclusion, sticky
#   test_shadow_mode.py       — divergences logged, user unaffected
#   test_health.py            — backend death, recovery, timeout
#   test_circuit_breaker.py   — trip, half-open, recovery
#   test_rollback.py          — promote, rollback, drain timing
```

Tests spin up the proxy via `subprocess.Popen` with a generated TOML config, drive it with the existing `http_request()` raw-socket helper from `harness/conftest.py:17-33` (or add `requests>=2,<3` to `requirements-dev.txt`), and assert on both proxy and backend state.

The TOML config is generated by a `write_proxy_config` helper (defined in `test_proxy.py` or `conftest.py`) that renders the config template below:

```python
def write_proxy_config(tmp_path, listen, metrics, control_socket, state_path, backends, weights):
    c_addr = backends["c_addr"]
    rust_addr = backends["rust_addr"]
    c_w, rust_w = weights["c-thttpd"], weights["rust-thttpd"]
    toml = f'''\
listen = "{listen}"
log_level = "info"
state_path = "{state_path}"
control_socket = "{control_socket}"

[metrics]
listen = "{metrics}"
path = "/metrics"

[shadow]
max_body_bytes = 1048576

[backends.c-thttpd]
address = "{c_addr}"
weight = {c_w}
health_path = "/"

[backends.rust-thttpd]
address = "{rust_addr}"
weight = {rust_w}
health_path = "/"

[routing]
mode = "active-active"
primary_backend = "c-thttpd"
shadow_backend = "rust-thttpd"
exclude_paths = ["/metrics"]
'''
    cfg_path = tmp_path / "thttpd-migrate.toml"
    cfg_path.write_text(toml)
    return cfg_path
```

#### 2. Fixture: dual backend
**File**: `harness/conftest.py`
**Changes**: MODIFY — add a small `wait_for_port(port, timeout=5.0)` helper plus `dual_thttpd_backends` fixture by reusing existing patterns from `find_free_port()` and `dual_server_process` (`harness/conftest.py:619-650`). Do not reference non-existent helpers such as `allocated_port` or `start_thttpd_binary`.

```python
def wait_for_port(port, timeout=5.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                return
        except OSError:
            time.sleep(0.05)
    raise RuntimeError(f"port {port} did not open")

@pytest.fixture
def dual_thttpd_backends(c_binary, rust_binary, www_root):
    c_port = find_free_port()
    rust_port = find_free_port()
    c_proc = subprocess.Popen([c_binary, "-p", str(c_port), "-D", "-d", str(www_root), "-c", "**cgi-bin**"],
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    rust_proc = subprocess.Popen([rust_binary, "-p", str(rust_port), "-D", "-d", str(www_root), "-c", "**cgi-bin**"],
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    wait_for_port(c_port); wait_for_port(rust_port)
    yield {"c_proc": c_proc, "rust_proc": rust_proc, "c_addr": f"127.0.0.1:{c_port}", "rust_addr": f"127.0.0.1:{rust_port}"}
    for proc in (c_proc, rust_proc):
        proc.terminate(); proc.wait(timeout=5)
```

#### 3. Fixture: proxy
**File**: `harness/conftest.py`
**Changes**: MODIFY — add `proxy` fixture that writes a TOML config under `tmp_path`, spawns `rust/target/release/thttpd-migrate start --config <cfg>`, yields the proxy port/control socket/state path, and tears down. Use temp paths for `state_path` and `control_socket`; `/var/run` is not writable in CI.

```python
@pytest.fixture
def proxy(dual_thttpd_backends, tmp_path):
    port = find_free_port()
    metrics_port = find_free_port()
    control_socket = tmp_path / "control.sock"
    state_path = tmp_path / "state.json"
    cfg_path = write_proxy_config(
        tmp_path,
        listen=f"127.0.0.1:{port}",
        metrics=f"127.0.0.1:{metrics_port}",
        control_socket=control_socket,
        state_path=state_path,
        backends=dual_thttpd_backends,
        weights={"c-thttpd": 95, "rust-thttpd": 5},
    )
    proc = subprocess.Popen(["rust/target/release/thttpd-migrate", "start", "--config", str(cfg_path)],
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    wait_for_port(port)
    yield {"addr": f"127.0.0.1:{port}", "metrics": f"127.0.0.1:{metrics_port}",
           "control_socket": control_socket, "state_path": state_path, "proc": proc, "config_path": cfg_path}
    proc.terminate(); proc.wait(timeout=10)
```

#### 4. Makefile integration
**File**: `Makefile`
**Changes**: MODIFY — add a `proxy` target and include it in `integration` after `build legacy`.

```make
proxy: build legacy
	$(PYTEST) harness/tests/test_proxy.py -q --timeout=60 --timeout-method=thread

integration: harness differential proxy
```

### Success Criteria:

#### Automated Verification:
- [x] `pytest harness/tests/test_proxy.py` — 30/30 pass
- [x] Each test runs in < 60s (slow tests use `--timeout=60`; pure unit-level proxy behavior stays in Rust tests)
- [x] `make integration` passes: 80 existing non-differential harness tests + 105 differential tests + 30 proxy tests = 215 harness tests total

#### Manual Verification:
- [ ] Locally, `pytest -v harness/tests/test_proxy.py::test_rollback_under_load` reproduces a 100 req/s load, sends `set-weight`, verifies all 100 req/s shift to Rust within 1s with no failed requests
- [ ] `pytest -v harness/tests/test_proxy.py::test_drain_during_burst` shows 0 connection-reset errors during graceful drain
- [ ] Proxy p99 latency at 1k req/s (via histogram scrape from `/metrics`): p99 request_duration through proxy minus p99 direct-to-backend is <1ms

---

## Phase 9: Documentation + runbooks

### Overview
The proxy is only useful if operators can deploy and recover it. Three documents, each ~1 page, plus the example config (already in Phase 2).

### Changes Required:

#### 1. User guide
**File**: `docs/STRANGLER_FIG.md`
**Changes**: NEW.

Contents:
- What is the strangler fig pattern, and why this proxy
- Architecture diagram (reproduce the ASCII from this plan's Overview)
- Quick start (install, configure, start, set-weight, rollback)
- Routing modes: active-active vs shadow vs canary — when to use each
- Health and circuit breaker — what they do, what they don't
- Observability — what metrics to alert on
- Common failure modes and what to do
- See also: `ROLLBACK.md`, `CONTROL_PROTOCOL.md`, `MIGRATION_PLAYBOOK.md`

#### 2. Rollback runbook
**File**: `docs/ROLLBACK.md`
**Changes**: NEW.

```markdown
# Rollback Runbook — thttpd-migrate

## TL;DR

```bash
thttpd-migrate --control-socket /var/run/thttpd-migrate/control.sock rollback --to c-thttpd
```

That's it. All traffic shifts to the named backend within 1 second; in-flight requests continue to completion; no requests are lost.

## When to use this

Any of:
- Error rate on Rust backend exceeds 1%
- p99 latency on Rust backend exceeds baseline + 50%
- A specific endpoint returns 5xx
- An operator is uncertain about the current state

**You are never wrong to roll back.** Roll forward again once you understand the issue.

## Step-by-step (1-minute procedure)

1. **Confirm the situation** (15s): `thttpd-migrate status`. Read the `circuit` field and the divergence count.
2. **Roll back** (1s): `thttpd-migrate --control-socket /var/run/thttpd-migrate/control.sock rollback --to c-thttpd`. Look for the log line `rollback complete`.
3. **Verify** (15s): `curl -H 'X-Request-Id: rollback-test' http://proxy:8080/` — check that the response comes from C (e.g. `Server: thttpd/2.27.0`).
4. **Capture evidence** (30s): `thttpd-migrate status --json > /var/log/thttpd-migrate/rollback-$(date +%s).json`. Save the proxy logs.
5. **Postmortem** (later): open a ticket. The evidence file is the audit trail.

## What rollback does NOT do

- It does not stop the Rust backend — it stops sending it traffic. The Rust process keeps running so you can attach a debugger.
- It does not flush in-flight requests — they complete normally.
- It does not require a config file edit — it's a one-command operator action.

## What to do if rollback itself fails

| Symptom | Action |
|---|---|
| `thttpd-migrate` command not found | SSH to proxy host; use absolute path `/usr/local/bin/thttpd-migrate` |
| "no backend named X" | Check `/etc/thttpd-migrate.toml`; the `--to` value must match a `[backends.*]` name |
| `state.json` is stale | Direct traffic at the TCP level: point DNS / load balancer at C's port directly. Bypass the proxy. |
| Proxy is hung | `kill -TERM $(pidof thttpd-migrate)`; if it doesn't exit in 10s, `kill -9`. Then point DNS at C. |
```

#### 3. Migration playbook
**File**: `docs/MIGRATION_PLAYBOOK.md`
**Changes**: NEW — week-by-week playbook: week 0 (setup), week 1 (shadow, no traffic shift), week 2 (5% canary), week 3 (50%), week 4 (100%), week 5 (decommission C).

#### 4. Control protocol spec
**File**: `docs/CONTROL_PROTOCOL.md`
**Changes**: MODIFY — Phase 7 creates the protocol contract; Phase 9 adds examples, operator notes, compatibility guarantees, and links from the user guide/runbook. The spec covers JSON RPC messages for `set_weight`, `rollback`, `drain`, error responses, and versioning.

#### 5. Section in top-level README
**File**: `README.md`
**Changes**: MODIFY — add a "Migration Tools" section near the bottom, linking to the proxy docs.

### Success Criteria:

#### Automated Verification:
- [x] Internal doc links resolve with a repository-local checker (for example, a small Python script that validates relative Markdown links; do not introduce a Node-only `markdown-link-check` dependency unless it is added to project tooling)
- [x] ADR-0002 (async runtime split) exists at `docs/ADR-0002-async-runtime-split.md` and links to this plan
- [x] Shell snippets in `ROLLBACK.md` and `STRANGLER_FIG.md` are extracted and syntax-checked with `bash -n` where applicable

#### Manual Verification:
- [ ] A new operator (someone not familiar with the project) can read `STRANGLER_FIG.md` and start the proxy in 10 minutes without help
- [ ] A new operator can read `ROLLBACK.md` and execute the rollback procedure in under 60 seconds during a tabletop exercise
- [ ] `MIGRATION_PLAYBOOK.md` walks through the full 6-week migration on a fresh cluster

---

## Testing Strategy

### Automated
- **Unit tests** (`cargo test --manifest-path rust/Cargo.toml -p thttpd-migrate`): every module has tests for its public API
- **Integration tests** (`harness/tests/test_proxy.py`): 30 end-to-end tests covering every routing mode, health, circuit breaker, drain, rollback
- **Differential regression** (`harness/tests/test_differential.py`): unchanged — proxy must not affect the existing 105 differential tests when not in the loop

### Manual
- **6-week migration playbook** run against a real C and Rust thttpd pair
- **Load test**: 1k req/s for 1h, monitor metrics, verify no proxy-induced 5xx
- **Chaos test**: kill C and Rust alternately every 60s for 10 minutes; verify clients never see a connection error

## Performance Considerations

- **Connection pool reuse**: `hyper-util` client with `pool_idle_timeout = 30s` keeps keep-alive connections warm to backends. Cold-start cost is one connect per backend on first request.
- **Shadow async dispatch**: shadow mode buffers request and primary/shadow responses up to `shadow.max_body_bytes` (default 1MiB) for diff. The user's response is never blocked on the shadow backend, but shadow mode is intentionally not the low-latency streaming path.
- **Health check overhead**: one probe per backend per `interval_ms` (default 1000ms). At 100 backends, this is 100 req/s of overhead — acceptable but should be tuned for large pools.
- **Metrics scrape**: Prometheus pull at 15s interval adds negligible load. Counter increments are wait-free atomics.

Target: <1ms p99 overhead vs. talking to the backend directly, at 1k req/s. Verified by comparing `thttpd_migrate_request_duration_seconds` histogram p99 against direct-to-backend p99 from a separate histogram scrape (add a Phase 8 manual verification bullet).

## Migration Notes

- **No data migration** — the proxy is stateless except for the live `state.json` and `control.sock`
- **Backwards compatibility** — thttpd.conf and thttpd-rs configs are unchanged; the proxy is a new component
- **Rollback strategy** — `thttpd-migrate rollback` shifts traffic in 1s; bypassing the proxy entirely (point DNS at C directly) is the fallback

## References

- Martin Fowler, *StranglerFigApplication* (2004) — the pattern this implements
- Envoy docs: traffic shifting, circuit breaking, outlier detection — design inspiration for the circuit breaker
- Existing differential test infrastructure: `harness/diff_engine.py:32-325` (logic ported to `diff.rs` in Phase 4)
- Existing thttpd-rs event loop: `rust/crates/thttpd-core/src/eventloop.rs` — pattern reference for the proxy's request lifecycle (Phase 3)

## Follow-up — 2026-06-13T22:53:50-0300 implementation-readiness review

Revised before implementation to address readiness gaps discovered against the current repository:

- Phase 1 no longer declares missing future modules or requires Phase 2 config to start the skeleton server.
- Config examples now use parseable `host:port` addresses, default health path `/`, explicit metrics/control/state settings, and shadow body caps.
- Routing now gives shadow mode an explicit `primary_backend` so shadow traffic can never affect users.
- Hyper 1 snippets now use concrete/boxed body types instead of non-compiling `Response<String>` and empty-body client aliases for proxied requests.
- Shadow diffing now documents request/response buffering, body caps, and the fact that user responses are not blocked on the shadow backend but may be buffered in verification mode.
- Phase 7 owns the control protocol contract because `set-weight`, `rollback`, and `drain` tests need it before docs polish.
- Phase 8 now reuses existing harness patterns and adds `make proxy` / `make integration` coverage with corrected test counts.

## Follow-up — 2026-06-13T23:55:00-0300 independent model review (blocker + concern fixes)

Third pass by a fresh model found compile blockers and concerns the prior reviews missed:

**Blockers fixed (would not compile):**
- B1: `dispatch_shadow` captured `&RoutingDecision` inside `tokio::spawn` (`'static` violation). Fixed: `RoutingDecision` now derives `Clone`; `dispatch_shadow` takes it by value and moves the owned copy into the spawn.
- B2: `diff_responses` call had a `bool` (`shadow_truncated`) landing in the `shadow_status: u16` slot. Fixed: reordered args to match the signature.
- B3: `update_health` signature expected `Result` but caller passed `bool` — diverged edits. Fixed: `update_health` takes `bool` again; the 5xx status check moved to the call site (`Ok(Ok(resp)) => resp.status().is_success()`).
- B4: `handle` passed `Request<Incoming>` to `forwarder::forward` which wants `Request<ProxyBody>`. Fixed: convert the request body via `into_parts()` + `map_err(...).boxed()` before forwarding.

**Concerns fixed:**
- C1: Phase 2 `start()` had contradictory `addr == listen || listen == default` logic. Fixed: config.listen is the sole authority; `--listen` ignored after Phase 1.
- C2: Phase 2 step numbering skipped 4 (1,2,3,5,6). Fixed: renumbered to 1-5.
- C3: `router::decide<B>(req: &Request<B>, …)` never read `req`. Fixed: renamed to `_req` with a doc comment.
- C4: Phase 6 never replaced the `init_tracing` stub — tracing stayed a no-op. Fixed: added Phase 6 step 5 wiring `tracing_setup::init` into the stub body.
- C5: `metrics::install(listen, path)` took `path` but never used it. Fixed: documented as advisory (with_http_listener serves `/metrics`), param renamed `_path`.
- C6: `write_proxy_config` was called in the Phase 8 fixture but never defined. Fixed: added the full helper with TOML template.
- C7: Response-body mapping `.map_err(|e| e).boxed()` assumed `Incoming::Error == hyper::Error`. Fixed: added implementation note to verify the associated Error type at impl time.
- C8: New tokio/hyper/metrics transitive crates may trip `cargo deny` license check. Fixed: added Phase 1 success criterion to run `make security` and resolve variants.
- C9: Phase 7 control functions were comment-only pseudocode despite automated criteria assuming working code. Fixed: added implementation note that these are the real lift — define `ControlRequest`/`ControlResponse` serde enums, real socket connect/send/recv.
