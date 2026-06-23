//! CLI argument parsing and legacy configuration-file support for thttpd.
//!
//! Legacy argv compatibility: thttpd uses several multi-character
//! single-dash flags (e.g. `-nor`, `-nog`, `-nos`, `-dd`) and reserves `-h`
//! for the hostname, `-V` to print the server version, and `-g`/`-s` as
//! on-switches. clap's native short-flag handling cannot express those
//! single-dash multi-character forms, so [`normalize_legacy_args`] rewrites
//! the legacy spellings into canonical `--long` forms before clap parses.
//! This keeps the public CLI byte-compatible with C thttpd while the parser
//! itself stays clean.

use clap::Parser;
use std::path::{Path, PathBuf};

/// Canonical server-software string, surfaced by `-V`.
pub const SERVER_SOFTWARE: &str = "sthttpd/2.27.0 03oct2014";

/// Rewrite legacy single-dash argv spellings into canonical `--long` forms.
///
/// Only exact token matches are translated, so ordinary short flags such as
// `-p`, `-d`, and bundled options are passed through untouched. Value-taking
/// legacy flags (`-h HOST`, `-dd DIR`) consume the following argument as their
// value, matching C thttpd's argv scan.
pub fn normalize_legacy_args(args: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut iter = args.into_iter().peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            // Value-taking legacy flags: emit the long form and let the next
            // argument flow through as the value.
            "-h" => {
                out.push("--hostname".to_string());
                if let Some(next) = iter.next() {
                    out.push(next);
                }
            }
            "-dd" => {
                out.push("--data-dir".to_string());
                if let Some(next) = iter.next() {
                    out.push(next);
                }
            }
            // Boolean legacy flags.
            "-nor" => out.push("--nor".to_string()),
            "-nov" => out.push("--nov".to_string()),
            "-g" => out.push("--global-passwd".to_string()),
            "-nog" | "-noP" => out.push("--no-global-passwd".to_string()),
            "-s" => out.push("--symlinks".to_string()),
            "-nos" => out.push("--no-symlinks".to_string()),
            _ => out.push(arg),
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyToggle {
    Chroot(bool),
    SymlinkCheck(bool),
    Vhost(bool),
    GlobalPasswd(bool),
}

/// Extract order-sensitive legacy toggles from already-normalized argv.
pub fn legacy_toggle_events(args: &[String]) -> Vec<LegacyToggle> {
    let mut events = Vec::new();
    let mut skip_next = false;

    for arg in args.iter().skip(1) {
        if skip_next {
            skip_next = false;
            continue;
        }

        match arg.as_str() {
            "-r" | "--chroot" => events.push(LegacyToggle::Chroot(true)),
            "--nor" => events.push(LegacyToggle::Chroot(false)),
            "--symlinks" => events.push(LegacyToggle::SymlinkCheck(true)),
            "--no-symlinks" => events.push(LegacyToggle::SymlinkCheck(false)),
            "-v" | "--vhost" => events.push(LegacyToggle::Vhost(true)),
            "--nov" => events.push(LegacyToggle::Vhost(false)),
            "--global-passwd" => events.push(LegacyToggle::GlobalPasswd(true)),
            "--no-global-passwd" => events.push(LegacyToggle::GlobalPasswd(false)),
            "-p" | "--port" | "-d" | "--dir" | "--data-dir" | "-u" | "--user" | "-l" | "--log"
            | "-c" | "--cgipat" | "-T" | "--charset" | "-P" | "--p3p" | "-M" | "--maxage"
            | "-C" | "--config" | "-t" | "--throttle-file" | "-H" | "--hostname" | "-i"
            | "--pidfile" | "--cgi-limit" => skip_next = true,
            _ => {}
        }
    }

    events
}

pub fn apply_legacy_toggle_order(config: &mut ServerConfig, events: &[LegacyToggle]) {
    for event in events {
        match *event {
            LegacyToggle::Chroot(true) => {
                config.do_chroot = true;
                config.no_symlink_check = true;
            }
            LegacyToggle::Chroot(false) => {
                config.do_chroot = false;
                config.no_symlink_check = false;
            }
            LegacyToggle::SymlinkCheck(true) => {
                config.no_symlink_check = false;
            }
            LegacyToggle::SymlinkCheck(false) => {
                config.no_symlink_check = true;
            }
            LegacyToggle::Vhost(enabled) => {
                config.vhost = enabled;
            }
            LegacyToggle::GlobalPasswd(enabled) => {
                config.global_passwd = enabled;
            }
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "thttpd",
    about = "thttpd HTTP server",
    disable_version_flag = true
)]
pub struct Cli {
    #[arg(short = 'p', long = "port")]
    pub port: Option<u16>,
    #[arg(short = 'd', long = "dir")]
    pub dir: Option<PathBuf>,
    #[arg(short = 'r', long = "chroot")]
    pub chroot: bool,
    #[arg(long = "nor")]
    pub no_chroot: bool,
    /// Legacy `-dd` — data directory to chdir into after chroot.
    #[arg(long = "data-dir")]
    pub data_dir: Option<PathBuf>,
    /// Legacy `-s` — perform symlink checks (disable `no_symlink_check`).
    #[arg(long = "symlinks")]
    pub symlinks: bool,
    /// Legacy `-nos` — skip symlink checks.
    #[arg(long = "no-symlinks")]
    pub no_symlinks: bool,
    #[arg(short = 'u', long = "user")]
    pub user: Option<String>,
    #[arg(short = 'l', long = "log")]
    pub logfile: Option<PathBuf>,
    #[arg(short = 'c', long = "cgipat")]
    pub cgipat: Option<String>,
    #[arg(short = 'T', long = "charset")]
    pub charset: Option<String>,
    #[arg(short = 'P', long = "p3p")]
    pub p3p: Option<String>,
    #[arg(short = 'M', long = "maxage")]
    pub max_age: Option<i32>,
    #[arg(long = "nov")]
    pub no_vhost: bool,
    #[arg(short = 'v', long = "vhost")]
    pub vhost: bool,
    /// Legacy `-g` — enable global password checking.
    #[arg(long = "global-passwd")]
    pub global_passwd: bool,
    /// Legacy `-nog` / `-noP` — disable global password checking.
    #[arg(long = "no-global-passwd")]
    pub no_global_passwd: bool,
    #[arg(short = 'C', long = "config")]
    pub config_file: Option<PathBuf>,
    #[arg(short = 'D', long = "debug")]
    pub debug: bool,
    #[arg(short = 't', long = "throttle-file")]
    pub throttle_file: Option<PathBuf>,
    #[arg(short = 'H', long = "hostname")]
    pub hostname: Option<String>,
    #[arg(short = 'i', long = "pidfile")]
    pub pidfile: Option<PathBuf>,
    #[arg(long = "cgi-limit")]
    pub cgi_limit: Option<i32>,
    /// Legacy `-V` — print the server software string and exit.
    #[arg(short = 'V', long = "legacy-version")]
    pub legacy_version: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    pub port: u16,
    pub dir: PathBuf,
    pub do_chroot: bool,
    /// When true, skip the symlink-escape check. Mirrors C's `no_symlink_check`:
    /// defaults to the value of `do_chroot`, flipped by `-r`/`-nor`/`-s`/`-nos`.
    pub no_symlink_check: bool,
    /// Legacy `data_dir` — chdir into this after chroot, before serving.
    pub data_dir: Option<PathBuf>,
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

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: 80,
            dir: PathBuf::from("."),
            do_chroot: false,
            // C inits no_symlink_check = do_chroot; both default to 0.
            no_symlink_check: false,
            data_dir: None,
            user: None,
            logfile: None,
            cgi_pattern: None,
            cgi_limit: None,
            charset: "iso-8859-1".to_string(),
            p3p: None,
            max_age: -1,
            vhost: false,
            global_passwd: true,
            url_pattern: None,
            local_pattern: None,
            no_empty_referers: false,
            hostname: None,
            throttle_file: None,
            pidfile: None,
            daemonize: true,
        }
    }
}

impl ServerConfig {
    /// Load a legacy `thttpd.conf`, then apply explicit CLI overrides.
    pub fn from_cli(cli: &Cli) -> Result<Self, String> {
        let mut config = Self::default();
        if let Some(path) = &cli.config_file {
            config.apply_config_file(path)?;
        }

        if let Some(port) = cli.port {
            config.port = port;
        }
        if let Some(dir) = &cli.dir {
            config.dir = dir.clone();
        }
        if cli.chroot {
            config.do_chroot = true;
            // `-r` implies no symlink checking, matching C thttpd.
            config.no_symlink_check = true;
        }
        if cli.no_chroot {
            config.do_chroot = false;
            config.no_symlink_check = false;
        }
        if let Some(dir) = &cli.data_dir {
            config.data_dir = Some(dir.clone());
        }
        // `-s`/`-nos` are applied after `-r`/`-nor` so the explicit symlink
        // toggles win over the chroot implication (e.g. `-r -s` checks links).
        if cli.symlinks {
            config.no_symlink_check = false;
        }
        if cli.no_symlinks {
            config.no_symlink_check = true;
        }
        if let Some(user) = &cli.user {
            config.user = Some(user.clone());
        }
        if let Some(logfile) = &cli.logfile {
            config.logfile = Some(logfile.clone());
        }
        if let Some(pattern) = &cli.cgipat {
            config.cgi_pattern = Some(pattern.clone());
        }
        if let Some(limit) = cli.cgi_limit {
            config.cgi_limit = Some(limit);
        }
        if let Some(charset) = &cli.charset {
            config.charset = charset.clone();
        }
        if let Some(p3p) = &cli.p3p {
            config.p3p = Some(p3p.clone());
        }
        if let Some(max_age) = cli.max_age {
            config.max_age = max_age;
        }
        if cli.vhost {
            config.vhost = true;
        }
        if cli.no_vhost {
            config.vhost = false;
        }
        if cli.global_passwd {
            config.global_passwd = true;
        }
        if cli.no_global_passwd {
            config.global_passwd = false;
        }
        if let Some(hostname) = &cli.hostname {
            config.hostname = Some(hostname.clone());
        }
        if let Some(path) = &cli.throttle_file {
            config.throttle_file = Some(path.clone());
        }
        if let Some(path) = &cli.pidfile {
            config.pidfile = Some(path.clone());
        }
        if cli.debug {
            config.daemonize = false;
        }

        Ok(config)
    }

    fn apply_config_file(&mut self, path: &Path) -> Result<(), String> {
        let contents = std::fs::read_to_string(path)
            .map_err(|error| format!("{}: {error}", path.display()))?;

        for (line_index, raw_line) in contents.lines().enumerate() {
            let line = raw_line.split('#').next().unwrap_or("");
            for option in line.split_whitespace() {
                let (name, value) = option
                    .split_once('=')
                    .map_or((option, None), |(name, value)| (name, Some(value)));
                self.apply_config_option(name, value)
                    .map_err(|error| format!("{}:{}: {error}", path.display(), line_index + 1))?;
            }
        }
        Ok(())
    }

    fn apply_config_option(&mut self, name: &str, value: Option<&str>) -> Result<(), String> {
        let key = name.to_ascii_lowercase();
        let required = || value.ok_or_else(|| format!("value required for {name} option"));
        let no_value = || {
            if value.is_some() {
                Err(format!("no value required for {name} option"))
            } else {
                Ok(())
            }
        };

        match key.as_str() {
            "debug" => {
                no_value()?;
                self.daemonize = false;
            }
            "port" => {
                self.port = required()?
                    .parse()
                    .map_err(|_| format!("invalid port for {name} option"))?;
            }
            "dir" => self.dir = PathBuf::from(required()?),
            "chroot" => {
                no_value()?;
                self.do_chroot = true;
                self.no_symlink_check = true;
            }
            "nochroot" => {
                no_value()?;
                self.do_chroot = false;
                self.no_symlink_check = false;
            }
            "data_dir" => self.data_dir = Some(PathBuf::from(required()?)),
            "symlink" | "symlinks" => {
                no_value()?;
                self.no_symlink_check = false;
            }
            "nosymlink" | "nosymlinks" => {
                no_value()?;
                self.no_symlink_check = true;
            }
            "user" => self.user = Some(required()?.to_string()),
            "cgipat" => self.cgi_pattern = Some(required()?.to_string()),
            "cgilimit" => {
                self.cgi_limit = Some(
                    required()?
                        .parse()
                        .map_err(|_| format!("invalid integer for {name} option"))?,
                );
            }
            "urlpat" => self.url_pattern = Some(required()?.to_string()),
            "noemptyreferers" => {
                no_value()?;
                self.no_empty_referers = true;
            }
            "localpat" => self.local_pattern = Some(required()?.to_string()),
            "throttles" => self.throttle_file = Some(PathBuf::from(required()?)),
            "host" => self.hostname = Some(required()?.to_string()),
            "logfile" => self.logfile = Some(PathBuf::from(required()?)),
            "vhost" => {
                no_value()?;
                self.vhost = true;
            }
            "novhost" => {
                no_value()?;
                self.vhost = false;
            }
            "globalpasswd" => {
                no_value()?;
                self.global_passwd = true;
            }
            "noglobalpasswd" => {
                no_value()?;
                self.global_passwd = false;
            }
            "pidfile" => self.pidfile = Some(PathBuf::from(required()?)),
            "charset" => self.charset = required()?.to_string(),
            "p3p" => self.p3p = Some(required()?.to_string()),
            "max_age" => {
                self.max_age = required()?
                    .parse()
                    .map_err(|_| format!("invalid integer for {name} option"))?;
            }
            _ => return Err(format!("unknown config option '{name}'")),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn cli(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).unwrap()
    }

    fn config_from_argv(args: &[&str]) -> ServerConfig {
        let normalized = normalize_legacy_args(args.iter().map(|s| s.to_string()).collect());
        let events = legacy_toggle_events(&normalized);
        let parsed = Cli::try_parse_from(normalized).unwrap();
        let mut config = ServerConfig::from_cli(&parsed).unwrap();
        apply_legacy_toggle_order(&mut config, &events);
        config
    }

    #[test]
    fn defaults_match_legacy_server() {
        let config = ServerConfig::from_cli(&cli(&["thttpd"])).unwrap();
        assert_eq!(config.port, 80);
        assert_eq!(config.charset, "iso-8859-1");
        assert!(config.global_passwd);
        assert!(config.daemonize);
    }

    #[test]
    fn loads_legacy_config_and_applies_cli_overrides() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "port=8080 dir=/srv/www vhost # comment").unwrap();
        writeln!(file, "charset=utf-8 throttles=/tmp/throttles").unwrap();
        let config_arg = file.path().to_string_lossy().to_string();
        let parsed = cli(&["thttpd", "-C", &config_arg, "-p", "9090"]);
        let config = ServerConfig::from_cli(&parsed).unwrap();
        assert_eq!(config.port, 9090);
        assert_eq!(config.dir, PathBuf::from("/srv/www"));
        assert!(config.vhost);
        assert_eq!(config.charset, "utf-8");
        assert_eq!(config.throttle_file, Some(PathBuf::from("/tmp/throttles")));
    }

    #[test]
    fn rejects_unknown_options() {
        let mut unknown = tempfile::NamedTempFile::new().unwrap();
        writeln!(unknown, "mystery=value").unwrap();
        let unknown_arg = unknown.path().to_string_lossy().to_string();
        let error = ServerConfig::from_cli(&cli(&["thttpd", "-C", &unknown_arg])).unwrap_err();
        assert!(error.contains("unknown config option"));
    }

    #[test]
    fn chroot_implies_no_symlink_check_and_nor_restores() {
        // `-r` (chroot) turns on no_symlink_check.
        let cfg = ServerConfig::from_cli(&cli(&["thttpd", "--chroot"])).unwrap();
        assert!(cfg.do_chroot);
        assert!(cfg.no_symlink_check);

        // `-nor` restores symlink checking.
        let cfg = ServerConfig::from_cli(&cli(&["thttpd", "--nor"])).unwrap();
        assert!(!cfg.do_chroot);
        assert!(!cfg.no_symlink_check);
    }

    #[test]
    fn symlink_flags_override_chroot_implication() {
        // `-r -s`: chroot but still check symlinks.
        let cfg = ServerConfig::from_cli(&cli(&["thttpd", "--chroot", "--symlinks"])).unwrap();
        assert!(cfg.do_chroot);
        assert!(!cfg.no_symlink_check);

        // `-s` alone checks symlinks; `-nos` skips them.
        assert!(
            !ServerConfig::from_cli(&cli(&["thttpd", "--symlinks"]))
                .unwrap()
                .no_symlink_check
        );
        assert!(
            ServerConfig::from_cli(&cli(&["thttpd", "--no-symlinks"]))
                .unwrap()
                .no_symlink_check
        );
    }

    #[test]
    fn legacy_argv_short_flags_normalize() {
        // Multi-char legacy single-dash flags + value flags must reach the
        // same ServerConfig as the canonical long forms.
        let legacy = normalize_legacy_args(
            [
                "thttpd",
                "-h",
                "example.com",
                "-dd",
                "/srv/data",
                "-g",
                "-s",
                "-nor",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        );
        let parsed = Cli::try_parse_from(legacy).unwrap();
        let cfg = ServerConfig::from_cli(&parsed).unwrap();
        assert_eq!(cfg.hostname.as_deref(), Some("example.com"));
        assert_eq!(
            cfg.data_dir.as_deref(),
            Some(std::path::Path::new("/srv/data"))
        );
        assert!(cfg.global_passwd);
        assert!(!cfg.no_symlink_check);
        assert!(!cfg.do_chroot);

        // `-nog` / `-nos` flip the toggles the other way.
        let legacy = normalize_legacy_args(
            ["thttpd", "-nog", "-nos"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        );
        let parsed = Cli::try_parse_from(legacy).unwrap();
        let cfg = ServerConfig::from_cli(&parsed).unwrap();
        assert!(!cfg.global_passwd);
        assert!(cfg.no_symlink_check);
    }

    #[test]
    fn legacy_chroot_toggles_are_last_wins() {
        let cfg = config_from_argv(&["thttpd", "-r", "-nor"]);
        assert!(!cfg.do_chroot);
        assert!(!cfg.no_symlink_check);

        let cfg = config_from_argv(&["thttpd", "-nor", "-r"]);
        assert!(cfg.do_chroot);
        assert!(cfg.no_symlink_check);

        let cfg = config_from_argv(&["thttpd", "--chroot", "--nor"]);
        assert!(!cfg.do_chroot);
        assert!(!cfg.no_symlink_check);

        let cfg = config_from_argv(&["thttpd", "--nor", "--chroot"]);
        assert!(cfg.do_chroot);
        assert!(cfg.no_symlink_check);
    }

    #[test]
    fn legacy_global_passwd_toggles_are_last_wins() {
        assert!(!config_from_argv(&["thttpd", "-g", "-nog"]).global_passwd);
        assert!(config_from_argv(&["thttpd", "-nog", "-g"]).global_passwd);
        assert!(
            !config_from_argv(&["thttpd", "--global-passwd", "--no-global-passwd"]).global_passwd
        );
        assert!(
            config_from_argv(&["thttpd", "--no-global-passwd", "--global-passwd"]).global_passwd
        );
    }

    #[test]
    fn legacy_vhost_and_symlink_toggles_are_last_wins() {
        assert!(!config_from_argv(&["thttpd", "-v", "-nov"]).vhost);
        assert!(config_from_argv(&["thttpd", "-nov", "-v"]).vhost);

        assert!(config_from_argv(&["thttpd", "-s", "-nos"]).no_symlink_check);
        assert!(!config_from_argv(&["thttpd", "-nos", "-s"]).no_symlink_check);
        assert!(config_from_argv(&["thttpd", "--symlinks", "--no-symlinks"]).no_symlink_check);
        assert!(!config_from_argv(&["thttpd", "--no-symlinks", "--symlinks"]).no_symlink_check);
    }

    #[test]
    fn config_file_data_dir_and_symlink_options_supported() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "data_dir=/srv/data symlink nosymlinks").unwrap();
        let config_arg = file.path().to_string_lossy().to_string();
        let cfg = ServerConfig::from_cli(&cli(&["thttpd", "-C", &config_arg])).unwrap();
        assert_eq!(
            cfg.data_dir.as_deref(),
            Some(std::path::Path::new("/srv/data"))
        );
        // `symlink` sets no_symlink_check=false, then `nosymlinks` sets it true.
        assert!(cfg.no_symlink_check);
    }
}
