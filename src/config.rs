use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

pub const APP_NAME: &str = "Foxhole";

/// Blocklist cache and stats.json: /var/lib/foxhole, or the StateDirectory systemd hands the service.
pub fn data_dir() -> PathBuf {
    systemd_dir("STATE_DIRECTORY", "/var/lib")
}

/// config.toml: /etc/foxhole, or the ConfigurationDirectory systemd hands the service. Only root
/// can write there; the service itself only reads it.
pub fn config_dir() -> PathBuf {
    systemd_dir("CONFIGURATION_DIRECTORY", "/etc")
}

/// systemd sets these variables to a colon-separated list; we only ever ask for one directory.
fn systemd_dir(var: &str, base: &str) -> PathBuf {
    std::env::var_os(var)
        .and_then(|v| v.to_str().and_then(|s| s.split(':').next()).filter(|s| !s.is_empty()).map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(base).join(APP_NAME.to_ascii_lowercase()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// How often to re-download blocklists.
    pub update_interval_hours: u64,
    pub lists: Vec<ListSource>,
    pub upstream: UpstreamConfig,
    /// Domains (and their subdomains) that are never blocked.
    pub allow: Vec<String>,
    /// Extra domains (and their subdomains) to always block.
    pub block: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListSource {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub mirrors: Vec<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UpstreamConfig {
    /// Per-attempt timeout for each upstream, in milliseconds.
    pub timeout_ms: u64,
    /// Plain-UDP last resort if every DoH server is down. Set to [] to never fall back to unencrypted DNS.
    pub fallback: Vec<SocketAddr>,
    /// DNS-over-HTTPS servers, tried in order. Servers that fail are skipped for 30 s.
    pub doh: Vec<DohServer>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DohServer {
    pub url: String,
    /// Hostname in `url`; pinned to `ips` so we never need DNS to reach DNS.
    pub host: String,
    pub ips: Vec<IpAddr>,
}

fn yes() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            update_interval_hours: 24,
            lists: vec![ListSource {
                name: "hagezi-pro-plus".into(),
                url: "https://cdn.jsdelivr.net/gh/hagezi/dns-blocklists@latest/adblock/pro.plus.txt".into(),
                mirrors: vec![
                    "https://raw.githubusercontent.com/hagezi/dns-blocklists/main/adblock/pro.plus.txt".into(),
                ],
                enabled: true,
            }],
            upstream: UpstreamConfig::default(),
            allow: vec![],
            block: vec![],
        }
    }
}

impl Default for UpstreamConfig {
    fn default() -> Self {
        Self {
            timeout_ms: 1500,
            fallback: vec!["1.1.1.2:53".parse().unwrap(), "9.9.9.9:53".parse().unwrap()],
            doh: vec![
                // Cloudflare for Families "Security": filters known malware/phishing domains.
                DohServer {
                    url: "https://security.cloudflare-dns.com/dns-query".into(),
                    host: "security.cloudflare-dns.com".into(),
                    ips: vec!["1.1.1.2".parse().unwrap(), "1.0.0.2".parse().unwrap()],
                },
                DohServer {
                    url: "https://dns.quad9.net/dns-query".into(),
                    host: "dns.quad9.net".into(),
                    ips: vec!["9.9.9.9".parse().unwrap(), "149.112.112.112".parse().unwrap()],
                },
            ],
        }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        config_dir().join("config.toml")
    }

    /// A commented config.toml with every setting at its default, for the installer to write out.
    pub fn default_toml() -> Result<String> {
        let header = "# Foxhole configuration. Run `foxhole on` after editing to apply it.\n\
                      # Add more AdGuard-style / hosts / domain lists as extra [[lists]] entries.\n\n";
        Ok(format!("{header}{}", toml::to_string_pretty(&Config::default())?))
    }

    /// Reads config.toml. If there is none, uses the defaults, and writes them out when allowed
    /// to (the service can't write to /etc/foxhole; the installer writes the file instead).
    pub fn load_or_create() -> Result<Self> {
        std::fs::create_dir_all(data_dir()).context("creating data directory")?;
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(text) => return toml::from_str(&text).with_context(|| format!("parsing {}", path.display())),
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(e).with_context(|| format!("reading {}", path.display()));
            }
            Err(_) => {}
        }
        let text = Self::default_toml()?;
        let written = std::fs::create_dir_all(config_dir()).and_then(|()| std::fs::write(&path, text));
        if written.is_err() {
            tracing::info!(path = %path.display(), "no config file; using the defaults");
        }
        Ok(Config::default())
    }
}
