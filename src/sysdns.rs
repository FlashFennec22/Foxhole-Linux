//! Points systemd-resolved at Foxhole, and takes it back out again.
//!
//! On a systemd-resolved system every program asks resolved (through 127.0.0.53 or nss-resolve),
//! and resolved asks the DNS servers it has been given. A drop-in in /run/systemd/resolved.conf.d
//! makes Foxhole (127.0.0.1) the global DNS server with the catch-all routing domain `~.`, so
//! resolved sends lookups to it rather than to the per-link servers from DHCP or NetworkManager.
//! Only names under a link's own search domains (e.g. `printer.lan`) still go to that link.
//!
//! Unlike Windows there's nothing per adapter to save, restore or watch: the global setting covers
//! Wi-Fi, VPN and docking changes. /run is emptied at boot, so a crash can't outlive a reboot.
//!
//! Known gap: a link that sets `~.` itself (some full-tunnel VPNs) ties with ours, and resolved
//! then asks both, so lookups over that VPN aren't all filtered.

use anyhow::{bail, Context, Result};
use std::{fs, io, path::Path, process::Command};
use tracing::{info, warn};

const DROP_IN_DIR: &str = "/run/systemd/resolved.conf.d";
const DROP_IN: &str = "/run/systemd/resolved.conf.d/50-foxhole.conf";
const CONTENT: &str = "# Written by Foxhole while protection is on, and removed when it stops.\n\
                       [Resolve]\nDNS=127.0.0.1\nDomains=~.\n";

/// Whether resolved is currently pointed at us (the drop-in is in place).
pub fn is_applied() -> bool {
    Path::new(DROP_IN).exists()
}

/// Points resolved at 127.0.0.1. Cheap when already applied, so the engine can call it every few
/// minutes to put the drop-in back if something removed it. Only 127.0.0.1 is set: resolved is our
/// only client, so the [::1] listener isn't needed for it.
pub fn apply(_ipv6: bool) -> Result<()> {
    if fs::read_to_string(DROP_IN).is_ok_and(|c| c == CONTENT) {
        return Ok(());
    }
    if !systemctl(&["is-active", "--quiet", "systemd-resolved.service"]) {
        bail!("systemd-resolved isn't running; Foxhole only supports systems that use it for now");
    }
    fs::create_dir_all(DROP_IN_DIR).with_context(|| format!("creating {DROP_IN_DIR}"))?;
    let tmp = format!("{DROP_IN}.tmp");
    fs::write(&tmp, CONTENT).with_context(|| format!("writing {tmp}"))?;
    fs::rename(&tmp, DROP_IN).with_context(|| format!("creating {DROP_IN}"))?;
    reload_resolved(Wait::Yes).context("reloading systemd-resolved")?;
    info!("systemd-resolved now forwards to Foxhole");
    Ok(())
}

/// Takes the drop-in away so resolved goes back to the network's own DNS servers.
pub fn restore() -> Result<()> {
    match fs::remove_file(DROP_IN) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("removing {DROP_IN}")),
    }
    // While the system shuts down, resolved is about to stop anyway; asking it to reload could get
    // in the way of that. /run is emptied before the next boot.
    let output = Command::new("systemctl").arg("is-system-running").output();
    if output.is_ok_and(|o| o.stdout.starts_with(b"stopping")) {
        return Ok(());
    }
    // Don't wait for the reload: this runs from foxhole.service's ExecStop/ExecStopPost, and
    // systemd queues resolved's reload behind our stop job (we're After= it, and stops go first).
    // Waiting would deadlock until systemd killed us at the stop timeout, 90 s by default.
    reload_resolved(Wait::No).context("reloading systemd-resolved")?;
    info!("systemd-resolved restored to the network's DNS servers");
    Ok(())
}

#[derive(PartialEq)]
enum Wait {
    Yes,
    No,
}

/// Makes resolved re-read its config, which also empties its cache, so answers from before the
/// change (blocked or not) aren't served from it. `--job-mode=fail` so we never cancel a job
/// systemd already has queued for resolved (a stop, say). With `Wait::No` the reload is only
/// queued, and runs as soon as systemd gets to it.
fn reload_resolved(wait: Wait) -> Result<()> {
    let mut args = vec!["--job-mode=fail", "try-reload-or-restart", "systemd-resolved.service"];
    if wait == Wait::No {
        args.insert(0, "--no-block");
    }
    if !systemctl(&args) {
        bail!("systemctl reload failed");
    }
    // The reload (or restart) already empties the cache; this is belt and braces. It only helps once
    // the reload has happened: before that, resolved would just refill it from the old server.
    if wait == Wait::Yes {
        match Command::new("resolvectl").arg("flush-caches").status() {
            Ok(s) if s.success() => {}
            _ => warn!("could not flush systemd-resolved's cache; old answers may linger for their TTL"),
        }
    }
    Ok(())
}

fn systemctl(args: &[&str]) -> bool {
    Command::new("systemctl").args(args).status().is_ok_and(|s| s.success())
}

/// Network change notifications. Never fires for now: resolved follows network changes itself, so
/// this would only be needed to clear our answer cache when moving networks (it ages out by TTL).
pub struct Watcher(tokio::sync::Notify);

impl Watcher {
    pub fn new() -> Result<Self> {
        Ok(Self(tokio::sync::Notify::new()))
    }

    pub async fn changed(&self) {
        self.0.notified().await
    }
}
