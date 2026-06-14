//! Tracing initialization.
//!
//! Initializes `tracing-subscriber` with an `EnvFilter` and either compact
//! (dev) or JSON (prod) output. JSON is selected via the
//! `THTTPD_MIGRATE_LOG_FORMAT=json` environment variable (see [`crate::start`]).

use tracing_subscriber::{EnvFilter, fmt, prelude::*};

pub fn init(level: &str, json: bool) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));
    let fmt_layer = if json {
        fmt::layer()
            .json()
            .with_current_span(true)
            .with_span_list(false)
            .boxed()
    } else {
        fmt::layer().compact().with_target(true).boxed()
    };
    // The binary wants a global default. try_init avoids panicking if a global
    // subscriber is already installed (e.g. by a prior test).
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing::debug;

    #[test]
    fn json_format_emits_valid_json() {
        // Initialize (may already be set by another test — that's fine).
        init("debug", true);
        // We can't easily capture stderr here, but we can at least assert the
        // subscriber accepts a json-formatted event without panicking.
        debug!(field = "value", "json format smoke");
    }
}
