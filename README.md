# Foxhole for Linux (experimental)

An experimental Linux port of [Foxhole](https://github.com/FlashFennec22/Foxhole), a Rust-based system-level ad blocker. The Windows version lives in that repository; this one shares its DNS engine and replaces everything Windows-specific.

## How does it work?

Foxhole checks your outgoing DNS requests against an always up-to-date blocklist. If the connection points at a known advertising domain, Foxhole simply refuses the connection so the ad doesn't load. DNS requests are routed to Cloudflare Security (1.1.1.2, which also filters known malware and phishing domains) over HTTPS by default. If Cloudflare goes down, Foxhole temporarily falls back to Quad9. If DNS-Over-HTTPS fails, it falls back to basic UDP. Blocking is provided by the Hagezi Pro++ blocklist, which attempts to block known trackers and malware domains in addition to advertisements.

On Linux, Foxhole runs as a systemd service and plugs into systemd-resolved, which most current desktop distributions (Ubuntu, Pop!_OS, Fedora and others) already use for DNS.

## Does it work on everything?

Only most things. Sites or apps which serve their own video ads (Such as Youtube, Twitch) still get through as there's not much that can be done about these at the DNS level. The vast majority of banner ads and trackers should be stopped, however.

It needs systemd-resolved for now. Systems that manage `/etc/resolv.conf` some other way aren't supported yet.

## What should I be aware of?

Content blocking of any kind implies a risk of bumping into false positives. You might also find that sponsored links on some shopping sites are non-responsive, if they show at all. If you're encountering unexpected connection issues, turn protection off with `foxhole off`. If ads aren't being blocked as expected, go to your browser's DNS settings and ensure that it's set to use system DNS (not "secure DNS" with its own provider).

## How do I install it?

Paste this into a terminal (it asks for your password, then turns protection on):

    curl -fsSL https://raw.githubusercontent.com/FlashFennec22/Foxhole-Linux/main/install.sh | sh

It downloads the latest release for your machine (x86_64 or ARM64), checks it against the release's checksums and installs it to `/usr/local`. Run it again to upgrade; your settings are kept. Prefer to read it first? Download [install.sh](install.sh), look it over, then run `sh install.sh`.

To build it yourself instead (needs a Rust toolchain):

    cargo build --release
    sudo packaging/install.sh

## How do I turn it off or check on it?

    foxhole off       # turn protection off (stays off across reboots)
    foxhole on        # turn it back on
    foxhole status

## How do I uninstall it?

    foxhole uninstall            # keeps /etc/foxhole/config.toml
    foxhole uninstall --purge    # removes that too

## Thanks and considerations

Thanks to my longtime friend RocketKnight1 for his improvements upon my initial implementation and to the Hagezi team whose tireless work makes the internet a better place for all of us.
