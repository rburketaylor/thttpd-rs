//! Server struct for thttpd.
//! Holds all runtime state: Poll, timer wheel, mmap cache, connection table.

use crate::config::ServerConfig;
use crate::connection::ConnSlot;
use crate::logging::AccessLogger;
use crate::throttle::ThrottleTable;
use thttpd_fdwatch::Poll;
use thttpd_mmc::MmapCache;
use thttpd_timers::TimerWheel;

/// Server statistics.
#[derive(Debug, Default)]
pub struct ServerStats {
    pub connections: u64,
    pub requests: u64,
    pub bytes_sent: u64,
}

/// The main server state.
pub struct Server {
    pub config: ServerConfig,
    pub poll: Poll,
    pub timers: TimerWheel,
    pub mmc: MmapCache,
    pub stats: ServerStats,
    /// Connection table — slab-allocated for O(1) insert/remove.
    pub conns: slab::Slab<ConnSlot>,
    /// Listen sockets (registered with poll before entering the event loop).
    pub listeners: Vec<thttpd_fdwatch::TcpListener>,
    /// Bandwidth throttle table. `None` when no `-t`/`throttles` was supplied.
    pub throttles: Option<ThrottleTable>,
    /// Access-log owner, opened while still privileged.
    pub access_log: AccessLogger,
    /// Currently-active CGI processes; bounded by `config.cgi_limit` (default 50).
    pub active_cgis: i32,
    /// Graceful-drain flag set by SIGUSR1: stop accepting, finish active, exit.
    pub draining: bool,
}

impl Server {
    pub fn new(
        config: ServerConfig,
        listeners: Vec<thttpd_fdwatch::TcpListener>,
        access_log: AccessLogger,
    ) -> std::io::Result<Self> {
        let poll = Poll::new()?;
        Ok(Self {
            config,
            poll,
            timers: TimerWheel::new(),
            mmc: MmapCache::new(),
            stats: ServerStats::default(),
            conns: slab::Slab::new(),
            listeners,
            throttles: None,
            access_log,
            active_cgis: 0,
            draining: false,
        })
    }

    /// Effective CGI limit. Defaults to legacy `CGI_LIMIT = 50`; an explicit
    /// `0` means unlimited (legacy: `cgi_limit != 0` gates admission).
    #[inline]
    pub fn cgi_limit(&self) -> i32 {
        self.config.cgi_limit.unwrap_or(50)
    }
}
