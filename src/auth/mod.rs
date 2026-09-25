//! Pairing and credentials.
//!
//! `cww login` runs the OAuth device authorization grant (RFC 8628) with a
//! fresh Ed25519 device key. The key and the refresh credential go to the
//! secret store; the server origin and device ID go to the config file.

pub mod client;
pub mod key;
pub mod secrets;
pub mod tokens;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::config::{Config, ServerConfig};
use crate::paths::Paths;
use client::{AuthClient, DeviceAuthorization, DeviceInfo, PollOutcome, ServerUrl};
use key::DeviceKey;
use secrets::{DEVICE_KEY, REFRESH_TOKEN, SecretStore};

pub struct Credentials {
    pub server: ServerUrl,
    pub device_id: String,
    pub key: DeviceKey,
    pub store: SecretStore,
}

/// Load the pairing, or `None` if this computer isn't paired.
pub fn load_credentials(config: &Config, paths: &Paths) -> Result<Option<Credentials>> {
    let Some(server) = &config.server else {
        return Ok(None);
    };
    let store = SecretStore::for_config(config, paths);
    let Some(stored_key) = store.get(DEVICE_KEY)? else {
        return Ok(None);
    };
    Ok(Some(Credentials {
        server: ServerUrl::parse(&server.url)?,
        device_id: server.device_id.clone(),
        key: DeviceKey::from_stored(&stored_key)?,
        store,
    }))
}

#[derive(Debug, Default)]
pub struct LoginOptions {
    /// Device name shown on the server (defaults to the hostname).
    pub name: Option<String>,
    /// Where to keep the secrets (defaults to the keychain if it works).
    pub store: Option<SecretStore>,
    /// Open the approval page in the default browser.
    pub open_browser: bool,
    /// Don't ask for the terminal UI's chats, only for sharing folders.
    pub without_chats: bool,
}

/// A pairing started with [`start_login`], waiting for the user to approve
/// the code on the server.
pub struct PendingLogin {
    paths: Paths,
    server: ServerUrl,
    client: AuthClient,
    key: DeviceKey,
    info: DeviceInfo,
    auth: DeviceAuthorization,
    store: Option<SecretStore>,
    deadline: Instant,
}

impl PendingLogin {
    /// The code the user enters on the server, such as `WDJB-MJHT`.
    pub fn user_code(&self) -> &str {
        &self.auth.user_code
    }

    /// Where to enter the code.
    pub fn verification_uri(&self) -> &str {
        &self.auth.verification_uri
    }

    /// The same page with the code filled in, when the server offers it.
    pub fn verification_uri_complete(&self) -> Option<&str> {
        self.auth.verification_uri_complete.as_deref()
    }

    /// The page to open in a browser, if it is safe to: an http(s) page on
    /// the server being paired with, so a server can't make a client open
    /// some other program or site.
    pub fn browser_url(&self) -> Option<&str> {
        let origin = format!("{}/", self.server.origin());
        [
            self.verification_uri_complete(),
            Some(self.verification_uri()),
        ]
        .into_iter()
        .flatten()
        .find(|url| url.starts_with(&origin))
    }

    /// The name the server shows for this computer.
    pub fn device_name(&self) -> &str {
        &self.info.name
    }

    /// The device key's thumbprint, which the approval page can show.
    pub fn fingerprint(&self) -> String {
        self.key.thumbprint()
    }

    /// Poll until the user approves or denies, the code expires, or `cancel`
    /// is set. Saves the key, the refresh token and the server on approval.
    pub fn wait(self, cancel: &AtomicBool) -> Result<ServerConfig> {
        let mut interval = self.auth.interval.max(1);
        let token = loop {
            // Sleep in short steps so a cancel takes effect quickly.
            let wake = Instant::now() + Duration::from_secs(interval);
            while Instant::now() < wake {
                if cancel.load(Ordering::SeqCst) {
                    bail!("pairing was cancelled");
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            if Instant::now() > self.deadline {
                bail!("the pairing code expired; start again for a new one");
            }
            match self
                .client
                .poll_pairing(&self.key, &self.auth.device_code)?
            {
                PollOutcome::Pending => {}
                PollOutcome::SlowDown => interval += 5,
                PollOutcome::Denied => bail!("pairing was denied on the server"),
                PollOutcome::Expired => {
                    bail!("the pairing code expired; start again for a new one")
                }
                PollOutcome::Approved(token) => break token,
            }
        };
        let device_id = token.device_id.clone().expect("checked in poll_pairing");
        let refresh = token
            .refresh_token
            .clone()
            .expect("checked in poll_pairing");

        let paths = &self.paths;
        let mut config = Config::load(paths)?;
        // Forget an earlier pairing, wherever its secrets were.
        if config.server.is_some() {
            let old_store = SecretStore::for_config(&config, paths);
            let _ = old_store.delete(DEVICE_KEY);
            let _ = old_store.delete(REFRESH_TOKEN);
        }

        let store = self.store.unwrap_or_else(|| SecretStore::choose(paths));
        store.set(DEVICE_KEY, &self.key.to_stored())?;
        store.set(REFRESH_TOKEN, &refresh)?;
        let server_config = ServerConfig {
            url: self.server.origin(),
            device_id,
        };
        config.server = Some(server_config.clone());
        config.secret_store = Some(store.name().to_string());
        config.save(paths)?;
        Ok(server_config)
    }
}

/// Start pairing this computer with a server: create a key and ask the
/// server for a code. [`PendingLogin::wait`] finishes it.
pub fn start_login(paths: &Paths, server: &str, options: LoginOptions) -> Result<PendingLogin> {
    let LoginOptions {
        name,
        store,
        without_chats,
        ..
    } = options;
    let server = ServerUrl::parse(server)?;
    let proxy = crate::proxy::for_server(Config::load(paths)?.proxy.as_deref(), &server)?;
    let client = AuthClient::new(server.clone(), proxy.as_ref())?;
    let key = DeviceKey::generate()?;
    let info = DeviceInfo::current(name);
    let auth = client
        .start_pairing(&key, &info, !without_chats)
        .context("starting pairing")?;
    let deadline = Instant::now() + Duration::from_secs(auth.expires_in.max(1));
    Ok(PendingLogin {
        paths: paths.clone(),
        server,
        client,
        key,
        info,
        auth,
        store,
        deadline,
    })
}

/// Pair this computer with a server. Blocks until the user approves,
/// denies, or the code expires. `say` receives the lines to show the user.
pub fn login(
    paths: &Paths,
    server: &str,
    options: LoginOptions,
    mut say: impl FnMut(&str),
) -> Result<ServerConfig> {
    let open_browser = options.open_browser;
    let pending = start_login(paths, server, options)?;
    say(&format!(
        "To connect \"{}\" to Chat with Work:",
        pending.device_name()
    ));
    say("");
    say(&format!("  1. Open {}", pending.verification_uri()));
    say(&format!("  2. Enter the code {}", pending.user_code()));
    if let Some(complete) = pending.verification_uri_complete() {
        say("");
        say(&format!("Or open {complete}"));
    }
    say("");
    say(&format!("Key fingerprint: {}", pending.fingerprint()));
    if open_browser && pending.browser_url().is_some_and(crate::browser::open) {
        say("Opened the approval page in your browser.");
    }
    say("Waiting for approval...");
    pending.wait(&AtomicBool::new(false))
}

pub fn logout(paths: &Paths) -> Result<bool> {
    let mut config = Config::load(paths)?;
    let store = SecretStore::for_config(&config, paths);
    store.delete(DEVICE_KEY)?;
    store.delete(REFRESH_TOKEN)?;
    let was_paired = config.server.take().is_some();
    config.save(paths)?;
    Ok(was_paired)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(verification_uri: &str, complete: Option<&str>) -> PendingLogin {
        let server = ServerUrl::parse("https://chatwithwork.com").unwrap();
        PendingLogin {
            paths: Paths::under(std::path::Path::new("/nonexistent")),
            client: AuthClient::new(server.clone(), None).unwrap(),
            server,
            key: DeviceKey::generate().unwrap(),
            info: DeviceInfo::current(Some("test".into())),
            auth: DeviceAuthorization {
                device_code: "d".into(),
                user_code: "WDJB-MJHT".into(),
                verification_uri: verification_uri.into(),
                verification_uri_complete: complete.map(str::to_string),
                expires_in: 60,
                interval: 1,
            },
            store: None,
            deadline: Instant::now(),
        }
    }

    #[test]
    fn only_pages_on_the_server_are_opened_in_a_browser() {
        let p = pending(
            "https://chatwithwork.com/device",
            Some("https://chatwithwork.com/device?user_code=WDJB-MJHT"),
        );
        assert_eq!(
            p.browser_url(),
            Some("https://chatwithwork.com/device?user_code=WDJB-MJHT")
        );
        // A server can't send the browser somewhere else...
        let p = pending(
            "https://chatwithwork.com/device",
            Some("https://evil.example/device"),
        );
        assert_eq!(p.browser_url(), Some("https://chatwithwork.com/device"));
        // ...including a look-alike host or another scheme.
        let p = pending("https://chatwithwork.com.evil.example/device", None);
        assert_eq!(p.browser_url(), None);
        let p = pending("file:///etc/passwd", None);
        assert_eq!(p.browser_url(), None);
    }

    #[test]
    fn a_cancelled_wait_stops_promptly() {
        let p = pending("https://chatwithwork.com/device", None);
        let cancel = AtomicBool::new(true);
        let started = Instant::now();
        let err = p.wait(&cancel).unwrap_err();
        assert!(err.to_string().contains("cancelled"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
