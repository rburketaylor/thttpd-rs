//! CLI argument parsing and configuration for thttpd.
use clap::Parser;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "thttpd", version, about = "thttpd HTTP server")]
pub struct Cli {
    #[arg(short = 'p', long = "port")] pub port: Option<u16>,
    #[arg(short = 'd', long = "dir")] pub dir: Option<PathBuf>,
    #[arg(short = 'r', long = "chroot")] pub chroot: bool,
    #[arg(short = 'u', long = "user")] pub user: Option<String>,
    #[arg(short = 'l', long = "log")] pub logfile: Option<PathBuf>,
    #[arg(short = 'c', long = "cgipat")] pub cgipat: Option<String>,
    #[arg(short = 'T', long = "charset")] pub charset: Option<String>,
    #[arg(long = "p3p")] pub p3p: Option<String>,
    #[arg(short = 'M', long = "maxage")] pub max_age: Option<i32>,
    #[arg(long = "nor")] pub no_chroot: bool,
    #[arg(long = "nov")] pub no_vhost: bool,
    #[arg(long = "noP")] pub no_global_passwd: bool,
    #[arg(short = 'C', long = "config")] pub config_file: Option<PathBuf>,
    #[arg(short = 'D', long = "debug")] pub debug: bool,
    #[arg(short = 't', long = "throttle-file")] pub throttle_file: Option<PathBuf>,
    #[arg(short = 'H', long = "hostname")] pub hostname: Option<String>,
    #[arg(short = 'i', long = "pidfile")] pub pidfile: Option<PathBuf>,
    #[arg(long = "cgi-limit")] pub cgi_limit: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub port: u16,
    pub dir: PathBuf,
    pub do_chroot: bool,
    pub user: Option<String>,
    pub logfile: Option<PathBuf>,
    pub cgi_pattern: Option<String>,
    pub cgi_limit: Option<i32>,
    pub charset: String,
    pub p3p: Option<String>,
    pub max_age: i32,
    pub vhost: bool,
    pub global_passwd: bool,
    pub url_pattern: Option<String>,
    pub local_pattern: Option<String>,
    pub no_empty_referers: bool,
    pub hostname: Option<String>,
    pub throttle_file: Option<PathBuf>,
    pub pidfile: Option<PathBuf>,
    pub daemonize: bool,
}

impl ServerConfig {
    pub fn from_cli(cli: &Cli) -> Self {
        Self {
            port: cli.port.unwrap_or(80),
            dir: cli.dir.clone().unwrap_or_else(|| PathBuf::from(".")),
            do_chroot: cli.chroot,
            user: cli.user.clone(),
            logfile: cli.logfile.clone(),
            cgi_pattern: cli.cgipat.clone(),
            cgi_limit: cli.cgi_limit,
            charset: cli.charset.clone().unwrap_or_else(|| "iso-8859-1".to_string()),
            p3p: cli.p3p.clone(),
            max_age: cli.max_age.unwrap_or(-1),
            vhost: !cli.no_vhost,
            global_passwd: !cli.no_global_passwd,
            url_pattern: None,
            local_pattern: None,
            no_empty_referers: false,
            hostname: cli.hostname.clone(),
            throttle_file: cli.throttle_file.clone(),
            pidfile: cli.pidfile.clone(),
            daemonize: !cli.debug,
        }
    }
}
