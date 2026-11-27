use crate::config::UpstreamConfig;
use anyhow::{anyhow, Result};
use hickory_proto::op::{Message, MessageType, Query};
use hickory_proto::rr::{Name, RecordType};
use reqwest::{
    header::{ACCEPT, CONTENT_TYPE},
    StatusCode,
};
use std::{
    fmt::Display,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering::Relaxed},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::{net::UdpSocket, time::timeout};
use tracing::{debug, info, warn};

/// How long a DoH server that just failed is skipped before being tried again.
const DOWN_FOR_MS: u64 = 30_000;
/// Room for the largest possible UDP DNS message, so a big answer is never silently cut short.
const MAX_UDP: usize = 65_535;

/// Milliseconds since the upstream was created; cheap to copy into background tasks.
#[derive(Clone, Copy)]
struct Clock(Instant);

impl Clock {
    fn now_ms(self) -> u64 {
        self.0.elapsed().as_millis() as u64
    }
}

struct Server {
    url: String,
    /// Time (`Clock::now_ms`) until which this server is considered down.
    down_until: AtomicU64,
    /// Time this server last sent back any HTTP answer, i.e. was known to be up.
    last_ok: AtomicU64,
    /// A reachability check (after a timeout) is in flight.
    probing: AtomicBool,
}

impl Server {
    fn is_down(&self, clock: Clock) -> bool {
        self.down_until.load(Relaxed) > clock.now_ms()
    }

    fn answered(&self, clock: Clock) {
        self.last_ok.store(clock.now_ms(), Relaxed);
        // Back in service after being marked down?
        if self.down_until.swap(0, Relaxed) != 0 {
            info!(server = %self.url, "DoH server recovered");
        }
    }

    fn mark_down(&self, clock: Clock, error: &dyn Display) {
        let now = clock.now_ms();
        if self.down_until.swap(now + DOWN_FOR_MS, Relaxed) <= now {
            warn!(server = %self.url, %error, "DoH server failed; skipping it for 30s");
        }
    }
}

/// Why a DoH request produced no answer. Only `Failed` (and a timeout that a check confirms) says
/// the server itself is in trouble.
enum DohError {
    /// No answer in time. On its own that says nothing about the server: the name's own nameservers
    /// may just be slow, and anyone who can make us look a name up (a web page) can pick such a name.
    Timeout,
    /// The server is up but refused this particular request (HTTP 4xx).
    Rejected(StatusCode),
    /// The server couldn't be reached or misbehaved: connection, TLS, HTTP 5xx or 429, bad body.
    Failed(anyhow::Error),
}

/// Forwards raw DNS packets upstream: DoH servers in order (skipping ones that are down), then
/// plain UDP as a last resort, only once every DoH server is down.
pub struct Upstream {
    client: reqwest::Client,
    servers: Vec<Arc<Server>>,
    fallback: Vec<SocketAddr>,
    timeout: Duration,
    clock: Clock,
}

impl Upstream {
    pub fn new(cfg: &UpstreamConfig) -> Result<Self> {
        let mut b = reqwest::Client::builder()
            .pool_idle_timeout(Duration::from_secs(60))
            .user_agent(concat!("foxhole/", env!("CARGO_PKG_VERSION")));
        for s in &cfg.doh {
            if !s.ips.is_empty() {
                // Pin each DoH hostname: our own resolver must never be needed to reach upstream.
                let addrs: Vec<SocketAddr> = s.ips.iter().map(|ip| SocketAddr::new(*ip, 443)).collect();
                b = b.resolve_to_addrs(&s.host, &addrs);
            }
        }
        Ok(Self {
            client: b.build()?,
            servers: cfg
                .doh
                .iter()
                .map(|s| {
                    Arc::new(Server {
                        url: s.url.clone(),
                        down_until: AtomicU64::new(0),
                        last_ok: AtomicU64::new(0),
                        probing: AtomicBool::new(false),
                    })
                })
                .collect(),
            fallback: cfg.fallback.clone(),
            timeout: Duration::from_millis(cfg.timeout_ms.max(200)),
            clock: Clock(Instant::now()),
        })
    }

    pub async fn resolve(&self, packet: &[u8]) -> Result<Vec<u8>> {
        for s in &self.servers {
            if s.is_down(self.clock) {
                continue; // recently failed: don't make every query wait on it again
            }
            let sent = self.clock.now_ms();
            match doh(&self.client, &s.url, packet, self.timeout).await {
                Ok(r) => {
                    s.answered(self.clock);
                    return Ok(r);
                }
                Err(DohError::Rejected(status)) => {
                    s.answered(self.clock);
                    debug!(server = %s.url, %status, "DoH server refused the query");
                }
                Err(DohError::Timeout) => {
                    debug!(server = %s.url, "DoH query timed out");
                    // If anything else came back from this server meanwhile, it's up and only this
                    // name is slow. Otherwise find out, without holding up this query.
                    if s.last_ok.load(Relaxed) <= sent {
                        self.check(s);
                    }
                }
                Err(DohError::Failed(e)) => s.mark_down(self.clock, &e),
            }
        }
        // Unencrypted DNS only when DoH is really unavailable. A query that merely timed out or was
        // refused gets SERVFAIL instead: one slow name must not send every lookup out in plain text.
        if self.servers.iter().all(|s| s.is_down(self.clock)) {
            debug!("no DoH server available, using plain UDP fallback");
            return self.udp(packet).await;
        }
        Err(anyhow!("no DoH server answered in time"))
    }

    /// After a timeout, asks the server for something every resolver has at hand (the root zone's
    /// nameservers), in the background. Marks it down only if that times out or fails too.
    fn check(&self, s: &Arc<Server>) {
        if s.probing.swap(true, Relaxed) {
            return; // a check is already running
        }
        let (client, s, limit, clock) = (self.client.clone(), s.clone(), self.timeout, self.clock);
        tokio::spawn(async move {
            match doh(&client, &s.url, &probe_query(), limit).await {
                Ok(_) | Err(DohError::Rejected(_)) => s.answered(clock),
                Err(DohError::Timeout) => s.mark_down(clock, &"timed out, and so did a reachability check"),
                Err(DohError::Failed(e)) => s.mark_down(clock, &e),
            }
            s.probing.store(false, Relaxed);
        });
    }

    async fn udp(&self, packet: &[u8]) -> Result<Vec<u8>> {
        let query = Message::from_vec(packet)?;
        for &server in &self.fallback {
            match timeout(self.timeout, udp_exchange(server, packet, &query)).await {
                Ok(Ok(r)) => return Ok(r),
                Ok(Err(e)) => debug!(%server, error = %e, "plain DNS fallback failed"),
                Err(_) => debug!(%server, "plain DNS fallback timed out"),
            }
        }
        Err(anyhow!("all upstreams failed"))
    }
}

async fn doh(client: &reqwest::Client, url: &str, packet: &[u8], limit: Duration) -> Result<Vec<u8>, DohError> {
    let fail = |e: reqwest::Error| if e.is_timeout() { DohError::Timeout } else { DohError::Failed(e.into()) };
    let resp = client
        .post(url)
        .timeout(limit)
        .header(CONTENT_TYPE, "application/dns-message")
        .header(ACCEPT, "application/dns-message")
        .body(packet.to_vec())
        .send()
        .await
        .map_err(fail)?;
    let status = resp.status();
    if status.is_client_error() && status != StatusCode::TOO_MANY_REQUESTS {
        return Err(DohError::Rejected(status));
    }
    if !status.is_success() {
        return Err(DohError::Failed(anyhow!("HTTP {status}")));
    }
    let bytes = resp.bytes().await.map_err(fail)?;
    if bytes.len() < 12 {
        return Err(DohError::Failed(anyhow!("short DoH response")));
    }
    Ok(bytes.to_vec())
}

/// `. NS`: answered straight from any resolver's cache, so a slow reply means a slow server.
fn probe_query() -> Vec<u8> {
    let mut m = Message::query();
    m.metadata.id = 0; // RFC 8484 section 4.1
    m.metadata.recursion_desired = true;
    m.add_query(Query::query(Name::root(), RecordType::NS));
    m.to_vec().unwrap_or_default()
}

/// One query over plain UDP. The socket is connected, so the kernel drops datagrams from anywhere
/// but `server`, and a reply only counts if it answers this query (same ID and question). Anything
/// else is ignored, and we keep waiting for the real answer.
async fn udp_exchange(server: SocketAddr, packet: &[u8], query: &Message) -> Result<Vec<u8>> {
    let local: SocketAddr = match server {
        SocketAddr::V4(_) => (Ipv4Addr::UNSPECIFIED, 0).into(),
        SocketAddr::V6(_) => (Ipv6Addr::UNSPECIFIED, 0).into(),
    };
    let sock = UdpSocket::bind(local).await?;
    sock.connect(server).await?;
    sock.send(packet).await?;
    let mut buf = vec![0u8; MAX_UDP];
    loop {
        let n = sock.recv(&mut buf).await?;
        if answers(query, &buf[..n]) {
            buf.truncate(n);
            return Ok(buf);
        }
        debug!(%server, "ignored a UDP reply that doesn't match the query");
    }
}

/// Whether `wire` is a reply to `query`: same ID, and the same question (names compare
/// case-insensitively).
fn answers(query: &Message, wire: &[u8]) -> bool {
    let Ok(resp) = Message::from_vec(wire) else { return false };
    resp.message_type == MessageType::Response
        && resp.id == query.id
        && resp.queries.len() == query.queries.len()
        && resp.queries.iter().zip(&query.queries).all(|(a, b)| {
            a.name() == b.name() && a.query_type() == b.query_type() && a.query_class() == b.query_class()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DohServer;
    use hickory_proto::op::OpCode;
    use hickory_proto::rr::{rdata::A, RData, Record};
    use std::str::FromStr;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
    };

    const REAL: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 1);
    const FORGED: Ipv4Addr = Ipv4Addr::new(203, 0, 113, 66);

    fn query(name: &str, id: u16) -> Message {
        let mut m = Message::new(id, MessageType::Query, OpCode::Query);
        m.metadata.recursion_desired = true;
        m.add_query(Query::query(Name::from_str(name).unwrap(), RecordType::A));
        m
    }

    fn reply(q: &Message, id: u16, ip: Ipv4Addr) -> Vec<u8> {
        let mut r = Message::new(id, MessageType::Response, OpCode::Query);
        r.add_queries(q.queries.iter().cloned());
        r.add_answer(Record::from_rdata(q.queries[0].name().clone(), 60, RData::A(A(ip))));
        r.to_vec().unwrap()
    }

    fn first_a(wire: &[u8]) -> Ipv4Addr {
        match Message::from_vec(wire).unwrap().answers[0].data {
            RData::A(A(ip)) => ip,
            ref other => panic!("unexpected answer {other:?}"),
        }
    }

    fn upstream(doh: &[&str], fallback: &[SocketAddr]) -> Upstream {
        Upstream::new(&UpstreamConfig {
            timeout_ms: 300,
            fallback: fallback.to_vec(),
            doh: doh.iter().map(|u| DohServer { url: u.to_string(), host: "127.0.0.1".into(), ips: vec![] }).collect(),
        })
        .unwrap()
    }

    /// A plain-DNS server that answers every query correctly.
    async fn udp_server(addr: &str) -> Option<SocketAddr> {
        let sock = UdpSocket::bind(addr).await.ok()?;
        let local = sock.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            while let Ok((n, peer)) = sock.recv_from(&mut buf).await {
                let q = Message::from_vec(&buf[..n]).unwrap();
                let _ = sock.send_to(&reply(&q, q.id, REAL), peer).await;
            }
        });
        Some(local)
    }

    /// A plain-HTTP DoH server that answers every query with `REAL`, except names starting with
    /// `slow.`, which it never answers.
    async fn doh_server() -> String {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/dns-query", l.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((s, _)) = l.accept().await {
                tokio::spawn(serve_http(s));
            }
        });
        url
    }

    async fn serve_http(mut s: TcpStream) -> std::io::Result<()> {
        let mut buf = Vec::new();
        loop {
            let end = loop {
                if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break i + 4;
                }
                let mut chunk = [0u8; 4096];
                let n = s.read(&mut chunk).await?;
                if n == 0 {
                    return Ok(());
                }
                buf.extend_from_slice(&chunk[..n]);
            };
            let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
            let len: usize =
                head.lines().find_map(|l| l.strip_prefix("content-length:")).map_or(0, |v| v.trim().parse().unwrap());
            while buf.len() < end + len {
                let mut chunk = [0u8; 4096];
                let n = s.read(&mut chunk).await?;
                if n == 0 {
                    return Ok(());
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            let q = Message::from_vec(&buf[end..end + len]).unwrap();
            buf.drain(..end + len);
            if q.queries[0].name().to_ascii().starts_with("slow.") {
                std::future::pending::<()>().await;
            }
            let body = reply(&q, q.id, REAL);
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/dns-message\r\ncontent-length: {}\r\n\r\n",
                body.len()
            );
            s.write_all(head.as_bytes()).await?;
            s.write_all(&body).await?;
        }
    }

    /// Waits for any background reachability checks to finish.
    async fn checks_done(u: &Upstream) {
        for _ in 0..100 {
            if u.servers.iter().all(|s| !s.probing.load(Relaxed)) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("reachability check never finished");
    }

    #[tokio::test]
    async fn udp_fallback_ignores_spoofed_and_mismatched_replies() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server.local_addr().unwrap();
        let spoofer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            let (n, client) = server.recv_from(&mut buf).await.unwrap();
            let q = Message::from_vec(&buf[..n]).unwrap();
            // A forged answer from another address, a reply to some other query, an answer to a
            // different question, and only then the real answer.
            spoofer.send_to(&reply(&q, q.id, FORGED), client).await.unwrap();
            server.send_to(&reply(&q, q.id.wrapping_add(1), FORGED), client).await.unwrap();
            server.send_to(&reply(&query("other.example.", q.id), q.id, FORGED), client).await.unwrap();
            server.send_to(&reply(&q, q.id, REAL), client).await.unwrap();
        });
        let u = upstream(&[], &[server_addr]);
        let wire = u.resolve(&query("www.example.", 0x5151).to_vec().unwrap()).await.unwrap();
        assert_eq!(first_a(&wire), REAL);
    }

    #[tokio::test]
    async fn udp_fallback_reaches_ipv6_servers() {
        let Some(server) = udp_server("[::1]:0").await else {
            eprintln!("no IPv6 loopback here; skipping");
            return;
        };
        let u = upstream(&[], &[server]);
        let wire = u.resolve(&query("www.example.", 7).to_vec().unwrap()).await.unwrap();
        assert_eq!(first_a(&wire), REAL);
    }

    #[tokio::test]
    async fn slow_name_does_not_downgrade_to_plain_dns() {
        // A fallback that would answer, so using it would make the slow query succeed.
        let fallback = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let u = upstream(&[&doh_server().await], &[fallback.local_addr().unwrap()]);

        assert!(u.resolve(&query("slow.example.", 1).to_vec().unwrap()).await.is_err());
        checks_done(&u).await;
        assert!(!u.servers[0].is_down(u.clock), "a responsive server must stay up");

        let wire = u.resolve(&query("www.example.", 2).to_vec().unwrap()).await.unwrap();
        assert_eq!(first_a(&wire), REAL);
        let mut buf = [0u8; 512];
        assert!(fallback.try_recv_from(&mut buf).is_err(), "nothing may go out as plain DNS");
    }

    #[tokio::test]
    async fn unreachable_doh_server_falls_back_to_plain_dns() {
        // A port nothing listens on: the connection is refused straight away.
        let closed = TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap();
        let fallback = udp_server("127.0.0.1:0").await.unwrap();
        let u = upstream(&[&format!("http://{closed}/dns-query")], &[fallback]);

        let wire = u.resolve(&query("www.example.", 3).to_vec().unwrap()).await.unwrap();
        assert_eq!(first_a(&wire), REAL);
        assert!(u.servers[0].is_down(u.clock));
    }

    #[tokio::test]
    async fn silent_doh_server_is_marked_down_after_a_failed_check() {
        // Accepts connections but never answers, like a network that drops HTTPS.
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/dns-query", l.local_addr().unwrap());
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((s, _)) = l.accept().await {
                held.push(s);
            }
        });
        let fallback = udp_server("127.0.0.1:0").await.unwrap();
        let u = upstream(&[&url], &[fallback]);

        assert!(u.resolve(&query("www.example.", 4).to_vec().unwrap()).await.is_err());
        checks_done(&u).await;
        assert!(u.servers[0].is_down(u.clock));
        let wire = u.resolve(&query("www.example.", 5).to_vec().unwrap()).await.unwrap();
        assert_eq!(first_a(&wire), REAL);
    }

    #[test]
    fn answers_requires_matching_id_and_question() {
        let q = query("www.example.", 9);
        assert!(answers(&q, &reply(&q, 9, REAL)));
        assert!(answers(&q, &reply(&query("WWW.Example.", 9), 9, REAL)), "case doesn't matter");
        assert!(!answers(&q, &reply(&q, 10, REAL)));
        assert!(!answers(&q, &reply(&query("evil.example.", 9), 9, REAL)));
        assert!(!answers(&q, &q.to_vec().unwrap()), "a query is not an answer");
        assert!(!answers(&q, b"garbage"));
    }
}
