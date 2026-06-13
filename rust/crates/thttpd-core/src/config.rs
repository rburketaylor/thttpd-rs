//! CLI argument parsing and legacy configuration-file support for thttpd.

use clap::Parser;
use std::path::{Path, PathBuf};

#[derive(Debug, Parser)]
#[command(name = "thttpd", version, about = "thttpd HTTP server")]
pub struct Cli {
    #[arg(short = 'p', long = "port")]
    pub port: Option<u16>,
    #[arg(short = 'd', long = "dir")]
    pub dir: Option<PathBuf>,
    #[arg(short = 'r', long = "chroot")]
    pub chroot: bool,
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
    #[arg(long = "nor")]
    pub no_chroot: bool,
    #[arg(long = "nov", conflicts_with = "vhost")]
    pub no_vhost: bool,
    #[arg(short = 'v', long = "vhost", conflicts_with = "no_vhost")]
    pub vhost: bool,
    #[arg(long = "noP")]
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
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

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: 80,
            dir: PathBuf::from("."),
            do_chroot: false,
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
        }
        if cli.no_chroot {
            config.do_chroot = false;
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
            }
            "nochroot" => {
                no_value()?;
                self.do_chroot = false;
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
            "data_dir" | "symlink" | "nosymlink" | "symlinks" | "nosymlinks" => {
                return Err(format!(
                    "config option '{name}' is recognized but not supported"
                ));
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
    fn rejects_unknown_or_unsupported_options() {
        let mut unknown = tempfile::NamedTempFile::new().unwrap();
        writeln!(unknown, "mystery=value").unwrap();
        let unknown_arg = unknown.path().to_string_lossy().to_string();
        let error = ServerConfig::from_cli(&cli(&["thttpd", "-C", &unknown_arg])).unwrap_err();
        assert!(error.contains("unknown config option"));

        let mut unsupported = tempfile::NamedTempFile::new().unwrap();
        writeln!(unsupported, "data_dir=/srv/data").unwrap();
        let unsupported_arg = unsupported.path().to_string_lossy().to_string();
        let error = ServerConfig::from_cli(&cli(&["thttpd", "-C", &unsupported_arg])).unwrap_err();
        assert!(error.contains("recognized but not supported"));
    }
}
