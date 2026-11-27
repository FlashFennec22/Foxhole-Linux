mod cache;
mod config;
mod dns;
mod engine;
mod lists;
mod rules;
mod sysdns;
mod upstream;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use config::Config;
use std::{collections::HashMap, ffi::OsString, path::Path, process::Command, sync::Arc};
use tokio::sync::Notify;

/// The systemd unit installed by packaging/install.sh.
const UNIT: &str = "foxhole.service";

#[derive(Parser)]
#[command(name = "foxhole", version, about = "System-wide DNS ad blocker (AdGuard-style lists; Hagezi Pro++ by default)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Turn protection on, now and at every boot (also restarts it, to apply config changes)
    On,
    /// Turn protection off and give DNS back to the network's own servers
    Off,
    /// Show whether protection is running
    Status,
    /// Turn protection off and remove Foxhole (asks for the admin password)
    Uninstall {
        /// Also remove /etc/foxhole/config.toml and the foxhole system user
        #[arg(long)]
        purge: bool,
    },
    /// Run in the foreground for debugging (Ctrl+C to stop). Needs root to listen on port 53.
    Run {
        /// Don't touch systemd-resolved; just listen on 127.0.0.1:53
        #[arg(long)]
        no_system_dns: bool,
    },
    /// Internal: the resolver itself, started by foxhole.service
    #[command(hide = true)]
    Service,
    /// Internal (foxhole.service, as root): point systemd-resolved at Foxhole
    #[command(hide = true)]
    DnsApply,
    /// Internal (foxhole.service, as root): point systemd-resolved back at the network's DNS
    #[command(hide = true)]
    DnsRestore,
    /// Internal (installer): print a config.toml with the default settings
    #[command(hide = true)]
    DefaultConfig,
}

/// Logs to stderr; under systemd that ends up in the journal (`journalctl -u foxhole`).
fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let under_systemd = std::env::var_os("INVOCATION_ID").is_some();
    // The journal timestamps every line itself.
    let fmt = tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr);
    if under_systemd {
        fmt.with_ansi(false).without_time().init();
    } else {
        fmt.init();
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Run { no_system_dns } => resolver(!no_system_dns),
        Cmd::Service => resolver(false),
        Cmd::DnsApply => {
            init_logging();
            sysdns::apply(false)
        }
        Cmd::DnsRestore => {
            init_logging();
            sysdns::restore()
        }
        Cmd::DefaultConfig => {
            print!("{}", Config::default_toml()?);
            Ok(())
        }
        Cmd::On => turn_on(),
        Cmd::Off => turn_off(),
        Cmd::Uninstall { purge } => uninstall(purge),
        Cmd::Status => {
            println!("{}", status());
            Ok(())
        }
    }
}

/// Runs the resolver until SIGTERM or Ctrl+C. As the service, systemd (not us) points resolved
/// at it, from foxhole.service's ExecStartPost, once we report ready.
fn resolver(manage_dns: bool) -> Result<()> {
    init_logging();
    let cfg = Config::load_or_create()?;
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async {
        let shutdown = Arc::new(Notify::new());
        let s2 = shutdown.clone();
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::spawn(async move {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            s2.notify_one();
        });
        engine::run(cfg, shutdown, manage_dns, || sd_notify("READY=1")).await
    })
}

/// Tells systemd (Type=notify) how we're doing. Does nothing when not started by systemd.
fn sd_notify(state: &str) {
    use std::os::{linux::net::SocketAddrExt, unix::net::{SocketAddr, UnixDatagram}};
    let Some(path) = std::env::var_os("NOTIFY_SOCKET") else { return };
    let path = path.to_string_lossy();
    let addr = match path.strip_prefix('@') {
        Some(name) => SocketAddr::from_abstract_name(name),
        None => SocketAddr::from_pathname(&*path),
    };
    let sent = addr.and_then(|a| UnixDatagram::unbound()?.send_to_addr(state.as_bytes(), &a));
    if let Err(e) = sent {
        tracing::warn!(error = %e, "could not notify systemd");
    }
}

/// Runs systemctl in the foreground, so it can ask for the admin password (polkit) if needed.
fn systemctl(args: &[&str]) -> Result<()> {
    let status = Command::new("systemctl").args(args).status()?;
    if !status.success() {
        bail!("`systemctl {}` failed", args.join(" "));
    }
    Ok(())
}

/// The unit's LoadState / ActiveState / SubState.
fn unit_state() -> HashMap<String, String> {
    let out = Command::new("systemctl")
        .args(["show", UNIT, "--property=LoadState,ActiveState,SubState"])
        .output();
    let text = out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    text.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.to_owned(), v.to_owned())).collect()
}

fn installed() -> bool {
    unit_state().get("LoadState").is_some_and(|s| s == "loaded")
}

fn turn_on() -> Result<()> {
    if !installed() {
        bail!("Foxhole isn't installed ({UNIT} not found); run packaging/install.sh first");
    }
    // A restart rather than a start, so config.toml changes apply. reset-failed lets it start
    // again after it gave up (too many failed starts in a row).
    let _ = Command::new("systemctl").args(["reset-failed", UNIT]).output();
    let r = systemctl(&["enable", UNIT]).and_then(|()| systemctl(&["restart", UNIT]));
    match r {
        Ok(()) => println!("Foxhole is ON. Ads and trackers are now blocked system-wide."),
        Err(e) => bail!("could not start Foxhole ({e:#}); see `journalctl -u foxhole`"),
    }
    Ok(())
}

fn turn_off() -> Result<()> {
    if installed() {
        systemctl(&["disable", "--now", UNIT])?;
    }
    // The service takes itself out of resolved's settings when it stops. If something went wrong
    // there (or a `foxhole run` was killed), say how to fix it rather than leave DNS broken.
    if sysdns::is_applied() {
        bail!("DNS still points at Foxhole; run `sudo foxhole dns-restore` to undo that");
    }
    println!("Foxhole is OFF. DNS is back to your network's own servers.");
    Ok(())
}

/// Runs the uninstall.sh that the installer put in <prefix>/lib/foxhole, through sudo unless
/// we're already root.
fn uninstall(purge: bool) -> Result<()> {
    let exe = std::env::current_exe()?;
    let prefix = exe.parent().and_then(|bin| bin.parent()).ok_or_else(|| anyhow::anyhow!("unexpected install location"))?;
    let script = prefix.join("lib/foxhole/uninstall.sh");
    if !script.exists() {
        bail!("{} not found; was Foxhole installed with its install script?", script.display());
    }
    let is_root = Command::new("id").arg("-u").output().is_ok_and(|o| o.stdout.trim_ascii() == b"0");
    if !uninstall_command(&script, prefix, purge, is_root).status()?.success() {
        bail!("uninstall failed");
    }
    Ok(())
}

/// `[sudo] env PREFIX=<prefix> sh <script> [--purge]`. PREFIX goes on the command line rather than
/// in our environment because sudo starts commands with a clean environment, which would drop it
/// and make uninstall.sh look in /usr/local whatever the real prefix is.
fn uninstall_command(script: &Path, prefix: &Path, purge: bool, is_root: bool) -> Command {
    let mut assign = OsString::from("PREFIX=");
    assign.push(prefix);
    let mut cmd = if is_root {
        Command::new("env")
    } else {
        let mut c = Command::new("sudo");
        c.arg("env");
        c
    };
    cmd.arg(assign).arg("sh").arg(script);
    if purge {
        cmd.arg("--purge");
    }
    cmd
}

fn status() -> &'static str {
    let s = unit_state();
    let get = |k: &str| s.get(k).map(String::as_str).unwrap_or("");
    match (get("LoadState"), get("ActiveState"), get("SubState")) {
        ("loaded", "active", _) => "ON (service running)",
        ("loaded", "activating", "auto-restart") => "OFF (couldn't start; trying again shortly)",
        ("loaded", "activating" | "deactivating" | "reloading", _) => "starting/stopping",
        ("loaded", "failed", _) => "OFF (couldn't start; see `journalctl -u foxhole`, `foxhole on` to retry)",
        ("loaded", _, _) => "OFF (service stopped)",
        _ => "not installed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(cmd: &Command) -> Vec<String> {
        std::iter::once(cmd.get_program()).chain(cmd.get_args()).map(|a| a.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn uninstall_passes_prefix_through_sudo() {
        let (script, prefix) = (Path::new("/opt/fh/lib/foxhole/uninstall.sh"), Path::new("/opt/fh"));
        let cmd = uninstall_command(script, prefix, true, false);
        assert_eq!(args(&cmd), ["sudo", "env", "PREFIX=/opt/fh", "sh", "/opt/fh/lib/foxhole/uninstall.sh", "--purge"]);
        assert_eq!(cmd.get_envs().count(), 0, "sudo would drop it from the environment");

        let cmd = uninstall_command(script, prefix, false, true);
        assert_eq!(args(&cmd), ["env", "PREFIX=/opt/fh", "sh", "/opt/fh/lib/foxhole/uninstall.sh"]);
    }
}
