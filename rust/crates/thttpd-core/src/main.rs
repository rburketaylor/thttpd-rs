//! Binary entry point for thttpd.
//! Translates `legacy/src/thttpd.c` main().

use clap::Parser;

fn main() {
    let cli = thttpd_core::config::Cli::parse();
    let config = thttpd_core::config::ServerConfig::from_cli(&cli);

    // Security-critical ordering: chroot → bind → setuid
    // (libhttpd.c:469-540)
    if let Err(e) = thttpd_core::startup::do_chroot(&config) {
        eprintln!("thttpd: {e}");
        std::process::exit(1);
    }
    if let Err(e) = thttpd_core::startup::drop_privileges(&config) {
        eprintln!("thttpd: {e}");
        std::process::exit(1);
    }

    // Install signal handlers
    if let Err(e) = thttpd_core::signal::install_signal_handlers() {
        eprintln!("thttpd: signal handler setup failed: {e}");
        std::process::exit(1);
    }

    // Create server (binds listen sockets — must be after chroot)
    let mut server = match thttpd_core::server::Server::new(config) {
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
