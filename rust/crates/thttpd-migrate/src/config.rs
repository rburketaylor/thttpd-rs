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

fn default_log_level() -> String {
    "info".into()
}
fn default_state_path() -> String {
    "/var/run/thttpd-migrate/state.json".into()
}
fn default_control_socket() -> String {
    "/var/run/thttpd-migrate/control.sock".into()
}

#[derive(Debug, Deserialize, Clone)]
pub struct MetricsConfig {
    #[serde(default = "default_metrics_listen")]
    pub listen: String,
    #[serde(default = "default_metrics_path")]
    pub path: String,
}
fn default_metrics_listen() -> String {
    "127.0.0.1:9100".into()
}
fn default_metrics_path() -> String {
    "/metrics".into()
}

#[derive(Debug, Deserialize, Clone)]
pub struct ShadowConfig {
    #[serde(default = "default_shadow_max_body_bytes")]
    pub max_body_bytes: usize,
}
fn default_shadow_max_body_bytes() -> usize {
    1_048_576
}

#[derive(Debug, Deserialize, Clone)]
pub struct BackendConfig {
    pub address: String,
    #[serde(default = "default_weight")]
    pub weight: u32,
    #[serde(default = "default_health_path")]
    pub health_path: String,
}
fn default_weight() -> u32 {
    1
}
fn default_health_path() -> String {
    "/".into()
}

#[derive(Debug, Deserialize, Clone, Default, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RoutingMode {
    #[default]
    ActiveActive,
    Shadow,
    Canary,
}

#[derive(Debug, Deserialize, Clone, Default)]
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
        Self {
            max_body_bytes: default_shadow_max_body_bytes(),
        }
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

fn default_health_interval() -> u64 {
    1000
}
fn default_health_timeout() -> u64 {
    500
}
fn default_failure_threshold() -> u32 {
    3
}
fn default_success_threshold() -> u32 {
    2
}
fn default_error_rate() -> f64 {
    0.5
}
fn default_window() -> u64 {
    30
}
fn default_min_requests() -> u32 {
    20
}

pub fn load(path: &Path) -> anyhow::Result<ProxyConfig> {
    let text = std::fs::read_to_string(path)?;
    let cfg: ProxyConfig = toml::from_str(&text)?;
    validate(&cfg)?;
    Ok(cfg)
}

pub fn validate(cfg: &ProxyConfig) -> anyhow::Result<()> {
    anyhow::ensure!(!cfg.backends.is_empty(), "at least one backend required");
    // Validate every backend address as a host:port authority up front so a
    // typo fails at startup instead of panicking the forwarder's connection
    // task (its `.expect("valid backend URI")`) whenever that backend is
    // selected.
    for (name, backend) in &cfg.backends {
        validate_backend_address(&backend.address).map_err(|e| {
            anyhow::anyhow!("backends.{name}.address must be host:port (e.g. 127.0.0.1:8081): {e}")
        })?;
    }
    let total_weight: u32 = cfg.backends.values().map(|b| b.weight).sum();
    anyhow::ensure!(
        total_weight > 0,
        "at least one backend must have weight > 0"
    );
    if matches!(cfg.routing.mode, RoutingMode::Shadow) {
        let primary = cfg.routing.primary_backend.as_deref();
        let shadow = cfg.routing.shadow_backend.as_deref();
        anyhow::ensure!(
            primary.is_some(),
            "routing.mode = shadow requires routing.primary_backend"
        );
        anyhow::ensure!(
            shadow.is_some(),
            "routing.mode = shadow requires routing.shadow_backend"
        );
        anyhow::ensure!(
            primary != shadow,
            "routing.primary_backend and routing.shadow_backend must differ"
        );
        anyhow::ensure!(
            cfg.backends.contains_key(primary.unwrap()),
            "routing.primary_backend names an unknown backend"
        );
        anyhow::ensure!(
            cfg.backends.contains_key(shadow.unwrap()),
            "routing.shadow_backend names an unknown backend"
        );
    }
    cfg.listen
        .parse::<std::net::SocketAddr>()
        .map_err(|e| anyhow::anyhow!("listen must be host:port, e.g. 127.0.0.1:8080: {e}"))?;
    cfg.metrics
        .listen
        .parse::<std::net::SocketAddr>()
        .map_err(|e| {
            anyhow::anyhow!("metrics.listen must be host:port, e.g. 127.0.0.1:9100: {e}")
        })?;
    Ok(())
}

impl HealthConfig {
    pub fn interval(&self) -> Duration {
        Duration::from_millis(self.interval_ms)
    }
    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms)
    }
}

/// Validate a backend `address` is an HTTP authority (`host:port`), not a full
/// URL. Catches configuration typos at startup instead of letting them panic
/// the forwarder's connection task at runtime.
///
/// Accepted shapes: `127.0.0.1:8081`, `example.com:8080`, `[::1]:8080`.
/// Rejected: anything with a scheme (`http://...`), a path/query/fragment,
/// whitespace, a non-numeric port, or an empty host.
fn validate_backend_address(address: &str) -> anyhow::Result<()> {
    anyhow::ensure!(!address.is_empty(), "address is empty");
    anyhow::ensure!(
        !address.contains("://"),
        "remove the scheme; expected host:port, not a full URL"
    );
    anyhow::ensure!(
        !(address.contains('/') || address.contains('?') || address.contains('#')),
        "remove the path/query/fragment; expected host:port"
    );
    anyhow::ensure!(
        !address.chars().any(|c| c.is_whitespace()),
        "address must not contain whitespace"
    );

    let (host, port) = split_host_port(address);
    anyhow::ensure!(!host.is_empty(), "address must include a host");
    // A port is required: this proxy always targets explicit backend ports
    // (e.g. 8081), so a bare host like `127.0.0.1` would silently connect on
    // port 80 and is almost certainly a typo.
    let port = port.ok_or_else(|| {
        anyhow::anyhow!("address must include a port (e.g. 127.0.0.1:8081), got '{address}'")
    })?;
    anyhow::ensure!(
        !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()),
        "address port must be numeric, got '{address}'"
    );

    // Finally confirm the whole thing forms a valid URI the way the forwarder
    // builds it (`http://{address}{path}`), so anything we missed still fails
    // at startup rather than in an `expect` on the hot path.
    let probe = format!("http://{address}/");
    probe
        .parse::<hyper::Uri>()
        .map_err(|e| anyhow::anyhow!("invalid host:port '{address}': {e}"))?;
    Ok(())
}

/// Split an HTTP authority into `(host, Option<port>)`, handling bracketed
/// IPv6 literals like `[::1]:8080`. The returned `host` keeps its brackets for
/// IPv6 so the empty-host check works uniformly.
fn split_host_port(authority: &str) -> (&str, Option<&str>) {
    if authority.starts_with('[') {
        // IPv6 literal: `[host]:port`.
        match authority.find("]:") {
            Some(i) => (&authority[..=i], Some(&authority[i + 2..])),
            None => (authority, None),
        }
    } else {
        match authority.rfind(':') {
            Some(i) => (&authority[..i], Some(&authority[i + 1..])),
            None => (authority, None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_backend_toml() -> &'static str {
        r#"
listen = "127.0.0.1:8080"
log_level = "info"
state_path = "/tmp/state.json"
control_socket = "/tmp/control.sock"

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
mode = "active-active"
primary_backend = "c-thttpd"
shadow_backend = "rust-thttpd"
exclude_paths = ["/metrics"]
"#
    }

    #[test]
    fn loads_example_config() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../config/thttpd-migrate.example.toml");
        let cfg = load(&path).expect("example config must load");
        assert!(!cfg.backends.is_empty());
    }

    #[test]
    fn parses_two_backend_config() {
        let cfg: ProxyConfig = toml::from_str(two_backend_toml()).unwrap();
        validate(&cfg).unwrap();
        assert_eq!(cfg.routing.mode, RoutingMode::ActiveActive);
    }

    #[test]
    fn rejects_empty_backends() {
        let toml = r#"
listen = "127.0.0.1:8080"
[metrics]
listen = "127.0.0.1:9100"
"#;
        let cfg: ProxyConfig = toml::from_str(toml).unwrap();
        let err = validate(&cfg).unwrap_err().to_string();
        assert!(err.contains("at least one backend"), "{err}");
    }

    #[test]
    fn rejects_zero_total_weight() {
        let toml = r#"
listen = "127.0.0.1:8080"
[metrics]
listen = "127.0.0.1:9100"
[backends.a]
address = "127.0.0.1:8081"
weight = 0
"#;
        let cfg: ProxyConfig = toml::from_str(toml).unwrap();
        let err = validate(&cfg).unwrap_err().to_string();
        assert!(err.contains("weight > 0"), "{err}");
    }

    #[test]
    fn shadow_requires_primary_and_shadow_backends() {
        let toml = r#"
listen = "127.0.0.1:8080"
[metrics]
listen = "127.0.0.1:9100"
[backends.a]
address = "127.0.0.1:8081"
weight = 1
[backends.b]
address = "127.0.0.1:8082"
weight = 1
[routing]
mode = "shadow"
"#;
        let cfg: ProxyConfig = toml::from_str(toml).unwrap();
        let err = validate(&cfg).unwrap_err().to_string();
        assert!(err.contains("primary_backend"), "{err}");
    }

    #[test]
    fn rejects_unknown_primary_or_shadow_backend() {
        let toml = r#"
listen = "127.0.0.1:8080"
[metrics]
listen = "127.0.0.1:9100"
[backends.a]
address = "127.0.0.1:8081"
weight = 1
[backends.b]
address = "127.0.0.1:8082"
weight = 1
[routing]
mode = "shadow"
primary_backend = "a"
shadow_backend = "nope"
"#;
        let cfg: ProxyConfig = toml::from_str(toml).unwrap();
        let err = validate(&cfg).unwrap_err().to_string();
        assert!(err.contains("unknown backend"), "{err}");
    }

    #[test]
    fn rejects_bad_listen_address() {
        let toml = r#"
listen = ":8080"
[metrics]
listen = "127.0.0.1:9100"
[backends.a]
address = "127.0.0.1:8081"
weight = 1
"#;
        let cfg: ProxyConfig = toml::from_str(toml).unwrap();
        let err = validate(&cfg).unwrap_err().to_string();
        assert!(err.contains("listen must be host:port"), "{err}");
    }

    #[test]
    fn rejects_full_url_backend_address() {
        let toml = r#"
listen = "127.0.0.1:8080"
[metrics]
listen = "127.0.0.1:9100"
[backends.a]
address = "http://127.0.0.1:8081"
weight = 1
"#;
        let cfg: ProxyConfig = toml::from_str(toml).unwrap();
        let err = validate(&cfg).unwrap_err().to_string();
        assert!(err.contains("backends.a.address"), "{err}");
        assert!(err.contains("scheme") || err.contains("host:port"), "{err}");
    }

    #[test]
    fn rejects_backend_address_with_path() {
        let toml = r#"
listen = "127.0.0.1:8080"
[metrics]
listen = "127.0.0.1:9100"
[backends.a]
address = "127.0.0.1:8081/vhost"
weight = 1
"#;
        let cfg: ProxyConfig = toml::from_str(toml).unwrap();
        let err = validate(&cfg).unwrap_err().to_string();
        assert!(err.contains("backends.a.address"), "{err}");
        assert!(err.contains("path"), "{err}");
    }

    #[test]
    fn rejects_non_numeric_backend_port() {
        let toml = r#"
listen = "127.0.0.1:8080"
[metrics]
listen = "127.0.0.1:9100"
[backends.a]
address = "127.0.0.1:notaport"
weight = 1
"#;
        let cfg: ProxyConfig = toml::from_str(toml).unwrap();
        let err = validate(&cfg).unwrap_err().to_string();
        assert!(err.contains("backends.a.address"), "{err}");
        assert!(err.contains("numeric"), "{err}");
    }

    #[test]
    fn accepts_ipv6_backend_address() {
        let toml = r#"
listen = "127.0.0.1:8080"
[metrics]
listen = "127.0.0.1:9100"
[backends.a]
address = "[::1]:8081"
weight = 1
"#;
        let cfg: ProxyConfig = toml::from_str(toml).unwrap();
        validate(&cfg).expect("[::1]:8081 is a valid host:port authority");
    }

    #[test]
    fn rejects_portless_backend_address() {
        // A bare host with no port would silently connect on port 80; reject it.
        let toml = r#"
listen = "127.0.0.1:8080"
[metrics]
listen = "127.0.0.1:9100"
[backends.a]
address = "127.0.0.1"
weight = 1
"#;
        let cfg: ProxyConfig = toml::from_str(toml).unwrap();
        let err = validate(&cfg).unwrap_err().to_string();
        assert!(err.contains("backends.a.address"), "{err}");
        assert!(err.contains("port"), "{err}");
    }
}
