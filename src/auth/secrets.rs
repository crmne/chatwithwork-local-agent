//! Where the device key and refresh token live.
//!
//! The OS keychain (macOS Keychain, Secret Service on Linux) is preferred.
//! When it is unavailable, as on headless servers, secrets go to a 0600 JSON
//! file in the config directory instead. `CWW_SECRET_STORE=file` forces the
//! file store.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};

use crate::config::Config;
use crate::paths::{Paths, write_private_file};

const KEYRING_SERVICE: &str = "com.chatwithwork.cww";

pub const DEVICE_KEY: &str = "device-key";
pub const REFRESH_TOKEN: &str = "refresh-token";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretStore {
    Keyring,
    File(PathBuf),
}

impl SecretStore {
    /// The store recorded in the config, unless the environment overrides it.
    pub fn for_config(config: &Config, paths: &Paths) -> Self {
        let choice = std::env::var("CWW_SECRET_STORE")
            .ok()
            .or_else(|| config.secret_store.clone());
        match choice.as_deref() {
            Some("file") => Self::File(paths.secrets_file()),
            _ => Self::Keyring,
        }
    }

    /// Pick a store for a new pairing: the keychain if a round trip works,
    /// otherwise the file.
    pub fn choose(paths: &Paths) -> Self {
        if std::env::var("CWW_SECRET_STORE").as_deref() == Ok("file") {
            return Self::File(paths.secrets_file());
        }
        let probe = "probe";
        let works = Self::Keyring.set(probe, "ok").is_ok()
            && Self::Keyring.get(probe).ok().flatten().as_deref() == Some("ok");
        let _ = Self::Keyring.delete(probe);
        if works {
            Self::Keyring
        } else {
            Self::File(paths.secrets_file())
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Keyring => "keyring",
            Self::File(_) => "file",
        }
    }

    pub fn get(&self, name: &str) -> Result<Option<String>> {
        match self {
            Self::Keyring => match entry(name)?.get_password() {
                Ok(v) => Ok(Some(v)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(e) => Err(anyhow!("reading {name} from the keychain: {e}")),
            },
            Self::File(path) => Ok(read_file(path)?.remove(name)),
        }
    }

    pub fn set(&self, name: &str, value: &str) -> Result<()> {
        match self {
            Self::Keyring => entry(name)?
                .set_password(value)
                .map_err(|e| anyhow!("writing {name} to the keychain: {e}")),
            Self::File(path) => {
                let mut map = read_file(path)?;
                map.insert(name.to_string(), value.to_string());
                write_private_file(path, &serde_json::to_vec_pretty(&map)?)
            }
        }
    }

    pub fn delete(&self, name: &str) -> Result<()> {
        match self {
            Self::Keyring => match entry(name)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(anyhow!("deleting {name} from the keychain: {e}")),
            },
            Self::File(path) => {
                let mut map = read_file(path)?;
                if map.remove(name).is_some() {
                    write_private_file(path, &serde_json::to_vec_pretty(&map)?)?;
                }
                Ok(())
            }
        }
    }
}

fn entry(name: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, name).map_err(|e| anyhow!("opening the keychain: {e}"))
}

fn read_file(path: &PathBuf) -> Result<BTreeMap<String, String>> {
    match std::fs::read(path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_store_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SecretStore::File(tmp.path().join("secrets.json"));
        assert_eq!(store.get("a").unwrap(), None);
        store.set("a", "1").unwrap();
        store.set("b", "2").unwrap();
        assert_eq!(store.get("a").unwrap().as_deref(), Some("1"));
        store.delete("a").unwrap();
        assert_eq!(store.get("a").unwrap(), None);
        assert_eq!(store.get("b").unwrap().as_deref(), Some("2"));
    }
}
