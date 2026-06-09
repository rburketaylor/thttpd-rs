//! Server struct for thttpd.
//! Holds all runtime state: Poll, timer wheel, mmap cache, connection table.

use crate::config::ServerConfig;
use crate::connection::ConnSlot;
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
}

impl Server {
    pub fn new(config: ServerConfig) -> std::io::Result<Self> {
        let poll = Poll::new()?;
        Ok(Self {
            config,
            poll,
            timers: TimerWheel::new(),
            mmc: MmapCache::new(),
            stats: ServerStats::default(),
            conns: slab::Slab::new(),
            listeners: Vec::new(),
        })
    }
}
