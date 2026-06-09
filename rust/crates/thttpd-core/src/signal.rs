//! Signal handling for thttpd.
//! Translates `legacy/src/thttpd.c:346-372`.
//! Uses signal-hook for unified event loop.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};

struct SignalFlags {
    terminate: Arc<AtomicBool>,
    hup: Arc<AtomicBool>,
    usr1: Arc<AtomicBool>,
}

impl SignalFlags {
    fn new() -> Self {
        Self {
            terminate: Arc::new(AtomicBool::new(false)),
            hup: Arc::new(AtomicBool::new(false)),
            usr1: Arc::new(AtomicBool::new(false)),
        }
    }
}

static FLAGS: LazyLock<SignalFlags> = LazyLock::new(SignalFlags::new);

/// Check if a termination signal was received.
pub fn got_terminate() -> bool {
    FLAGS.terminate.load(Ordering::Relaxed)
}

/// Check if SIGHUP was received.
pub fn got_hup() -> bool {
    FLAGS.hup.load(Ordering::Relaxed)
}

/// Clear the SIGHUP flag.
pub fn clear_hup() {
    FLAGS.hup.store(false, Ordering::Relaxed);
}

/// Check if SIGUSR1 was received.
pub fn got_usr1() -> bool {
    FLAGS.usr1.load(Ordering::Relaxed)
}

/// Set up signal handlers.
pub fn install_signal_handlers() -> std::io::Result<()> {
    use signal_hook::consts::{SIGTERM, SIGINT, SIGHUP, SIGUSR1, SIGPIPE};
    use signal_hook::flag;

    flag::register(SIGTERM, FLAGS.terminate.clone())?;
    flag::register(SIGINT, FLAGS.terminate.clone())?;
    flag::register(SIGHUP, FLAGS.hup.clone())?;
    flag::register(SIGUSR1, FLAGS.usr1.clone())?;
    // Ignore SIGPIPE
    unsafe {
        let _ = signal_hook::low_level::register(SIGPIPE, || {});
    }

    Ok(())
}
