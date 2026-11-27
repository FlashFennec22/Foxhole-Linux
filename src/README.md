# Foxhole for Linux (draft)

Local DNS ad blocker for Linux systems that use systemd-resolved. Ingests AdGuard-style (`||domain^`),
hosts and plain-domain lists; defaults to **Hagezi Pro++**. Upstream DNS is DoH (Cloudflare Security)
with a plain-UDP fallback.

## Build
    cargo test
    cargo build --release          # target/release/foxhole
    sudo packaging/install.sh      # installs to /usr/local (PREFIX=... to change) and turns it on
    foxhole uninstall [--purge]    # runs the uninstall.sh it installed in /usr/local/lib/foxhole

TLS is rustls, so no OpenSSL headers are needed. Certificates are checked against the system trust store.

## Releases
Users install with `curl -fsSL https://raw.githubusercontent.com/FlashFennec22/Foxhole-Linux/main/install.sh | sh`.
That script downloads `foxhole-<arch>-linux.tar.gz` and `SHA256SUMS` from the latest GitHub release, checks the
tarball against them, unpacks it and runs the `install.sh` inside it (`packaging/install.sh`) through sudo.
It needs the repository to be public. Testing it against a local build:

    CC_x86_64_unknown_linux_musl=gcc packaging/dist.sh x86_64   # dist/foxhole-x86_64-linux.tar.gz (static)
    (cd dist && sha256sum *.tar.gz > SHA256SUMS)
    FOXHOLE_BASE_URL=file://$PWD/dist sh install.sh

To publish a release:
1. Bump `version` in `Cargo.toml` and commit it to `main`.
2. `git tag vX.Y.Z && git push origin vX.Y.Z`. `.github/workflows/release.yml` checks the tag matches
   Cargo.toml, runs the tests, builds static x86_64 and aarch64 binaries (each on its own runner) and publishes
   the release with the tarballs and `SHA256SUMS`.

The checksums catch damaged or swapped downloads, but they come from the same place as the script, so whoever
controls this repository controls what gets installed. Signing releases (as the Windows updater does) would only
help once something already installed checks the signatures, such as a future updater.

## Use
    foxhole on           # turn on, now and at boot (also restarts it, to apply config.toml changes)
    foxhole off          # turn off and give DNS back to the network's servers
    foxhole status
    sudo foxhole run     # foreground debug mode: also points resolved at it until Ctrl+C
    sudo foxhole run --no-system-dns   # just listen on 127.0.0.1:53, don't touch resolved

`on`/`off` go through systemctl, which asks for the admin password (polkit) when needed.
Logs: `journalctl -u foxhole`. Config: `/etc/foxhole/config.toml` (root-writable only; the service just
reads it). Blocklist cache and `stats.json`: `/var/lib/foxhole/`. Outside systemd, the `CONFIGURATION_DIRECTORY`
and `STATE_DIRECTORY` environment variables override those two paths.

## How it works
1. `foxhole.service` (Type=notify) runs `foxhole service` as the unprivileged `foxhole` user, with only
   CAP_NET_BIND_SERVICE and a tight sandbox. It listens on 127.0.0.1:53 and [::1]:53 (UDP+TCP).
2. Blocked names (domain + all subdomains) get 0.0.0.0 / :: (A / AAAA) or an empty NOERROR; everything else is forwarded upstream.
   Upstream answers are kept in a 2 MB in-memory cache (least recently used dropped first) for as long as their TTL
   allows, capped at 1 day (1 hour for "no such name"). Hit/miss counts go to `stats.json`.
3. Once the resolver reports ready, `ExecStartPost=+foxhole dns-apply` (as root) writes
   `/run/systemd/resolved.conf.d/50-foxhole.conf` (`DNS=127.0.0.1`, `Domains=~.`), reloads resolved and flushes its
   cache. Every program asks resolved, and resolved now asks Foxhole. Only names under a link's own search domain
   (e.g. `printer.lan`) still go to the network's DNS. Network changes need no handling: the setting is global.
4. Stopping, crashing or failing to start runs `foxhole dns-restore`, which removes the drop-in, so the machine
   never points at a resolver that isn't there. The drop-in is in /run, so it can't outlive a reboot either.
5. A failed start is retried after 10 s, growing to 2 minutes; after 5 failures in 10 minutes systemd gives up
   and `foxhole status` says so until `foxhole on`.
6. Lists refresh every 24h; the last good cache is kept if a download fails.

## Known gaps
- Only systemd-resolved is supported.
- A link that sets the `~.` routing domain itself (some full-tunnel VPNs) ties with Foxhole, and resolved then
  asks both, so lookups over that VPN aren't all filtered.
- Programs that skip resolved (browsers with their own "secure DNS", anything with a hardcoded server) aren't filtered.
- No tray icon, packages or updater yet.

## Relationship to the Windows version
This repository started as a copy of [FlashFennec22/Foxhole](https://github.com/FlashFennec22/Foxhole) with its
history; the Windows-only code (service, tray, installer, updater, ACL handling) was removed. The DNS engine
(`dns.rs`, `engine.rs`, `rules.rs`, `lists.rs`, `cache.rs`, `upstream.rs`) is unchanged, so fixes to it can be merged:

    git fetch upstream
    git merge upstream/main     # expect conflicts only where upstream changed the files replaced or removed here

The `upstream` remote is fetch-only (its push URL is set to `DISABLED`).
