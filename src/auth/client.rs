//! HTTP calls to the server: device authorization (RFC 8628) and the token
//! endpoint, both with DPoP proofs (RFC 9449).
//!
//! Requests are form-encoded as in OAuth; responses are JSON. HTTPS only,
//! except plain HTTP to a loopback address for development. Redirects are
//! never followed.

use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use url::{Host, Url};

use super::key::DeviceKey;
use crate::proxy::Proxy;

pub const DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
pub const SCOPE: &str = "local_agent:serve";
/// Asked for as well when pairing, unless the user opts out: the terminal
/// UI's chats, relayed by the daemon. The server asks the user about it
/// separately and may leave it out.
pub const CHAT_SCOPE: &str = "local_agent:chat";

/// A validated server origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerUrl {
    origin: Url,
}

impl ServerUrl {
    pub fn parse(input: &str) -> Result<Self> {
        let input = input.trim().trim_end_matches('/');
        let with_scheme = if input.contains("://") {
            input.to_string()
        } else {
            format!("https://{input}")
        };
        let url =
            Url::parse(&with_scheme).with_context(|| format!("{input:?} is not a valid URL"))?;
        if !url.username().is_empty() || url.password().is_some() {
            bail!("the server URL must not contain credentials");
        }
        if url.query().is_some() || url.fragment().is_some() || !matches!(url.path(), "" | "/") {
            bail!("the server URL must be an origin like https://chatwithwork.com");
        }
        match url.scheme() {
            "https" => {}
            "http" if is_loopback(&url) => {}
            "http" => bail!("plain http is only allowed for localhost; use https"),
            other => bail!("unsupported scheme {other:?}; use https"),
        }
        let mut origin = url;
        origin.set_path("/");
        Ok(Self { origin })
    }

    /// `https://host[:port]` without a trailing slash.
    pub fn origin(&self) -> String {
        self.origin.as_str().trim_end_matches('/').to_string()
    }

    pub fn is_secure(&self) -> bool {
        self.origin.scheme() == "https"
    }

    /// The host, without brackets around an IPv6 address.
    pub fn host(&self) -> String {
        match self.origin.host() {
            Some(Host::Ipv6(ip)) => ip.to_string(),
            Some(host) => host.to_string(),
            None => String::new(),
        }
    }

    /// The port, explicit or the scheme's default.
    pub fn port(&self) -> u16 {
        self.origin.port_or_known_default().unwrap_or(443)
    }

    pub fn endpoint(&self, path: &str) -> String {
        format!("{}/{}", self.origin(), path.trim_start_matches('/'))
    }

    /// `wss://host/local_agent` (or `ws://` on loopback).
    pub fn websocket_url(&self) -> String {
        let scheme = if self.is_secure() { "wss" } else { "ws" };
        let rest = self
            .origin()
            .split_once("://")
            .map(|(_, r)| r.to_string())
            .unwrap_or_default();
        format!("{scheme}://{rest}/local_agent")
    }

    /// The `htu` for the WebSocket upgrade: the HTTP(S) form of its URL.
    pub fn websocket_htu(&self) -> String {
        self.endpoint("local_agent")
    }
}

fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(d)) => d == "localhost",
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub name: String,
    pub platform: String,
    pub client_version: String,
}

impl DeviceInfo {
    pub fn current(name: Option<String>) -> Self {
        Self {
            name: name.unwrap_or_else(hostname),
            platform: std::env::consts::OS.to_string(),
            client_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

/// This computer's name, without the `.local` that macOS adds.
fn hostname() -> String {
    #[cfg(unix)]
    let host = rustix::system::uname()
        .nodename()
        .to_string_lossy()
        .into_owned();
    #[cfg(windows)]
    let host = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows PC".into());
    host.strip_suffix(".local").unwrap_or(&host).to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceAuthorization {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
    #[serde(default = "default_interval")]
    pub interval: u64,
}

fn default_interval() -> u64 {
    5
}

#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: u64,
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Opaque. Accepted as a JSON string or number.
    #[serde(default, deserialize_with = "string_or_number")]
    pub device_id: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
}

fn string_or_number<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match Option::<serde_json::Value>::deserialize(d)? {
        Some(serde_json::Value::String(s)) => Some(s),
        Some(serde_json::Value::Number(n)) => Some(n.to_string()),
        _ => None,
    })
}

#[derive(Debug, Clone, Deserialize)]
struct OAuthError {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

#[derive(Debug)]
pub enum PollOutcome {
    Pending,
    SlowDown,
    Denied,
    Expired,
    Approved(TokenResponse),
}

#[derive(Debug, thiserror::Error)]
pub enum RefreshError {
    /// The server rejected the refresh credential: the device was revoked
    /// or the grant expired. Pair again with `cww login`.
    #[error("the server revoked this computer ({0}); run `cww login` to pair again")]
    Revoked(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub struct AuthClient {
    server: ServerUrl,
    agent: ureq::Agent,
    nonce: Mutex<Option<String>>,
}

impl AuthClient {
    /// A client for `server`, through `proxy` when there is one. Without a
    /// proxy it connects directly, whatever the environment says: the
    /// caller has already applied the environment (see `crate::proxy`).
    pub fn new(server: ServerUrl, proxy: Option<&Proxy>) -> Result<Self> {
        let proxy = proxy.map(Proxy::to_ureq).transpose()?;
        let config = ureq::Agent::config_builder()
            .proxy(proxy)
            .https_only(server.is_secure())
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(30)))
            .user_agent(format!("cww/{}", env!("CARGO_PKG_VERSION")))
            .tls_config(crate::tls::ureq_config())
            .build();
        Ok(Self {
            server,
            agent: config.into(),
            nonce: Mutex::new(None),
        })
    }

    /// The latest `DPoP-Nonce` the server sent.
    pub fn nonce(&self) -> Option<String> {
        self.nonce.lock().expect("nonce").clone()
    }

    pub fn set_nonce(&self, nonce: Option<String>) {
        if nonce.is_some() {
            *self.nonce.lock().expect("nonce") = nonce;
        }
    }

    /// POST a form with a DPoP proof, retrying once if the server asks for a
    /// fresh nonce. Returns the status and the JSON body.
    fn post(
        &self,
        key: &DeviceKey,
        path: &str,
        form: &[(&str, &str)],
    ) -> Result<(u16, serde_json::Value)> {
        let url = self.server.endpoint(path);
        for attempt in 0..2 {
            let proof = key.proof("POST", &url, self.nonce().as_deref(), None);
            let mut response = self
                .agent
                .post(&url)
                .header("DPoP", &proof)
                .header("Accept", "application/json")
                .send_form(form.iter().copied())
                .with_context(|| format!("contacting {url}"))?;
            let status = response.status().as_u16();
            self.set_nonce(
                response
                    .headers()
                    .get("DPoP-Nonce")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string),
            );
            if (300..400).contains(&status) {
                bail!(
                    "the server answered {url} with a redirect ({status}); redirects are never followed"
                );
            }
            let body: serde_json::Value = response
                .body_mut()
                .with_config()
                .limit(64 * 1024)
                .read_json()
                .unwrap_or(serde_json::Value::Null);
            if attempt == 0 && body["error"] == "use_dpop_nonce" {
                continue;
            }
            return Ok((status, body));
        }
        unreachable!("the loop returns on the second attempt")
    }

    /// Step 1 of pairing: register the device key and get a user code.
    /// With `chats`, also ask for the terminal UI's chats.
    pub fn start_pairing(
        &self,
        key: &DeviceKey,
        info: &DeviceInfo,
        chats: bool,
    ) -> Result<DeviceAuthorization> {
        let public_key = key.public_key();
        let scope = if chats {
            format!("{SCOPE} {CHAT_SCOPE}")
        } else {
            SCOPE.to_string()
        };
        let form = [
            ("client_id", "cww"),
            ("scope", scope.as_str()),
            ("public_key", public_key.as_str()),
            ("name", info.name.as_str()),
            ("platform", info.platform.as_str()),
            ("client_version", info.client_version.as_str()),
        ];
        let (status, body) = self.post(key, "local_agent/device_authorizations", &form)?;
        if status != 200 {
            return Err(oauth_error(status, &body));
        }
        serde_json::from_value(body).context("unexpected device authorization response")
    }

    /// Step 2 of pairing: poll the token endpoint once.
    pub fn poll_pairing(&self, key: &DeviceKey, device_code: &str) -> Result<PollOutcome> {
        let form = [
            ("grant_type", DEVICE_CODE_GRANT),
            ("device_code", device_code),
            ("client_id", "cww"),
        ];
        let (status, body) = self.post(key, "local_agent/token", &form)?;
        if status == 200 {
            let token: TokenResponse =
                serde_json::from_value(body).context("unexpected token response")?;
            if token.device_id.is_none() || token.refresh_token.is_none() {
                bail!("the token response is missing device_id or refresh_token");
            }
            return Ok(PollOutcome::Approved(token));
        }
        match body["error"].as_str() {
            Some("authorization_pending") => Ok(PollOutcome::Pending),
            Some("slow_down") => Ok(PollOutcome::SlowDown),
            Some("access_denied") => Ok(PollOutcome::Denied),
            Some("expired_token") => Ok(PollOutcome::Expired),
            _ => Err(oauth_error(status, &body)),
        }
    }

    /// A JSON request to the server's API as this device: the access token
    /// in `Authorization: DPoP`, and a proof bound to it for this method and
    /// URL. Retries once when the server asks for a fresh nonce. Returns the
    /// status and the JSON body (`null` when the body isn't JSON).
    pub fn send_json(
        &self,
        key: &DeviceKey,
        method: &str,
        path: &str,
        access_token: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<(u16, serde_json::Value)> {
        let url = self.server.endpoint(path);
        for attempt in 0..2 {
            let proof = key.proof(method, &url, self.nonce().as_deref(), Some(access_token));
            let authorization = format!("DPoP {access_token}");
            let result = match (method, body) {
                ("GET", _) => self
                    .agent
                    .get(&url)
                    .header("Authorization", &authorization)
                    .header("DPoP", &proof)
                    .header("Accept", "application/json")
                    .call(),
                (_, Some(body)) => self
                    .agent
                    .post(&url)
                    .header("Authorization", &authorization)
                    .header("DPoP", &proof)
                    .header("Accept", "application/json")
                    .send_json(body),
                (_, None) => self
                    .agent
                    .post(&url)
                    .header("Authorization", &authorization)
                    .header("DPoP", &proof)
                    .header("Accept", "application/json")
                    .send_empty(),
            };
            let mut response = result.with_context(|| format!("contacting {url}"))?;
            let status = response.status().as_u16();
            self.set_nonce(
                response
                    .headers()
                    .get("DPoP-Nonce")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string),
            );
            if (300..400).contains(&status) {
                bail!(
                    "the server answered {url} with a redirect ({status}); redirects are never followed"
                );
            }
            let body: serde_json::Value = response
                .body_mut()
                .with_config()
                .limit(4 * 1024 * 1024)
                .read_json()
                .unwrap_or(serde_json::Value::Null);
            if attempt == 0 && body["error"] == "use_dpop_nonce" {
                continue;
            }
            return Ok((status, body));
        }
        unreachable!("the loop returns on the second attempt")
    }

    /// Exchange the refresh credential for a short-lived access token.
    pub fn refresh(
        &self,
        key: &DeviceKey,
        refresh_token: &str,
    ) -> Result<TokenResponse, RefreshError> {
        let form = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", "cww"),
        ];
        let (status, body) = self.post(key, "local_agent/token", &form)?;
        if status == 200 {
            return serde_json::from_value(body)
                .context("unexpected token response")
                .map_err(RefreshError::Other);
        }
        match body["error"].as_str() {
            Some(code @ ("invalid_grant" | "unauthorized_client" | "access_denied")) => {
                Err(RefreshError::Revoked(code.to_string()))
            }
            _ => Err(RefreshError::Other(oauth_error(status, &body))),
        }
    }
}

fn oauth_error(status: u16, body: &serde_json::Value) -> anyhow::Error {
    match serde_json::from_value::<OAuthError>(body.clone()) {
        Ok(e) => anyhow!(
            "server error {status}: {}{}",
            e.error,
            e.error_description
                .map(|d| format!(" ({d})"))
                .unwrap_or_default()
        ),
        Err(_) => anyhow!("server error {status}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_id_may_be_a_number() {
        let t: TokenResponse = serde_json::from_str(
            r#"{"access_token":"a","token_type":"DPoP","expires_in":600,"device_id":42}"#,
        )
        .unwrap();
        assert_eq!(t.device_id.as_deref(), Some("42"));
    }

    #[test]
    fn validates_server_urls() {
        let s = ServerUrl::parse("chatwithwork.com").unwrap();
        assert_eq!(s.origin(), "https://chatwithwork.com");
        assert_eq!(s.websocket_url(), "wss://chatwithwork.com/local_agent");
        assert_eq!(s.websocket_htu(), "https://chatwithwork.com/local_agent");
        assert_eq!(
            s.endpoint("local_agent/token"),
            "https://chatwithwork.com/local_agent/token"
        );
        let local = ServerUrl::parse("http://127.0.0.1:3000/").unwrap();
        assert_eq!(local.websocket_url(), "ws://127.0.0.1:3000/local_agent");
        assert!(ServerUrl::parse("http://localhost:3000").is_ok());
        for bad in [
            "http://example.com",
            "ftp://example.com",
            "https://user:pw@example.com",
            "https://example.com/app",
            "https://example.com/?x=1",
        ] {
            assert!(ServerUrl::parse(bad).is_err(), "{bad}");
        }
    }
}
