//! HTTP proxies, for networks where nothing leaves without one.
//!
//! Which proxy reaches the server:
//!
//! - `proxy` in `config.toml`, when set. `"none"` connects directly. The
//!   environment is ignored then, `NO_PROXY` included: services started by
//!   systemd, launchd or the Task Scheduler don't see a shell's variables,
//!   so the config file is the setting that always applies.
//! - Otherwise the environment, as curl reads it: `https_proxy` or
//!   `HTTPS_PROXY` for an `https` server (`http_proxy` or `HTTP_PROXY` for a
//!   plain `http` development server), then `all_proxy` or `ALL_PROXY`,
//!   unless `no_proxy` or `NO_PROXY` matches the server.
//!
//! Both the pairing requests and the WebSocket go through the proxy with
//! HTTP `CONNECT`, so TLS still runs end to end between `cww` and the
//! server: the proxy sees the host name and nothing else. Credentials in the
//! proxy URL (`http://user:password@proxy:3128`) are sent as Basic auth. An
//! `https://` proxy URL wraps the connection to the proxy in TLS as well.
//!
//! The password never appears in status output, logs or errors; see
//! [`Proxy::redacted`].

use std::io;
use std::net::IpAddr;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use percent_encoding::percent_decode_str;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use url::{Host, Url};

use crate::auth::client::ServerUrl;

/// The `proxy` value that turns proxies off, whatever the environment says.
pub const NONE: &str = "none";

/// Longest `CONNECT` response header accepted from a proxy.
const MAX_RESPONSE_HEAD: usize = 16 * 1024;

/// A proxy to reach the server through.
#[derive(Clone, PartialEq, Eq)]
pub struct Proxy {
    secure: bool,
    /// Bare host: no brackets around an IPv6 address.
    host: String,
    port: u16,
    username: String,
    password: Option<String>,
    source: String,
}

/// What `cww status` shows about the proxy in use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyInfo {
    /// The proxy URL, without its password.
    pub url: String,
    /// `config` or the environment variable it came from.
    pub source: String,
}

impl std::fmt::Debug for Proxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Proxy({} from {})", self.redacted(), self.source)
    }
}

impl Proxy {
    /// Parse a proxy URL. `source` says where it came from, for messages.
    /// Errors never repeat the URL, which may hold a password.
    pub fn parse(input: &str, source: &str) -> Result<Self> {
        let input = input.trim();
        let with_scheme = if input.contains("://") {
            input.to_string()
        } else {
            format!("http://{input}")
        };
        let url = Url::parse(&with_scheme)
            .map_err(|e| anyhow!("the proxy in {source} is not a valid URL ({e})"))?;
        let secure = match url.scheme() {
            "http" => false,
            "https" => true,
            scheme if scheme.starts_with("socks") => {
                bail!("the proxy in {source} is a SOCKS proxy; cww supports HTTP proxies only")
            }
            scheme => bail!("the proxy in {source} has the scheme {scheme:?}; use http://"),
        };
        let host = match url.host() {
            Some(Host::Domain(d)) if !d.is_empty() => d.to_string(),
            Some(Host::Ipv4(ip)) => ip.to_string(),
            Some(Host::Ipv6(ip)) => ip.to_string(),
            _ => bail!("the proxy in {source} has no host"),
        };
        let port = url
            .port_or_known_default()
            .ok_or_else(|| anyhow!("the proxy in {source} has no port"))?;
        let decode = |s: &str| {
            percent_decode_str(s)
                .decode_utf8()
                .map(|s| s.into_owned())
                .map_err(|_| anyhow!("the credentials in the proxy in {source} are not UTF-8"))
        };
        let username = decode(url.username())?;
        let password = url.password().map(decode).transpose()?;
        if username.contains(':') {
            bail!(
                "the user name in the proxy in {source} contains a colon, which Basic auth can't carry"
            );
        }
        Ok(Self {
            secure,
            host,
            port,
            username,
            password,
            source: source.to_string(),
        })
    }

    /// `host:port`, with brackets around an IPv6 address.
    pub fn authority(&self) -> String {
        authority(&self.host, self.port)
    }

    /// Where this proxy came from: `config`, or an environment variable.
    pub fn source(&self) -> &str {
        &self.source
    }

    fn has_credentials(&self) -> bool {
        !self.username.is_empty() || self.password.is_some()
    }

    /// The proxy URL with the password replaced by `***`.
    pub fn redacted(&self) -> String {
        let scheme = if self.secure { "https" } else { "http" };
        let user = match (&self.username, &self.password) {
            (user, Some(_)) => format!("{}:***@", encode_userinfo(user)),
            (user, None) if !user.is_empty() => format!("{}@", encode_userinfo(user)),
            _ => String::new(),
        };
        format!("{scheme}://{user}{}", self.authority())
    }

    pub fn info(&self) -> ProxyInfo {
        ProxyInfo {
            url: self.redacted(),
            source: self.source.clone(),
        }
    }

    fn basic_credentials(&self) -> String {
        let pair = format!(
            "{}:{}",
            self.username,
            self.password.as_deref().unwrap_or("")
        );
        STANDARD.encode(pair)
    }

    /// The same proxy for ureq's own `CONNECT`, used by the pairing requests.
    ///
    /// ureq keeps credentials inside a URI and splits them at the last `:`,
    /// so a password with characters a URI can't hold unencoded (`:`, `/`,
    /// `?`, `#`, `%`, spaces, anything outside ASCII) would reach the proxy
    /// mangled. Those are refused with a clear error instead.
    pub fn to_ureq(&self) -> Result<ureq::Proxy> {
        let protocol = if self.secure {
            ureq::ProxyProtocol::Https
        } else {
            ureq::ProxyProtocol::Http
        };
        let host = match self.host.parse::<IpAddr>() {
            Ok(IpAddr::V6(ip)) => format!("[{ip}]"),
            _ => self.host.clone(),
        };
        let mut builder = ureq::Proxy::builder(protocol).host(&host).port(self.port);
        if self.has_credentials() {
            let fits = |s: &str| {
                s.chars().all(|c| {
                    c.is_ascii_graphic() && !matches!(c, ':' | '/' | '?' | '#' | '%' | '[' | ']')
                })
            };
            let password = self.password.as_deref().unwrap_or("");
            if !fits(&self.username) || !fits(password) {
                bail!(
                    "the proxy credentials from {} contain a character (such as : / ? # % or a space) \
                     that pairing can't send yet; use a proxy account without one",
                    self.source
                );
            }
            builder = builder.username(&self.username);
            if self.password.is_some() {
                builder = builder.password(password);
            }
        }
        builder.build().map_err(|e| {
            anyhow!(
                "the proxy from {} can't be used for pairing ({e})",
                self.source
            )
        })
    }

    /// Open a tunnel through the proxy to `host:port` with `CONNECT`.
    pub async fn connect(&self, host: &str, port: u16) -> Result<ProxyStream> {
        let tcp = TcpStream::connect((self.host.as_str(), self.port))
            .await
            .with_context(|| format!("connecting to the proxy {}", self.authority()))?;
        let mut stream = if self.secure {
            let name = rustls::pki_types::ServerName::try_from(self.host.clone())
                .map_err(|_| anyhow!("the proxy host {} is not a valid TLS name", self.host))?;
            let connector = tokio_rustls::TlsConnector::from(crate::tls::client_config());
            let tls = connector
                .connect(name, tcp)
                .await
                .with_context(|| format!("TLS to the proxy {}", self.authority()))?;
            ProxyStream::Tls(Box::new(tls))
        } else {
            ProxyStream::Tcp(tcp)
        };
        let target = authority(host, port);
        let mut request = format!(
            "CONNECT {target} HTTP/1.1\r\nHost: {target}\r\nUser-Agent: cww/{}\r\n",
            env!("CARGO_PKG_VERSION")
        );
        if self.has_credentials() {
            request.push_str(&format!(
                "Proxy-Authorization: Basic {}\r\n",
                self.basic_credentials()
            ));
        }
        request.push_str("\r\n");
        stream
            .write_all(request.as_bytes())
            .await
            .context("sending CONNECT to the proxy")?;
        stream
            .flush()
            .await
            .context("sending CONNECT to the proxy")?;

        // Read the response head one byte at a time, so nothing that
        // belongs to the tunnel is consumed with it.
        let mut head = Vec::with_capacity(256);
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            if head.len() >= MAX_RESPONSE_HEAD {
                bail!("the proxy sent an oversized response to CONNECT");
            }
            let n = stream
                .read(&mut byte)
                .await
                .context("reading the proxy's answer to CONNECT")?;
            if n == 0 {
                bail!("the proxy closed the connection instead of answering CONNECT");
            }
            head.push(byte[0]);
        }
        let head = String::from_utf8_lossy(&head);
        let status_line = head.lines().next().unwrap_or_default();
        let status = parse_status(status_line)
            .ok_or_else(|| anyhow!("the proxy sent a malformed answer to CONNECT"))?;
        match status {
            200..=299 => Ok(stream),
            407 if self.has_credentials() => {
                bail!(
                    "the proxy rejected the credentials from {} (407)",
                    self.source
                )
            }
            407 => bail!(
                "the proxy asks for credentials (407); put them in the proxy URL: http://user:password@host:port"
            ),
            _ => bail!(
                "the proxy refused to connect to {target} ({})",
                status_line.trim()
            ),
        }
    }
}

fn authority(host: &str, port: u16) -> String {
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V6(ip)) => format!("[{ip}]:{port}"),
        _ => format!("{host}:{port}"),
    }
}

fn encode_userinfo(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

fn parse_status(line: &str) -> Option<u16> {
    let mut parts = line.split_whitespace();
    let version = parts.next()?;
    if !version.starts_with("HTTP/1.") {
        return None;
    }
    parts.next()?.parse().ok()
}

/// The proxy to reach `server` through, from the `proxy` setting in
/// `config.toml` or else the environment. `None` means a direct connection.
pub fn for_server(setting: Option<&str>, server: &ServerUrl) -> Result<Option<Proxy>> {
    resolve(setting, server, |name| std::env::var(name).ok())
}

/// [`for_server`] with the environment passed in, for tests.
pub fn resolve(
    setting: Option<&str>,
    server: &ServerUrl,
    env: impl Fn(&str) -> Option<String>,
) -> Result<Option<Proxy>> {
    if let Some(setting) = setting.map(str::trim).filter(|s| !s.is_empty()) {
        if setting.eq_ignore_ascii_case(NONE) {
            return Ok(None);
        }
        return Proxy::parse(setting, "config").map(Some);
    }
    let names: &[&str] = if server.is_secure() {
        &["https_proxy", "HTTPS_PROXY", "all_proxy", "ALL_PROXY"]
    } else {
        &["http_proxy", "HTTP_PROXY", "all_proxy", "ALL_PROXY"]
    };
    let found = names.iter().find_map(|name| {
        env(name)
            .filter(|v| !v.trim().is_empty())
            .map(|v| (*name, v))
    });
    let Some((name, value)) = found else {
        return Ok(None);
    };
    let no_proxy = env("no_proxy")
        .filter(|v| !v.trim().is_empty())
        .or_else(|| env("NO_PROXY"))
        .unwrap_or_default();
    if NoProxy::parse(&no_proxy).matches(&server.host(), server.port()) {
        return Ok(None);
    }
    Proxy::parse(&value, name).map(Some)
}

/// Proxy variables set in this process's environment, by name.
const ENV_PROXIES: &[&str] = &[
    "https_proxy",
    "HTTPS_PROXY",
    "all_proxy",
    "ALL_PROXY",
    "http_proxy",
    "HTTP_PROXY",
];

/// A warning for `cww login` and `cww daemon install`: a proxy set in this
/// shell only, which the background service won't inherit.
pub fn service_hint(setting: Option<&str>, config_file: &std::path::Path) -> Option<String> {
    if setting.is_some_and(|s| !s.trim().is_empty()) {
        return None;
    }
    let name = ENV_PROXIES
        .iter()
        .find(|name| std::env::var(name).is_ok_and(|v| !v.trim().is_empty()))?;
    Some(format!(
        "{name} is set in this shell, but the background daemon doesn't see your shell's \
         environment. To reach the server through that proxy, add\n  \
         proxy = \"http://proxy.example:3128\"\n\
         with your proxy's address at the top of {}, then run `cww reload`.",
        config_file.display()
    ))
}

/// The `NO_PROXY` list: hosts reached directly even with a proxy set.
///
/// Comma or space separated. `*` matches everything. `example.com`,
/// `.example.com` and `*.example.com` all match `example.com` and every
/// host under it. IP addresses match exactly, and CIDR blocks such as
/// `10.0.0.0/8` match addresses inside them. `host:port` matches only that
/// port. Case and a trailing dot don't matter.
#[derive(Debug, Default)]
pub struct NoProxy {
    entries: Vec<Entry>,
}

#[derive(Debug)]
enum Entry {
    All,
    Domain { name: String, port: Option<u16> },
    Ip { ip: IpAddr, port: Option<u16> },
    Cidr { net: IpAddr, bits: u8 },
}

impl NoProxy {
    pub fn parse(list: &str) -> Self {
        let entries = list
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|s| !s.is_empty())
            .filter_map(Entry::parse)
            .collect();
        Self { entries }
    }

    /// Whether `host:port` is reached directly. `host` may be an IP
    /// address, with or without brackets.
    pub fn matches(&self, host: &str, port: u16) -> bool {
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        let ip = host.parse::<IpAddr>().ok();
        self.entries.iter().any(|entry| match entry {
            Entry::All => true,
            Entry::Domain { name, port: p } => {
                p.is_none_or(|p| p == port)
                    && ip.is_none()
                    && (host == *name
                        || host
                            .strip_suffix(name.as_str())
                            .is_some_and(|rest| rest.ends_with('.')))
            }
            Entry::Ip { ip: entry, port: p } => {
                p.is_none_or(|p| p == port) && ip.is_some_and(|ip| same_ip(ip, *entry))
            }
            Entry::Cidr { net, bits } => ip.is_some_and(|ip| in_cidr(ip, *net, *bits)),
        })
    }
}

impl Entry {
    fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim().to_ascii_lowercase();
        if raw == "*" {
            return Some(Self::All);
        }
        if let Some((net, bits)) = raw.split_once('/') {
            let net: IpAddr = net
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse()
                .ok()?;
            let bits: u8 = bits.parse().ok()?;
            let max = if net.is_ipv4() { 32 } else { 128 };
            return (bits <= max).then_some(Self::Cidr { net, bits });
        }
        // A bare IPv6 address has colons but no port.
        if let Ok(ip) = raw.parse::<IpAddr>() {
            return Some(Self::Ip { ip, port: None });
        }
        let (host, port) = if let Some(rest) = raw.strip_prefix('[') {
            let (ip, after) = rest.split_once(']')?;
            let port = after.strip_prefix(':').and_then(|p| p.parse().ok());
            return ip.parse().ok().map(|ip| Self::Ip { ip, port });
        } else if let Some((host, port)) = raw.rsplit_once(':') {
            (host.to_string(), Some(port.parse().ok()?))
        } else {
            (raw, None)
        };
        if let Ok(ip) = host.parse::<IpAddr>() {
            return Some(Self::Ip { ip, port });
        }
        let name = host
            .trim_start_matches("*.")
            .trim_start_matches('.')
            .trim_end_matches('.')
            .to_string();
        (!name.is_empty()).then_some(Self::Domain { name, port })
    }
}

fn same_ip(a: IpAddr, b: IpAddr) -> bool {
    canonical(a) == canonical(b)
}

/// IPv4-mapped IPv6 addresses compare as IPv4.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    }
}

fn in_cidr(ip: IpAddr, net: IpAddr, bits: u8) -> bool {
    match (canonical(ip), canonical(net)) {
        (IpAddr::V4(ip), IpAddr::V4(net)) => {
            let mask = u32::MAX.checked_shl(32 - u32::from(bits)).unwrap_or(0);
            u32::from(ip) & mask == u32::from(net) & mask
        }
        (IpAddr::V6(ip), IpAddr::V6(net)) => {
            let mask = u128::MAX.checked_shl(128 - u32::from(bits)).unwrap_or(0);
            u128::from(ip) & mask == u128::from(net) & mask
        }
        _ => false,
    }
}

/// A connection to the server: direct, or tunnelled through a proxy (and
/// possibly TLS to that proxy). TLS to the server runs on top of it.
pub enum ProxyStream {
    Tcp(TcpStream),
    Tls(Box<tokio_rustls::client::TlsStream<TcpStream>>),
}

impl AsyncRead for ProxyStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(s) => Pin::new(s).poll_read(cx, buf),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for ProxyStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Tcp(s) => Pin::new(s).poll_write(cx, buf),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(s) => Pin::new(s).poll_flush(cx),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(s) => Pin::new(s).poll_shutdown(cx),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Tcp(s) => Pin::new(s).poll_write_vectored(cx, bufs),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_write_vectored(cx, bufs),
        }
    }

    fn is_write_vectored(&self) -> bool {
        match self {
            Self::Tcp(s) => s.is_write_vectored(),
            Self::Tls(s) => s.is_write_vectored(),
        }
    }
}

/// Connect to `host:port`, through `proxy` when there is one.
pub async fn open(proxy: Option<&Proxy>, host: &str, port: u16) -> Result<ProxyStream> {
    match proxy {
        Some(proxy) => proxy
            .connect(host, port)
            .await
            .with_context(|| format!("through the proxy {}", proxy.redacted())),
        None => {
            let tcp = TcpStream::connect((host, port))
                .await
                .with_context(|| format!("connecting to {}", authority(host, port)))?;
            Ok(ProxyStream::Tcp(tcp))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn server(url: &str) -> ServerUrl {
        ServerUrl::parse(url).unwrap()
    }

    fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| vars.get(name).cloned()
    }

    #[test]
    fn parses_proxy_urls() {
        let p = Proxy::parse("http://proxy.corp:3128", "config").unwrap();
        assert_eq!(p.authority(), "proxy.corp:3128");
        assert_eq!(p.redacted(), "http://proxy.corp:3128");
        let p = Proxy::parse("proxy.corp:8080", "config").unwrap();
        assert_eq!(p.redacted(), "http://proxy.corp:8080");
        let p = Proxy::parse("http://proxy.corp", "config").unwrap();
        assert_eq!(p.port, 80);
        let p = Proxy::parse("https://[::1]:8443", "config").unwrap();
        assert_eq!(p.redacted(), "https://[::1]:8443");
        assert!(Proxy::parse("socks5://proxy:1080", "config").is_err());
        assert!(Proxy::parse("ftp://proxy", "config").is_err());
    }

    #[test]
    fn credentials_are_decoded_and_never_shown() {
        let p = Proxy::parse("http://al%40ice:s%3Acr%23t@proxy:3128", "HTTPS_PROXY").unwrap();
        assert_eq!(p.username, "al@ice");
        assert_eq!(p.password.as_deref(), Some("s:cr#t"));
        assert_eq!(p.redacted(), "http://al%40ice:***@proxy:3128");
        assert!(!format!("{p:?}").contains("cr"));
        assert_eq!(
            STANDARD.decode(p.basic_credentials()).unwrap(),
            b"al@ice:s:cr#t"
        );
        // ureq can't carry that password; the tunnel can.
        let err = p.to_ureq().unwrap_err().to_string();
        assert!(!err.contains("cr#t") && !err.contains("cr%23t"), "{err}");

        let p = Proxy::parse("http://alice:hunter2@proxy:3128", "config").unwrap();
        let ureq = p.to_ureq().unwrap();
        assert_eq!(ureq.username(), Some("alice"));
        assert_eq!(ureq.password(), Some("hunter2"));
        assert_eq!(ureq.port(), 3128);

        // An @ is fine: ureq splits the credentials from the host at the last one.
        let p = Proxy::parse("http://al%40ice:p%40ss!@proxy:3128", "config").unwrap();
        let ureq = p.to_ureq().unwrap();
        assert_eq!(ureq.username(), Some("al@ice"));
        assert_eq!(ureq.password(), Some("p@ss!"));
        assert_eq!(ureq.host(), "proxy");
    }

    #[test]
    fn errors_never_repeat_the_password() {
        for bad in [
            "http://alice:hunter2@",
            "socks5://alice:hunter2@proxy:1080",
            "http://alice:hunter2@proxy:99999",
            "gopher://alice:hunter2@proxy",
        ] {
            let err = format!("{:#}", Proxy::parse(bad, "config").unwrap_err());
            assert!(!err.contains("hunter2"), "{err}");
        }
    }

    #[test]
    fn config_overrides_the_environment() {
        let s = server("https://chatwithwork.com");
        let e = env(&[
            ("HTTPS_PROXY", "http://env:1"),
            ("NO_PROXY", "chatwithwork.com"),
        ]);
        let p = resolve(Some("http://conf:2"), &s, &e).unwrap().unwrap();
        assert_eq!((p.authority().as_str(), p.source()), ("conf:2", "config"));
        assert!(resolve(Some("none"), &s, &e).unwrap().is_none());
        assert!(
            resolve(Some("NONE"), &s, env(&[("HTTPS_PROXY", "http://env:1")]))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn reads_the_environment_like_curl() {
        let https = server("https://chatwithwork.com");
        let http = server("http://127.0.0.1:3000");
        let pick = |s: &ServerUrl, vars: &[(&str, &str)]| {
            resolve(None, s, env(vars))
                .unwrap()
                .map(|p| (p.authority(), p.source().to_string()))
        };
        assert_eq!(pick(&https, &[]), None);
        assert_eq!(
            pick(&https, &[("HTTPS_PROXY", "a:1"), ("https_proxy", "b:2")]),
            Some(("b:2".into(), "https_proxy".into()))
        );
        assert_eq!(
            pick(&https, &[("HTTP_PROXY", "a:1"), ("ALL_PROXY", "c:3")]),
            Some(("c:3".into(), "ALL_PROXY".into()))
        );
        assert_eq!(pick(&https, &[("HTTP_PROXY", "a:1")]), None);
        assert_eq!(
            pick(&http, &[("HTTP_PROXY", "a:1"), ("HTTPS_PROXY", "b:2")]),
            Some(("a:1".into(), "HTTP_PROXY".into()))
        );
        assert_eq!(pick(&https, &[("HTTPS_PROXY", "  ")]), None);
        assert_eq!(
            pick(
                &https,
                &[("HTTPS_PROXY", "a:1"), ("NO_PROXY", ".chatwithwork.com")]
            ),
            None
        );
        assert_eq!(
            pick(
                &http,
                &[("HTTP_PROXY", "a:1"), ("no_proxy", "localhost,127.0.0.1")]
            ),
            None
        );
        assert!(resolve(None, &https, env(&[("HTTPS_PROXY", "socks5://x:1")])).is_err());
    }

    #[test]
    fn no_proxy_matching() {
        let list = NoProxy::parse(
            "localhost, .internal.example,*.corp.example  chat.example.net:8443,10.0.0.0/8,192.168.1.5,[::1],fd00::/8",
        );
        let yes = [
            ("localhost", 443),
            ("LOCALHOST.", 443),
            ("internal.example", 443),
            ("a.b.internal.example", 443),
            ("corp.example", 443),
            ("x.corp.example", 80),
            ("chat.example.net", 8443),
            ("10.1.2.3", 443),
            ("192.168.1.5", 443),
            ("::1", 443),
            ("[::1]", 443),
            ("fd12::1", 443),
            ("::ffff:10.0.0.1", 443),
        ];
        for (host, port) in yes {
            assert!(list.matches(host, port), "{host}:{port} should bypass");
        }
        let no = [
            ("chatwithwork.com", 443),
            ("notlocalhost", 443),
            ("xinternal.example", 443),
            ("chat.example.net", 443),
            ("11.0.0.1", 443),
            ("192.168.1.6", 443),
            ("::2", 443),
        ];
        for (host, port) in no {
            assert!(
                !list.matches(host, port),
                "{host}:{port} should use the proxy"
            );
        }
        assert!(NoProxy::parse("*").matches("anything.example", 1));
        assert!(!NoProxy::parse("").matches("anything.example", 1));
        assert!(!NoProxy::parse("example.com/99").matches("example.com", 1));
    }

    #[test]
    fn parses_connect_status_lines() {
        assert_eq!(
            parse_status("HTTP/1.1 200 Connection established"),
            Some(200)
        );
        assert_eq!(
            parse_status("HTTP/1.0 407 Proxy Authentication Required"),
            Some(407)
        );
        assert_eq!(parse_status("SSH-2.0-OpenSSH"), None);
    }
}
