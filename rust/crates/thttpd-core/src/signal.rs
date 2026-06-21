//! Signal handling for thttpd.
//! Translates `legacy/src/thttpd.c:346-372`.
//! Uses signal-hook for unified event loop.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};

struct SignalFlags {
    terminate: Arc<AtomicBool>,
    hup: Arc<AtomicBool>,
    usr1: Arc<AtomicBool>,
    /// SIGPIPE sink — registered so the default action (terminate the process)
    /// is replaced by setting a flag we simply never read. Equivalent to
    /// ignoring the signal, but via the safe `signal_hook::flag::register`
    /// path rather than the `unsafe` low-level registrar.
    sigpipe_sink: Arc<AtomicBool>,
}

impl SignalFlags {
    fn new() -> Self {
        Self {
            terminate: Arc::new(AtomicBool::new(false)),
            hup: Arc::new(AtomicBool::new(false)),
            usr1: Arc::new(AtomicBool::new(false)),
            sigpipe_sink: Arc::new(AtomicBool::new(false)),
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
    use signal_hook::consts::{SIGHUP, SIGINT, SIGPIPE, SIGTERM, SIGUSR1};
    use signal_hook::flag;

    flag::register(SIGTERM, FLAGS.terminate.clone())?;
    flag::register(SIGINT, FLAGS.terminate.clone())?;
    flag::register(SIGHUP, FLAGS.hup.clone())?;
    flag::register(SIGUSR1, FLAGS.usr1.clone())?;
    // Swallow SIGPIPE: register it with an AtomicBool whose value is never
    // read. This replaces the previous `unsafe` low-level registrar with the
    // safe flag path, so a `write()` to a closed pipe returns `Err(EPIPE)`
    // instead of killing the process — no `unsafe` required.
    flag::register(SIGPIPE, FLAGS.sigpipe_sink.clone())?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// After `install_signal_handlers`, writing to a closed read end of a pipe
    /// must return `Err(EPIPE)` rather than terminate the process via SIGPIPE.
    /// This is the behavior the safe SIGPIPE registrar exists to preserve.
    #[test]
    fn write_to_closed_pipe_returns_epipe() {
        install_signal_handlers().expect("install signal handlers");

        use std::os::fd::AsFd;
        let (read, write) = std::os::unix::net::UnixStream::pair().unwrap();
        // Close the read end so the write end has no readers.
        drop(read);
        let mut writer = std::fs::File::from(write.as_fd().try_clone_to_owned().unwrap());
        // Write enough that the kernel notices there are no readers. The first
        // write on a peer-closed socket yields EPIPE (and would raise SIGPIPE
        // without our handler).
        let result = writer.write_all(b"x");
        let err = result.unwrap_err();
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::BrokenPipe,
            "expected BrokenPipe/EPIPE, got {err:?}"
        );
    }
}
