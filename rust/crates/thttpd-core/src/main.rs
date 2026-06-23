//! Binary entry point for thttpd.
//! Translates `legacy/src/thttpd.c` main().

use clap::Parser;

fn main() {
    // Normalize legacy single-dash argv spellings (-h, -nor, -nog, -nos, -dd,
    // ...) into the canonical long forms before clap parses. This keeps the
    // public CLI byte-compatible with C thttpd.
    let raw: Vec<String> = std::env::args().collect();
    let args = thttpd_core::config::normalize_legacy_args(raw);
    let legacy_toggle_events = thttpd_core::config::legacy_toggle_events(&args);

    let cli = match thttpd_core::config::Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(e) => {
            // clap handles --help/--version-style exits; print and propagate.
            e.exit();
        }
    };

    // Legacy `-V`: print SERVER_SOFTWARE and exit 0.
    if cli.legacy_version {
        println!("{}", thttpd_core::config::SERVER_SOFTWARE);
        std::process::exit(0);
    }

    let mut config = match thttpd_core::config::ServerConfig::from_cli(&cli) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("thttpd: {error}");
            std::process::exit(1);
        }
    };
    thttpd_core::config::apply_legacy_toggle_order(&mut config, &legacy_toggle_events);

    // Open the access log while still privileged (before chroot), so an
    // absolute logfile outside the jail stays writable and SIGHUP rotation
    // works. Mirrors thttpd.c:417-456.
    let mut access_log = match thttpd_core::logging::AccessLogger::open(&config) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("thttpd: logfile: {e}");
            std::process::exit(1);
        }
    };

    // Write the pidfile while still root and before chroot, so a pidfile
    // outside the chroot tree (e.g. /var/run/thttpd.pid) stays writable.
    // Legacy order: logfile -> pidfile -> daemonize -> chroot -> bind -> setuid
    // (thttpd.c:417,533,496,558,637,705).
    if let Err(e) = thttpd_core::startup::write_pidfile(&config) {
        eprintln!("thttpd: {e}");
        std::process::exit(1);
    }

    // Load the throttle file while still privileged, outside the chroot jail,
    // and BEFORE daemonizing so parse/load errors are visible on stderr.
    // Legacy reads the throttle file before chroot/setuid (thttpd.c:398-399).
    let throttles = if let Some(ref throttle_path) = config.throttle_file {
        match thttpd_core::throttle::ThrottleTable::load(throttle_path) {
            Ok(table) => Some(table),
            Err(e) => {
                eprintln!("thttpd: throttle file {}: {e}", throttle_path.display());
                std::process::exit(1);
            }
        }
    } else {
        None
    };

    // Daemonize (unless -D) with a parent-child handshake.
    //
    // The original parent blocks on a pipe until the child signals startup
    // success (or the pipe closes on failure), so callers see a non-zero exit
    // code when any post-daemon startup step fails (e.g. port already in use).
    // Must happen before chroot so the child can open /dev/null, and before
    // setuid so the child can still re-write the pidfile.
    // (thttpd.c:490-540).
    let mut handshake = if config.daemonize {
        let hs = match thttpd_core::startup::daemonize_with_handshake(access_log.is_stdout()) {
            Ok(hs) => hs,
            Err(e) => {
                eprintln!("thttpd: daemonize: {e}");
                std::process::exit(1);
            }
        };
        // After daemonizing, re-write the pidfile with the child PID.
        if let Err(e) = thttpd_core::startup::write_pidfile(&config) {
            // Parent sees exit code 1 through the handshake Drop.
            eprintln!("thttpd: {e}");
            std::process::exit(1);
        }
        Some(hs)
    } else {
        None
    };

    // Security-critical ordering: chroot -> bind -> setuid
    // (libhttpd.c:469-540). do_chroot rewrites config.dir to "/" inside the
    // jail so request resolution stays correct. data_dir then chdir's into
    // the serving root.
    let chroot_dir = config.dir.clone();
    let will_chroot = config.do_chroot;
    if let Err(e) = thttpd_core::startup::do_chroot(&mut config) {
        eprintln!("thttpd: {e}");
        std::process::exit(1);
    }
    if will_chroot {
        access_log.remap_after_chroot(&chroot_dir);
    }
    if let Err(e) = thttpd_core::startup::apply_data_dir(&mut config) {
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
    // Validate listener count against the token range before reporting
    // startup success. Otherwise the parent would see exit 0 via the
    // handshake while the child fails inside eventloop::run during listener
    // registration (thttpd-fdwatch reserves [0, MAX_LISTENERS) for listeners).
    if listeners.len() > thttpd_fdwatch::MAX_LISTENERS {
        eprintln!(
            "thttpd: too many listen sockets ({}): maximum is {}",
            listeners.len(),
            thttpd_fdwatch::MAX_LISTENERS
        );
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

    // Create the server with sockets bound before privilege drop.
    let mut server = match thttpd_core::server::Server::new(config, listeners, access_log) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("thttpd: server setup failed: {e}");
            std::process::exit(1);
        }
    };

    // Install the throttle table loaded before chroot (see above).
    server.throttles = throttles;

    // Signal successful startup to the parent (daemon handshake).
    if let Some(ref mut hs) = handshake {
        hs.report_success();
    }

    // Run event loop
    if let Err(e) = thttpd_core::eventloop::run(&mut server) {
        eprintln!("thttpd: event loop error: {e}");
        std::process::exit(1);
    }
}
