//! The outbound tunnel: one WebSocket to `wss://<server>/local_agent`,
//! reconnected with jittered exponential backoff.
//!
//! Before each connection the daemon makes sure it holds a live access token
//! (refreshing it with a DPoP proof when needed), then opens the socket with
//! the token and a fresh proof in the upgrade headers. Tokens never go in
//! the URL. If the server rejects the refresh credential, the device was
//! revoked: the tunnel stops and waits for `cww login`.

pub mod framing;
pub mod session;

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::anyhow;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};
use tokio_util::sync::CancellationToken;

use crate::audit::{AuditEntry, AuditLog};
use crate::auth::client::{AuthClient, RefreshError, ServerUrl, TokenResponse};
use crate::auth::key::DeviceKey;
use crate::auth::secrets::{REFRESH_TOKEN, SecretStore};
use crate::proxy::{Proxy, ProxyStream};
use crate::status::{Connection, SharedStatus};
use crate::tools::LocalFiles;
use framing::{Framing, OFFERED_SUBPROTOCOLS};
use session::{SessionEnd, SessionOptions};

/// Refresh the access token this long before it expires.
const TOKEN_MARGIN: Duration = Duration::from_secs(60);
/// A session that lasted this long resets the backoff.
const STABLE_AFTER: Duration = Duration::from_secs(60);
const BACKOFF_BASE: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);

#[derive(Debug, PartialEq, Eq)]
pub enum TunnelExit {
    Shutdown,
    Revoked,
}

pub struct TunnelSettings {
    pub server: ServerUrl,
    pub device_id: String,
    pub key: Arc<DeviceKey>,
    pub store: SecretStore,
    pub max_message_bytes: usize,
    /// The proxy to reach the server through, if any.
    pub proxy: Option<Proxy>,
}

pub struct Tunnel {
    settings: TunnelSettings,
    client: Arc<AuthClient>,
    files: LocalFiles,
    audit: Arc<AuditLog>,
    status: SharedStatus,
    token: Option<(String, Instant)>,
}

enum ConnectError {
    Unauthorized { nonce: Option<String> },
    Other(anyhow::Error),
}

type Socket = WebSocketStream<MaybeTlsStream<ProxyStream>>;

impl Tunnel {
    pub fn new(
        settings: TunnelSettings,
        files: LocalFiles,
        audit: Arc<AuditLog>,
        status: SharedStatus,
    ) -> anyhow::Result<Self> {
        let client = Arc::new(AuthClient::new(
            settings.server.clone(),
            settings.proxy.as_ref(),
        )?);
        Ok(Self {
            settings,
            client,
            files,
            audit,
            status,
            token: None,
        })
    }

    pub async fn run(mut self, shutdown: CancellationToken) -> TunnelExit {
        let mut backoff = Backoff::default();
        let mut immediate_retry = false;
        loop {
            if shutdown.is_cancelled() {
                return TunnelExit::Shutdown;
            }
            self.status.set(Connection::Connecting, None);
            let result = match self.access_token().await {
                Ok(token) => self.connect(&token).await.map(|ws| (ws, token)),
                Err(RefreshError::Revoked(reason)) => {
                    self.status.set(Connection::Revoked, Some(reason.clone()));
                    let mut entry = AuditEntry::event("revoked");
                    entry.detail = Some(format!(
                        "the server rejected the refresh credential ({reason})"
                    ));
                    self.audit.append(&entry);
                    return TunnelExit::Revoked;
                }
                Err(RefreshError::Other(e)) => Err(ConnectError::Other(e)),
            };
            match result {
                Ok(((ws, framing), _token)) => {
                    self.status.set(Connection::Connected, None);
                    let mut entry = AuditEntry::event("connected");
                    entry.detail = Some(format!(
                        "{} as device {}",
                        self.settings.server.websocket_url(),
                        self.settings.device_id
                    ));
                    self.audit.append(&entry);
                    let started = Instant::now();
                    let end = session::run_session(
                        ws,
                        self.files.clone(),
                        SessionOptions {
                            framing,
                            max_message_bytes: self.settings.max_message_bytes,
                        },
                        shutdown.child_token(),
                    )
                    .await;
                    let mut entry = AuditEntry::event("disconnected");
                    entry.detail = Some(format!("{end:?}"));
                    self.audit.append(&entry);
                    if started.elapsed() >= STABLE_AFTER {
                        backoff.reset();
                    }
                    match end {
                        SessionEnd::Shutdown => return TunnelExit::Shutdown,
                        SessionEnd::Unauthorized(_) => self.token = None,
                        _ => {}
                    }
                    self.status
                        .set(Connection::Offline, Some(describe_end(&end)));
                }
                Err(ConnectError::Unauthorized { nonce }) => {
                    self.status.set(
                        Connection::Offline,
                        Some("the server rejected the connection".into()),
                    );
                    let fresh_nonce = nonce.is_some();
                    self.client.set_nonce(nonce);
                    if !fresh_nonce {
                        self.token = None;
                    }
                    if !immediate_retry {
                        immediate_retry = true;
                        continue;
                    }
                }
                Err(ConnectError::Other(e)) => {
                    tracing::warn!("connection failed: {e:#}");
                    self.status.set(Connection::Offline, Some(format!("{e:#}")));
                }
            }
            immediate_retry = false;
            let delay = backoff.next_delay();
            tokio::select! {
                () = shutdown.cancelled() => return TunnelExit::Shutdown,
                () = tokio::time::sleep(delay) => {}
            }
        }
    }

    /// A live access token, refreshed if missing or about to expire.
    async fn access_token(&mut self) -> Result<String, RefreshError> {
        if let Some((token, expires)) = &self.token
            && Instant::now() + TOKEN_MARGIN < *expires
        {
            return Ok(token.clone());
        }
        let client = Arc::clone(&self.client);
        let key = Arc::clone(&self.settings.key);
        let store = self.settings.store.clone();
        let response: TokenResponse = tokio::task::spawn_blocking(move || {
            let refresh = store
                .get(REFRESH_TOKEN)?
                .ok_or_else(|| RefreshError::Revoked("no refresh credential stored".into()))?;
            let response = client.refresh(&key, &refresh)?;
            if let Some(rotated) = &response.refresh_token
                && rotated != &refresh
            {
                store.set(REFRESH_TOKEN, rotated)?;
            }
            Ok::<_, RefreshError>(response)
        })
        .await
        .map_err(|e| RefreshError::Other(anyhow!("token refresh task failed: {e}")))??;
        let expires = Instant::now() + Duration::from_secs(response.expires_in.clamp(1, 3600));
        self.token = Some((response.access_token.clone(), expires));
        Ok(response.access_token)
    }

    async fn connect(&self, token: &str) -> Result<(Socket, Framing), ConnectError> {
        let server = &self.settings.server;
        let url = server.websocket_url();
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|e| ConnectError::Other(e.into()))?;
        let proof = self.settings.key.proof(
            "GET",
            &server.websocket_htu(),
            self.client.nonce().as_deref(),
            Some(token),
        );
        let headers = request.headers_mut();
        let header = |v: &str| HeaderValue::from_str(v).map_err(|e| ConnectError::Other(e.into()));
        headers.insert("Authorization", header(&format!("DPoP {token}"))?);
        headers.insert("DPoP", header(&proof)?);
        headers.insert("Sec-WebSocket-Protocol", header(OFFERED_SUBPROTOCOLS)?);
        headers.insert("Origin", header(&server.origin())?);
        headers.insert(
            "User-Agent",
            header(&format!("cww/{}", env!("CARGO_PKG_VERSION")))?,
        );

        let frame_cap = frame_cap(self.settings.max_message_bytes);
        let config = WebSocketConfig::default()
            .max_message_size(Some(frame_cap))
            .max_frame_size(Some(frame_cap));
        let connector = server
            .is_secure()
            .then(|| Connector::Rustls(crate::tls::client_config()));
        let proxy = self.settings.proxy.as_ref();
        let (host, port) = (server.host(), server.port());
        // The TCP connection, through the proxy if there is one, then TLS
        // and the upgrade on top of it.
        let connect = async move {
            let stream = crate::proxy::open(proxy, &host, port).await.map_err(Err)?;
            tokio_tungstenite::client_async_tls_with_config(
                request,
                stream,
                Some(config),
                connector,
            )
            .await
            .map_err(Ok)
        };
        let (ws, response) = match tokio::time::timeout(Duration::from_secs(30), connect).await {
            Err(_) => {
                return Err(ConnectError::Other(anyhow!(
                    "timed out connecting to {url}"
                )));
            }
            Ok(Ok(pair)) => pair,
            Ok(Err(Ok(tokio_tungstenite::tungstenite::Error::Http(response))))
                if matches!(response.status().as_u16(), 401 | 403) =>
            {
                let nonce = response
                    .headers()
                    .get("DPoP-Nonce")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                return Err(ConnectError::Unauthorized { nonce });
            }
            Ok(Err(Ok(tokio_tungstenite::tungstenite::Error::Http(response)))) => {
                return Err(ConnectError::Other(anyhow!(
                    "the server refused the WebSocket upgrade ({})",
                    response.status()
                )));
            }
            Ok(Err(Ok(e))) => return Err(ConnectError::Other(anyhow!("connecting to {url}: {e}"))),
            Ok(Err(Err(e))) => {
                return Err(ConnectError::Other(anyhow!("connecting to {url}: {e:#}")));
            }
        };
        let selected = response
            .headers()
            .get("Sec-WebSocket-Protocol")
            .and_then(|v| v.to_str().ok());
        let framing = Framing::for_protocol(selected).ok_or_else(|| {
            ConnectError::Other(anyhow!(
                "the server chose an unknown subprotocol {selected:?}"
            ))
        })?;
        Ok((ws, framing))
    }
}

/// Largest WebSocket frame accepted. Action Cable string-encodes the
/// JSON-RPC message, which can at most double its size.
pub fn frame_cap(max_message_bytes: usize) -> usize {
    max_message_bytes * 2 + 16 * 1024
}

fn describe_end(end: &SessionEnd) -> String {
    match end {
        SessionEnd::Closed { code, reason } => format!("closed by the server ({code:?} {reason})"),
        SessionEnd::Unauthorized(reason) => format!("unauthorized ({reason})"),
        SessionEnd::IdleTimeout => "no traffic from the server".into(),
        SessionEnd::Shutdown => "shut down".into(),
        SessionEnd::Error(e) => e.clone(),
    }
}

/// Full-jitter exponential backoff: a random delay in [0, min(max, base·2^n)].
#[derive(Default)]
struct Backoff {
    attempt: u32,
}

impl Backoff {
    fn next_delay(&mut self) -> Duration {
        let ceiling = BACKOFF_BASE
            .saturating_mul(1u32 << self.attempt.min(16))
            .min(BACKOFF_MAX);
        self.attempt = self.attempt.saturating_add(1);
        let mut byte = [0u8; 4];
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut byte).ok();
        let fraction = f64::from(u32::from_le_bytes(byte)) / f64::from(u32::MAX);
        ceiling.mul_f64(fraction).max(Duration::from_millis(250))
    }

    fn reset(&mut self) {
        self.attempt = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        let mut b = Backoff::default();
        for _ in 0..20 {
            assert!(b.next_delay() <= BACKOFF_MAX);
        }
        b.reset();
        assert!(b.next_delay() <= BACKOFF_BASE);
    }
}
