//! The daemon's access tokens, shared by the tunnel and the chat API.
//!
//! One source hands out the current access token and refreshes it with a
//! DPoP proof when it's missing, about to expire, or known to be stale.
//! Refreshes are serialized, so the tunnel and a chat request never race
//! each other with the same refresh credential (a server may rotate it).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::client::{AuthClient, RefreshError, TokenResponse};
use super::key::DeviceKey;
use super::secrets::{REFRESH_TOKEN, SecretStore};

/// Refresh the access token this long before it expires.
const MARGIN: Duration = Duration::from_secs(60);

struct Current {
    token: String,
    expires: Instant,
    scope: String,
}

pub struct Tokens {
    client: Arc<AuthClient>,
    key: Arc<DeviceKey>,
    store: SecretStore,
    current: Mutex<Option<Current>>,
}

impl Tokens {
    pub fn new(client: Arc<AuthClient>, key: Arc<DeviceKey>, store: SecretStore) -> Self {
        Self {
            client,
            key,
            store,
            current: Mutex::new(None),
        }
    }

    pub fn client(&self) -> &Arc<AuthClient> {
        &self.client
    }

    pub fn key(&self) -> &Arc<DeviceKey> {
        &self.key
    }

    /// A live access token, refreshed first if needed. Blocks on the
    /// network when it refreshes.
    pub fn access_token(&self) -> Result<String, RefreshError> {
        let mut current = self.current.lock().expect("token lock");
        if let Some(c) = current.as_ref()
            && Instant::now() + MARGIN < c.expires
        {
            return Ok(c.token.clone());
        }
        let fresh = self.refresh_locked()?;
        let token = fresh.token.clone();
        *current = Some(fresh);
        Ok(token)
    }

    /// A new access token even if the current one looks fine: the server
    /// said it lacks a scope that was granted since.
    pub fn refresh(&self) -> Result<String, RefreshError> {
        let mut current = self.current.lock().expect("token lock");
        let fresh = self.refresh_locked()?;
        let token = fresh.token.clone();
        *current = Some(fresh);
        Ok(token)
    }

    /// Forget the current token, after the server rejected it.
    pub fn invalidate(&self) {
        *self.current.lock().expect("token lock") = None;
    }

    /// The scopes of the current token, if there is one.
    pub fn scope(&self) -> Option<String> {
        self.current
            .lock()
            .expect("token lock")
            .as_ref()
            .map(|c| c.scope.clone())
    }

    fn refresh_locked(&self) -> Result<Current, RefreshError> {
        let refresh = self
            .store
            .get(REFRESH_TOKEN)?
            .ok_or_else(|| RefreshError::Revoked("no refresh credential stored".into()))?;
        let response: TokenResponse = self.client.refresh(&self.key, &refresh)?;
        if let Some(rotated) = &response.refresh_token
            && rotated != &refresh
        {
            self.store.set(REFRESH_TOKEN, rotated)?;
        }
        Ok(Current {
            expires: Instant::now() + Duration::from_secs(response.expires_in.clamp(1, 3600)),
            scope: response.scope.clone().unwrap_or_default(),
            token: response.access_token,
        })
    }
}
