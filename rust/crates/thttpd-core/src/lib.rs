//! Core thttpd server.
//! Translates `legacy/src/thttpd.c`.

pub mod config;
pub mod connection;
pub mod eventloop;
pub mod logging;
pub mod server;
pub mod signal;
pub mod startup;
pub mod throttle;

pub use config::ServerConfig;
