//! Binary entry point for thttpd.
//! Translates `legacy/src/thttpd.c` main().

use clap::Parser;

fn main() {
    let cli = thttpd_core::config::Cli::parse();
    let mut config = match thttpd_core::config::ServerConfig::from_cli(&cli) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("thttpd: {error}");
            std::process::exit(1);
        }
    };

    // Write the pidfile first, while still root and before chroot, so a
    // pidfile outside the chroot tree (e.g. /var/run/thttpd.pid) remains
    // writable. Legacy order: pidfile -> chroot -> bind -> setuid
    // (thttpd.c:533,558,637,705).
    if let Err(e) = thttpd_core::startup::write_pidfile(&config) {
        eprintln!("thttpd: {e}");
        std::process::exit(1);
    }

    // Security-critical ordering: chroot -> bind -> setuid
    // (libhttpd.c:469-540). do_chroot rewrites config.dir to "/" inside the
    // jail so request resolution stays correct.
    if let Err(e) = thttpd_core::startup::do_chroot(&mut config) {
        eprintln!("thttpd: {e}");
        std::process::exit(1);
    }
    let listeners = match thttpd_core::startup::bind_listeners(&config) {
        Ok(listeners) => listeners,
        Err(e) => {
            eprintln!("thttpd: bind failed: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = thttpd_core::startup::drop_privileges(&config) {
        eprintln!("thttpd: {e}");
        std::process::exit(1);
    }

    // Install signal handlers
    if let Err(e) = thttpd_core::signal::install_signal_handlers() {
        eprintln!("thttpd: signal handler setup failed: {e}");
        std::process::exit(1);
    }

    // Create the server with sockets bound before privilege drop.
    let mut server = match thttpd_core::server::Server::new(config, listeners) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("thttpd: server setup failed: {e}");
            std::process::exit(1);
        }
    };

    // Run event loop
    if let Err(e) = thttpd_core::eventloop::run(&mut server) {
        eprintln!("thttpd: event loop error: {e}");
        std::process::exit(1);
    }
}
