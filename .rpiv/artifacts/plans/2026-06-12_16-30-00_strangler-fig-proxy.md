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
last_updated: 2026-06-12T16:30:00-0300
last_updated_by: Burke T
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

### Architectural decision: async runtime

The existing `thttpd-rs` server deliberately uses `mio` directly with a manual event loop to match thttpd's C architecture. The proxy is a different kind of system — it manages many concurrent connections to multiple backends, and re-implementing that on `mio` would be busywork that hides the actual proxy logic.

**Decision:** the proxy uses `tokio` + `hyper`. Rationale:
- Proxy logic is naturally concurrent (many connections × multiple backends)
- The proxy is migration tooling, not part of the server's hot path
- Production proxies (Envoy, nginx, HAProxy) are async
- `tokio`'s task model maps cleanly to per-request proxying

**Trade-off:** introduces a new runtime to the project. ADR-0002 will record this.

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
listen = ":8080"
log_level = "info"

[backends.c-thttpd]
address = "127.0.0.1:8081"
weight = 95
health_path = "/__healthz"

[backends.rust-thttpd]
address = "127.0.0.1:8082"
weight = 5
health_path = "/__healthz"

[routing]
mode = "active-active"  # | "shadow" | "canary"
shadow_backend = "rust-thttpd"
exclude_paths = ["/internal/*", "/__healthz"]

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
thttpd-migrate set-weight rust-thttpd=100 c-thttpd=0

# Roll back to C in 30 seconds (one command, no traffic loss)
thttpd-migrate rollback --to c-thttpd

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
- `prometheus://localhost:8080/__metrics` shows `thttpd_migrate_requests_total{backend="..."}` incrementing

## What We're NOT Doing

- **L7 features** beyond routing (no body rewriting, header injection beyond pass-through)
- **TLS termination** — terminate TLS at a separate proxy (envoy/nginx/caddy) in front
- **Service discovery** — backends are configured statically; DNS-based discovery is deferred
- **Multi-region / global load balancing** — single-region only
- **Admin UI** — CLI and Prometheus metrics only; no web UI in this phase
- **Hot config reload** — SIGHUP re-reads config; weight changes use the dedicated CLI command
- **HTTP/2 to backends** — HTTP/1.1 only, matching thttpd-rs and thttpd capabilities
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

[dev-dependencies]
wiremock = "0.6"
tempfile = { workspace = true }
```

#### 3. CLI entry
**File**: `rust/crates/thttpd-migrate/src/main.rs`
**Changes**: NEW — clap subcommand dispatch with `tokio::main`.

```rust
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "thttpd-migrate",
    version,
    about = "Strangler-fig proxy for thttpd → thttpd-rs migration"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start the proxy
    Start {
        #[arg(long, default_value = "/etc/thttpd-migrate.toml")]
        config: PathBuf,
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
        Cmd::Start { config, log_level } => thttpd_migrate::start(config, log_level).await,
        Cmd::Status { state } => thttpd_migrate::status(state),
        Cmd::SetWeight { pairs } => thttpd_migrate::set_weight(pairs),
        Cmd::Drain { timeout_secs } => thttpd_migrate::drain(timeout_secs).await,
        Cmd::Rollback { to } => thttpd_migrate::rollback(&to),
    }
}
```

#### 4. Lib root with module stubs
**File**: `rust/crates/thttpd-migrate/src/lib.rs`
**Changes**: NEW — module declarations, one per future phase. Stubs return `unimplemented!()` so the build succeeds.

```rust
//! Strangler-fig migration proxy for thttpd → thttpd-rs.
//!
//! See `.rpiv/artifacts/plans/2026-06-12_16-30-00_strangler-fig-proxy.md`.

pub mod config;
pub mod router;
pub mod backend;
pub mod forwarder;
pub mod shadow;
pub mod health;
pub mod circuit;
pub mod metrics;
pub mod state;
pub mod server;
pub mod drain;

use std::path::PathBuf;

pub async fn start(_config: PathBuf, _log_level: String) -> anyhow::Result<()> {
    unimplemented!("Phase 1 stub: skeleton server only")
}
pub fn status(_state: PathBuf) -> anyhow::Result<()> { unimplemented!() }
pub fn set_weight(_pairs: Vec<String>) -> anyhow::Result<()> { unimplemented!() }
pub async fn drain(_timeout_secs: u64) -> anyhow::Result<()> { unimplemented!() }
pub fn rollback(_to: &str) -> anyhow::Result<()> { unimplemented!() }
```

#### 5. Hello-world hyper server
**File**: `rust/crates/thttpd-migrate/src/server.rs`
**Changes**: NEW — minimal hyper server returning 200 OK on every request. Real routing comes in Phase 3.

```rust
use hyper::{Request, Response, body::Incoming, service::service_fn};
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tracing::info;

pub async fn run_skeleton(listen: SocketAddr) -> anyhow::Result<()> {
    let listener = TcpListener::bind(listen).await?;
    info!(addr = %listen, "thttpd-migrate skeleton listening");
    loop {
        let (stream, peer) = listener.accept().await?;
        let io = TokioIo::new(stream);
        tokio::spawn(async move {
            let svc = service_fn(|_req: Request<Incoming>| async {
                Ok::<_, Infallible>(Response::new("ok".to_string()))
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
- [ ] `cargo build --manifest-path rust/Cargo.toml --workspace` succeeds
- [ ] `cargo clippy --manifest-path rust/Cargo.toml -p thttpd-migrate --all-targets -- -D warnings` passes
- [ ] `./target/debug/thttpd-migrate --help` lists `start`, `status`, `set-weight`, `drain`, `rollback`
- [ ] `./target/debug/thttpd-migrate start --config /nonexistent.toml` exits non-zero with a clear error
- [ ] `cargo test -p thttpd-migrate` passes (placeholder test in `lib.rs`)

#### Manual Verification:
- [ ] `./target/debug/thttpd-migrate start --config <some.toml>` binds to a port; `curl localhost:<port>/` returns `200 OK` with body `ok`
- [ ] Ctrl-C produces a clean shutdown (log line "shutdown complete", no orphan tasks)
- [ ] Log output goes to stderr in JSON when `RUST_LOG=info THTTPD_MIGRATE_LOG_FORMAT=json`

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

#[derive(Debug, Deserialize, Clone)]
pub struct BackendConfig {
    pub address: String,
    #[serde(default = "default_weight")]
    pub weight: u32,
    #[serde(default = "default_health_path")]
    pub health_path: String,
}
fn default_weight() -> u32 { 1 }
fn default_health_path() -> String { "/__healthz".into() }

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RoutingMode { #[default] ActiveActive, Shadow, Canary }

#[derive(Debug, Deserialize, Clone)]
pub struct RoutingConfig {
    #[serde(default)]
    pub mode: RoutingMode,
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

impl Default for RoutingConfig { /* … */ }
impl Default for HealthConfig { /* … */ }
impl Default for CircuitConfig { /* … */ }

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
        anyhow::ensure!(
            cfg.routing.shadow_backend.is_some(),
            "routing.mode = shadow requires routing.shadow_backend"
        );
    }
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

listen = ":8080"
log_level = "info"

[backends.c-thttpd]
address = "127.0.0.1:8081"
weight = 95                # 95% of traffic in active-active
health_path = "/__healthz"

[backends.rust-thttpd]
address = "127.0.0.1:8082"
weight = 5                 # 5% canary
health_path = "/__healthz"

[routing]
mode = "active-active"     # | "shadow" | "canary"
shadow_backend = "rust-thttpd"
exclude_paths = ["/internal/*"]   # never proxied, returned 404

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

#### 4. Wire config into `start()`
**File**: `rust/crates/thttpd-migrate/src/lib.rs`
**Changes**: MODIFY — `start()` now loads config, builds the pool, calls `server::run_skeleton`. Other commands still stubs.

```rust
pub async fn start(config: std::path::PathBuf, log_level: String) -> anyhow::Result<()> {
    init_tracing(&log_level);
    let cfg = config::load(&config)?;
    let _pool = backend::BackendPool::from_config(&cfg.backends);
    let addr: std::net::SocketAddr = cfg.listen.parse()?;
    server::run_skeleton(addr).await
}
```

### Success Criteria:

#### Automated Verification:
- [ ] `cargo test -p thttpd-migrate config::tests::loads_example_config` passes
- [ ] `config::tests::rejects_empty_backends` passes
- [ ] `config::tests::rejects_zero_total_weight` passes
- [ ] `config::tests::shadow_requires_shadow_backend` passes
- [ ] Loading `config/thttpd-migrate.example.toml` parses without error
- [ ] Invalid config (e.g. `weight = 0` for all backends) returns a clear error message naming the field

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
use crate::backend::{Backend, BackendPool, Health};
use hyper::Request;
use std::sync::Arc;
use std::time::Instant;
use tracing::debug;

pub struct RoutingDecision {
    pub backend: Arc<Backend>,
    pub shadow: Option<Arc<Backend>>,   // for shadow mode
    pub request_id: String,
    pub started_at: Instant,
}

pub fn decide<B>(
    req: &Request<B>,
    pool: &BackendPool,
    mode: crate::config::RoutingMode,
    shadow_backend_name: Option<&str>,
) -> Option<RoutingDecision> {
    let request_id = uuid::Uuid::new_v4().to_string();

    // Active/active or canary: weighted random among healthy backends
    let candidates: Vec<&Arc<Backend>> = pool.iter()
        .filter(|b| b.is_routable())
        .collect();

    anyhow::ensure!(!candidates.is_empty(), "no healthy backends");

    let total: u32 = candidates.iter().map(|b| b.config.weight).sum();
    let pick = rand::random::<u32>() % total;
    let mut acc = 0u32;
    let chosen = candidates.iter().find(|b| {
        acc += b.config.weight;
        pick < acc
    }).copied().cloned()?;

    let shadow = match mode {
        crate::config::RoutingMode::Shadow | crate::config::RoutingMode::ActiveActive => {
            shadow_backend_name.and_then(|n| pool.get(n)).filter(|b| b.name != chosen.name)
        }
        crate::config::RoutingMode::Canary => None,
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
use crate::backend::Backend;
use crate::router::RoutingDecision;
use hyper::body::Incoming;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use std::sync::Arc;
use std::time::Duration;

pub type ProxyClient = Client<HttpConnector, http_body_util::Empty<bytes::Bytes>>;

pub fn build_client() -> ProxyClient {
    let mut connector = HttpConnector::new();
    connector.set_connect_timeout(Some(Duration::from_secs(2)));
    Client::builder(hyper_util::rt::TokioExecutor::new())
        .pool_idle_timeout(Duration::from_secs(30))
        .build(connector)
}

pub async fn forward<B>(
    decision: &RoutingDecision,
    req: Request<B>,
    client: &ProxyClient,
) -> Result<Response<Incoming>, ForwardError>
where B: hyper::body::Body + 'static { /* … */ }
```

The forwarder constructs a new `Request` with the same method, URI, and headers, swaps the body, and sends it to `decision.backend.config.address`. The response body is streamed back without buffering.

#### 3. Server loop with routing
**File**: `rust/crates/thttpd-migrate/src/server.rs`
**Changes**: MODIFY — replace skeleton with real handler.

```rust
async fn handle<B>(
    req: Request<B>,
    pool: Arc<BackendPool>,
    mode: RoutingMode,
    shadow_backend: Option<String>,
    client: ProxyClient,
) -> Result<Response<Incoming>, Infallible>
where B: hyper::body::Body + 'static {
    // 1. Exclude paths
    if is_excluded(req.uri().path(), &excluded) {
        return Ok(not_found());
    }
    // 2. Decide
    let decision = match router::decide(&req, &pool, mode, shadow_backend.as_deref()) {
        Some(d) => d,
        None => return Ok(backend_unavailable()),
    };
    // 3. Forward
    match forwarder::forward(&decision, req, &client).await {
        Ok(resp) => Ok(resp),
        Err(e) => {
            tracing::error!(error = %e, backend = %decision.backend.name, "forward error");
            Ok(bad_gateway())
        }
    }
}
```

### Success Criteria:

#### Automated Verification:
- [ ] `router::tests::weighted_selection_respects_weights` (run 10k iterations; distribution within 5% of weight ratios)
- [ ] `router::tests::unhealthy_backends_excluded` (set backend to Unhealthy, verify never picked)
- [ ] `forwarder::tests::preserves_method_path_headers_body` against `wiremock`
- [ ] `forwarder::tests::streams_large_response` (1MB body) — `body.frame().next()` returns chunks, not a single buffer
- [ ] `cargo test -p thttpd-migrate` all pass

#### Manual Verification:
- [ ] Start two `nc -l` listeners on 8081 and 8082 (both respond with `Backend: c` or `Backend: rust`); proxy on 8080; 1000 curl requests → roughly the configured ratio lands on each
- [ ] Kill the rust backend mid-flight; subsequent requests all go to c
- [ ] `curl -X POST -d 'hello' localhost:8080/echo` — the request body reaches the backend (`nc` logs the body)

---

## Phase 4: Shadow mode + response diffing

### Overview
When shadow mode is enabled, every request is mirrored to the shadow backend asynchronously. The shadow response is captured and diffed against the primary response using the same comparison logic that `harness/diff_engine.py` implements. Divergences are logged, never propagated to the user.

### Changes Required:

#### 1. Port diff logic to Rust
**File**: `rust/crates/thttpd-migrate/src/diff.rs`
**Changes**: NEW — port `harness/diff_engine.py:1-180` (header normalization, response comparison). This avoids a Python subprocess on the hot path.

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
}

pub async fn diff_responses(
    primary: Response<Incoming>,
    shadow: Response<Incoming>,
    primary_body: Bytes,
    shadow_body: Bytes,
    ctx: &RequestContext,
) -> Vec<Divergence> { /* … */ }
```

The port preserves the normalizers from `diff_engine.py` (timestamp format-only, path temp-dir substitution, etc.) — these are deliberate engineering choices and we don't re-litigate them in the proxy.

#### 2. Shadow dispatcher
**File**: `rust/crates/thttpd-migrate/src/shadow.rs`
**Changes**: NEW — fire-and-forget shadow request via `tokio::spawn`, diff and log result.

```rust
pub fn dispatch_shadow<B>(
    decision: &RoutingDecision,
    req: Request<B>,
    primary_response: Response<Incoming>,
    primary_body: Bytes,
    client: ProxyClient,
) where B: hyper::body::Body + 'static + Send {
    let shadow = decision.shadow.clone().unwrap();
    let request_id = decision.request_id.clone();
    let path = req.uri().path().to_string();
    let method = req.method().to_string();
    tokio::spawn(async move {
        let shadow_req = rebuild_for_backend(req, &shadow);
        let result = forwarder::forward_raw(shadow_req, &client, &shadow).await;
        let divergences = match result {
            Ok((resp, body)) => diff::diff_responses(primary_response, resp, primary_body, body, &ctx).await,
            Err(e) => vec![Divergence {
                field: Field::ConnectionLifecycle,
                expected: "ok".into(),
                actual: format!("error: {e}"),
                path, method,
            }],
        };
        for d in divergences {
            tracing::warn!(
                request_id = %request_id,
                backend = %shadow.name,
                field = ?d.field,
                "shadow divergence"
            );
            metrics::counter!("thttpd_migrate_shadow_divergences_total",
                "backend" => shadow.name.clone(),
                "field" => format!("{:?}", d.field),
            ).increment(1);
        }
    });
}
```

#### 3. Wire shadow into server loop
**File**: `rust/crates/thttpd-migrate/src/server.rs`
**Changes**: MODIFY — after the primary response is built, call `shadow::dispatch_shadow` if `decision.shadow.is_some()`. Body must be fully buffered (or tee'd via `Body::wrap_stream`) for diffing.

### Success Criteria:

#### Automated Verification:
- [ ] `diff::tests::timestamp_headers_match_format_only` (proves the timestamp normalizer ported correctly)
- [ ] `diff::tests::status_mismatch_caught`, `headers_mismatch_caught`, `body_mismatch_caught`
- [ ] `diff::tests::temp_path_substitution` (e.g. `/tmp/thttpd_golden_xyz` vs `/tmp/pytest-abc` → no divergence)
- [ ] `shadow::tests::divergence_does_not_affect_user` (intentionally diverge the shadow backend; user response is unchanged)
- [ ] `diff::tests::match_known_differential_test_outputs` — replay 5 captured request/response pairs from `harness/golden/baseline.json` and confirm zero false-positive divergences

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
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;

pub fn spawn_checker(
    pool: Arc<BackendPool>,
    client: crate::forwarder::ProxyClient,
    cfg: HealthConfig,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let interval = cfg.interval();
        loop {
            for backend in pool.iter() {
                let url = format!("http://{}{}", backend.config.address, backend.config.health_path);
                let result = tokio::time::timeout(cfg.timeout(), client.get(url.parse().unwrap())).await;
                update_health(backend, &cfg, result.is_ok() && result.as_ref().unwrap().is_ok());
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

impl Breaker {
    pub fn record(&self, success: bool) {
        let mut w = self.window.lock();
        w.total += 1;
        if !success { w.errors += 1; }
        let rate = w.errors as f64 / w.total as f64;
        if w.total >= self.cfg.min_requests && rate > self.cfg.error_rate_threshold {
            self.trip();
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
- [ ] `health::tests::three_consecutive_failures_marks_unhealthy`
- [ ] `health::tests::two_consecutive_successes_marks_healthy` (after recovery)
- [ ] `health::tests::timeout_counts_as_failure`
- [ ] `circuit::tests::below_min_requests_does_not_trip`
- [ ] `circuit::tests::error_rate_above_threshold_trips`
- [ ] `circuit::tests::half_open_probe_recovers`

#### Manual Verification:
- [ ] Start C on 8081 (responding), Rust on 8082 (not running); proxy on 8080
- [ ] Within 5s, `thttpd-migrate status` reports rust-thttpd as `Unhealthy`
- [ ] All requests routed to C; zero 5xx from proxy
- [ ] Start Rust on 8082; within 5s `status` reports healthy again
- [ ] Kill C under load (50 req/s); proxy circuit trips within window; all traffic shifts to Rust; no error responses to clients

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
**Changes**: NEW — declare metrics, expose `/__metrics` endpoint.

```rust
use metrics::{counter, histogram, describe_counter, describe_histogram};
use metrics_exporter_prometheus::PrometheusBuilder;

pub fn install(listen: std::net::SocketAddr) -> anyhow::Result<()> {
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

### Success Criteria:

#### Automated Verification:
- [ ] `tracing_setup::tests::json_format_emits_valid_json` (one log line parses as JSON)
- [ ] `metrics::tests::requests_total_increments` (fire one request, scrape `/__metrics`, assert counter == 1)
- [ ] `metrics::tests::duration_histogram_records_observation` (one request → at least one bucket)
- [ ] Every log line in a test run includes the `request_id` field

#### Manual Verification:
- [ ] `curl http://localhost:9100/__metrics` returns Prometheus exposition format
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
use crate::config::ProxyConfig;

pub struct LiveState {
    pub config: Arc<ArcSwap<ProxyConfig>>,
    pub draining: Arc<AtomicBool>,
}

impl LiveState {
    pub fn new(cfg: ProxyConfig) -> Self {
        Self { config: Arc::new(ArcSwap::from_pointee(cfg)), draining: Arc::new(AtomicBool::new(false)) }
    }
    pub fn start_drain(&self) { self.draining.store(true, Ordering::SeqCst); }
    pub fn is_draining(&self) -> bool { self.draining.load(Ordering::SeqCst) }
}
```

#### 2. Server respects drain
**File**: `rust/crates/thttpd-migrate/src/server.rs`
**Changes**: MODIFY — accept loop checks `state.is_draining()`; if true, break and let in-flight finish.

```rust
loop {
    if state.is_draining() { break; }
    tokio::select! {
        accept = listener.accept() => { /* ... */ }
        _ = shutdown_signal() => { state.start_drain(); }
    }
}
// wait for in-flight tasks (track via JoinSet)
while let Some(_) = joinset.join_next().await {}
```

#### 3. CLI: set-weight, drain, rollback
**File**: `rust/crates/thttpd-migrate/src/lib.rs`
**Changes**: MODIFY — wire the subcommands. They communicate with the running proxy via a Unix domain socket at `/var/run/thttpd-migrate/control.sock` (or signal a pid file).

```rust
pub fn set_weight(pairs: Vec<String>) -> anyhow::Result<()> {
    // parse "backend=weight" pairs
    // connect to control socket
    // send "SET_WEIGHT <json>"
}

pub fn rollback(to: &str) -> anyhow::Result<()> {
    set_weight(vec![format!("{to}={}", u32::MAX)])
}

pub async fn drain(timeout_secs: u64) -> anyhow::Result<()> {
    // connect to control socket
    // send "DRAIN <timeout>"
    // wait for ack
}
```

The control protocol is a 5-line length-prefixed JSON RPC. A spec lives in `docs/CONTROL_PROTOCOL.md` (created in Phase 9).

#### 4. State file
**File**: `rust/crates/thttpd-migrate/src/state.rs`
**Changes**: MODIFY — atomically write `state.json` every 5s with backends, weights, divergence count, uptime. `thttpd-migrate status` reads this file (no need to query the running process; the file is the contract).

### Success Criteria:

#### Automated Verification:
- [ ] `state::tests::weight_update_visible_to_router` (update via arc-swap, router sees new weights)
- [ ] `state::tests::drain_flag_propagates_within_100ms`
- [ ] `state::tests::state_file_written_atomically` (read mid-write doesn't see partial file)
- [ ] End-to-end: start proxy, fire 1000 req/s for 5s, send DRAIN, all 5000 in-flight requests complete; new connections rejected with `503`

#### Manual Verification:
- [ ] Start proxy with C and Rust; send `thttpd-migrate set-weight rust-thttpd=100 c-thttpd=0`; within 1s, all traffic is on Rust
- [ ] Send `thttpd-migrate rollback --to c-thttpd`; within 1s, all traffic is back on C
- [ ] Send `thttpd-migrate drain --timeout 30`; existing requests finish, new connections fail; process exits within 30s
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

Tests spin up the proxy via `subprocess.Popen` with a generated TOML config, drive it with `requests` (not raw sockets — proxy is well-behaved HTTP), and assert on both proxy and backend state.

#### 2. Fixture: dual backend
**File**: `harness/conftest.py`
**Changes**: MODIFY — add `dual_thttpd_backends` fixture that starts both C and Rust on allocated ports, yields their addresses, tears them down.

```python
@pytest.fixture
def dual_thttpd_backends(allocated_port):
    c = start_thttpd_binary("legacy/src/thttpd", port=allocated_port(), www="harness/www")
    r = start_thttpd_binary("rust/target/release/thttpd", port=allocated_port(), www="harness/www")
    yield {"c": c, "rust": r, "c_addr": c.address, "rust_addr": r.address}
    c.stop(); r.stop()
```

#### 3. Fixture: proxy
**File**: `harness/conftest.py`
**Changes**: MODIFY — add `proxy` fixture that writes a TOML config, spawns `thttpd-migrate start`, yields the proxy port, tears down.

```python
@pytest.fixture
def proxy(dual_thttpd_backends, allocated_port):
    cfg = generate_proxy_config(backends=dual_thttpd_backends, listen=allocated_port(),
                                weights={"c-thttpd": 95, "rust-thttpd": 5})
    proc = subprocess.Popen(["thttpd-migrate", "start", "--config", cfg.path])
    wait_for_port(allocated_port.last)
    yield {"addr": f"127.0.0.1:{allocated_port.last}", "proc": proc, "config_path": cfg.path}
    proc.terminate(); proc.wait(timeout=10)
```

### Success Criteria:

#### Automated Verification:
- [ ] `pytest harness/tests/test_proxy.py` — 30/30 pass
- [ ] Each test runs in < 30s (slow tests use `--timeout=30` and degrade to wiremock)
- [ ] `pytest harness/tests/` (full suite) — 216 existing + 30 new = 246 total pass

#### Manual Verification:
- [ ] Locally, `pytest -v harness/tests/test_proxy.py::test_rollback_under_load` reproduces a 100 req/s load, sends `set-weight`, verifies all 100 req/s shift to Rust within 1s with no failed requests
- [ ] `pytest -v harness/tests/test_proxy.py::test_drain_during_burst` shows 0 connection-reset errors during graceful drain

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
thttpd-migrate rollback --to c-thttpd
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
2. **Roll back** (1s): `thttpd-migrate rollback --to c-thttpd`. Look for the log line `rollback complete`.
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
**Changes**: NEW — JSON RPC spec for the control socket (used by `set-weight`, `drain`, etc.). Allows other tooling (custom dashboards, CI, chaos engineering) to talk to the proxy.

#### 5. Section in top-level README
**File**: `README.md`
**Changes**: MODIFY — add a "Migration Tools" section near the bottom, linking to the proxy docs.

### Success Criteria:

#### Automated Verification:
- [ ] `markdown-link-check docs/STRANGLER_FIG.md docs/ROLLBACK.md docs/MIGRATION_PLAYBOOK.md docs/CONTROL_PROTOCOL.md` (no broken internal links)
- [ ] `bash -n` (or equivalent) on every shell snippet in `ROLLBACK.md`

#### Manual Verification:
- [ ] A new operator (someone not familiar with the project) can read `STRANGLER_FIG.md` and start the proxy in 10 minutes without help
- [ ] A new operator can read `ROLLBACK.md` and execute the rollback procedure in under 60 seconds during a tabletop exercise
- [ ] `MIGRATION_PLAYBOOK.md` walks through the full 6-week migration on a fresh cluster

---

## Testing Strategy

### Automated
- **Unit tests** (`cargo test -p thttpd-migrate`): every module has tests for its public API
- **Integration tests** (`harness/tests/test_proxy.py`): 30 end-to-end tests covering every routing mode, health, circuit breaker, drain, rollback
- **Differential regression** (`harness/tests/test_differential.py`): unchanged — proxy must not affect the existing 80 differential tests when not in the loop

### Manual
- **6-week migration playbook** run against a real C and Rust thttpd pair
- **Load test**: 1k req/s for 1h, monitor metrics, verify no proxy-induced 5xx
- **Chaos test**: kill C and Rust alternately every 60s for 10 minutes; verify clients never see a connection error

## Performance Considerations

- **Connection pool reuse**: `hyper-util` client with `pool_idle_timeout = 30s` keeps keep-alive connections warm to backends. Cold-start cost is one connect per backend on first request.
- **Shadow async dispatch**: shadow responses are buffered (up to 1MB; truncating larger with a log warning) for diff. The user's response is not blocked.
- **Health check overhead**: one probe per backend per `interval_ms` (default 1000ms). At 100 backends, this is 100 req/s of overhead — acceptable but should be tuned for large pools.
- **Metrics scrape**: Prometheus pull at 15s interval adds negligible load. Counter increments are wait-free atomics.

Target: <1ms p99 overhead vs. talking to the backend directly, at 1k req/s.

## Migration Notes

- **No data migration** — the proxy is stateless except for the live `state.json` and `control.sock`
- **Backwards compatibility** — thttpd.conf and thttpd-rs configs are unchanged; the proxy is a new component
- **Rollback strategy** — `thttpd-migrate rollback` shifts traffic in 1s; bypassing the proxy entirely (point DNS at C directly) is the fallback

## References

- Martin Fowler, *StranglerFigApplication* (2004) — the pattern this implements
- Envoy docs: traffic shifting, circuit breaking, outlier detection — design inspiration for the circuit breaker
- Existing differential test infrastructure: `harness/diff_engine.py:1-180` (logic ported to `diff.rs` in Phase 4)
- Existing thttpd-rs event loop: `rust/crates/thttpd-core/src/eventloop.rs` — pattern reference for the proxy's request lifecycle (Phase 3)
