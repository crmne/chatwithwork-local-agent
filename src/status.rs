//! Live daemon state, shared between the tunnel and the control socket.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Connection {
    NotPaired,
    Connecting,
    Connected,
    Offline,
    Revoked,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionStatus {
    pub connection: Connection,
    pub since: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// A wake-up signal for "something in the status changed". Bumps coalesce:
/// many bumps before the listener runs wake it once. Safe to bump from any
/// thread, including the indexer's.
#[derive(Clone, Default)]
pub struct Changes(Arc<tokio::sync::Notify>);

impl Changes {
    pub fn bump(&self) {
        self.0.notify_one();
    }

    pub async fn wait(&self) {
        self.0.notified().await;
    }
}

#[derive(Clone)]
pub struct SharedStatus {
    inner: Arc<Mutex<ConnectionStatus>>,
    changes: Changes,
}

impl Default for SharedStatus {
    fn default() -> Self {
        Self::new(Changes::default())
    }
}

impl SharedStatus {
    pub fn new(changes: Changes) -> Self {
        Self {
            inner: Arc::new(Mutex::new(ConnectionStatus {
                connection: Connection::NotPaired,
                since: crate::audit::now(),
                last_error: None,
            })),
            changes,
        }
    }

    pub fn set(&self, connection: Connection, last_error: Option<String>) {
        let mut s = self.inner.lock().expect("status lock");
        let before = s.clone();
        if s.connection != connection {
            s.since = crate::audit::now();
        }
        s.connection = connection;
        if last_error.is_some() || connection == Connection::Connected {
            s.last_error = last_error;
        }
        if s.connection != before.connection || s.last_error != before.last_error {
            self.changes.bump();
        }
    }

    pub fn get(&self) -> ConnectionStatus {
        self.inner.lock().expect("status lock").clone()
    }
}
