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
    #[arg(
        long,
        global = true,
        default_value = "/var/run/thttpd-migrate/control.sock"
    )]
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
        /// Override log level (trace, debug, info, warn, error). When omitted,
        /// the `log_level` from the config file is used.
        #[arg(long)]
        log_level: Option<String>,
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
        Cmd::Start {
            config,
            listen,
            log_level,
        } => thttpd_migrate::start(config, listen, log_level).await,
        Cmd::Status { state } => thttpd_migrate::status(state),
        Cmd::SetWeight { pairs } => thttpd_migrate::set_weight(cli.control_socket, pairs).await,
        Cmd::Drain { timeout_secs } => {
            thttpd_migrate::drain(cli.control_socket, timeout_secs).await
        }
        Cmd::Rollback { to } => thttpd_migrate::rollback(cli.control_socket, &to).await,
    }
}
