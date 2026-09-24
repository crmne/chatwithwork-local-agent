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

#[derive(Clone)]
pub struct SharedStatus(Arc<Mutex<ConnectionStatus>>);

impl Default for SharedStatus {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(ConnectionStatus {
            connection: Connection::NotPaired,
            since: crate::audit::now(),
            last_error: None,
        })))
    }
}

impl SharedStatus {
    pub fn set(&self, connection: Connection, last_error: Option<String>) {
        let mut s = self.0.lock().expect("status lock");
        if s.connection != connection {
            s.since = crate::audit::now();
        }
        s.connection = connection;
        if last_error.is_some() || connection == Connection::Connected {
            s.last_error = last_error;
        }
    }

    pub fn get(&self) -> ConnectionStatus {
        self.0.lock().expect("status lock").clone()
    }
}
