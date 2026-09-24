//! Pairing and credentials.
//!
//! `cww login` runs the OAuth device authorization grant (RFC 8628) with a
//! fresh Ed25519 device key. The key and the refresh credential go to the
//! secret store; the server origin and device ID go to the config file.

pub mod client;
pub mod key;
pub mod secrets;

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::config::{Config, ServerConfig};
use crate::paths::Paths;
use client::{AuthClient, DeviceInfo, PollOutcome, ServerUrl};
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
}

/// Pair this computer with a server. Blocks until the user approves,
/// denies, or the code expires. `say` receives the lines to show the user.
pub fn login(
    paths: &Paths,
    server: &str,
    options: LoginOptions,
    mut say: impl FnMut(&str),
) -> Result<ServerConfig> {
    let LoginOptions { name, store } = options;
    let server = ServerUrl::parse(server)?;
    let client = AuthClient::new(server.clone());
    let key = DeviceKey::generate()?;
    let info = DeviceInfo::current(name);
    let auth = client
        .start_pairing(&key, &info)
        .context("starting pairing")?;

    say(&format!("To connect \"{}\" to Chat with Work:", info.name));
    say("");
    say(&format!("  1. Open {}", auth.verification_uri));
    say(&format!("  2. Enter the code {}", auth.user_code));
    if let Some(complete) = &auth.verification_uri_complete {
        say("");
        say(&format!("Or open {complete}"));
    }
    say("");
    say(&format!("Key fingerprint: {}", key.thumbprint()));
    say("Waiting for approval...");

    let deadline = Instant::now() + Duration::from_secs(auth.expires_in.max(1));
    let mut interval = auth.interval.max(1);
    let token = loop {
        std::thread::sleep(Duration::from_secs(interval));
        if Instant::now() > deadline {
            bail!("the pairing code expired; run `cww login` again");
        }
        match client.poll_pairing(&key, &auth.device_code)? {
            PollOutcome::Pending => {}
            PollOutcome::SlowDown => interval += 5,
            PollOutcome::Denied => bail!("pairing was denied on the server"),
            PollOutcome::Expired => bail!("the pairing code expired; run `cww login` again"),
            PollOutcome::Approved(token) => break token,
        }
    };
    let device_id = token.device_id.clone().expect("checked in poll_pairing");
    let refresh = token
        .refresh_token
        .clone()
        .expect("checked in poll_pairing");

    let mut config = Config::load(paths)?;
    // Forget an earlier pairing, wherever its secrets were.
    if config.server.is_some() {
        let old_store = SecretStore::for_config(&config, paths);
        let _ = old_store.delete(DEVICE_KEY);
        let _ = old_store.delete(REFRESH_TOKEN);
    }

    let store = store.unwrap_or_else(|| SecretStore::choose(paths));
    store.set(DEVICE_KEY, &key.to_stored())?;
    store.set(REFRESH_TOKEN, &refresh)?;
    let server_config = ServerConfig {
        url: server.origin(),
        device_id,
    };
    config.server = Some(server_config.clone());
    config.secret_store = Some(store.name().to_string());
    config.save(paths)?;
    Ok(server_config)
}

/// Forget the pairing locally. The server side is revoked from Settings.
pub fn logout(paths: &Paths) -> Result<bool> {
    let mut config = Config::load(paths)?;
    let store = SecretStore::for_config(&config, paths);
    store.delete(DEVICE_KEY)?;
    store.delete(REFRESH_TOKEN)?;
    let was_paired = config.server.take().is_some();
    config.save(paths)?;
    Ok(was_paired)
}
