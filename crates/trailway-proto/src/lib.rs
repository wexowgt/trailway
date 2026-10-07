//! Shared wire types for API <-> agent messages.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Sent by an agent when it first connects to the control plane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentHello {
    pub agent_version: String,
    pub hostname: String,
}

/// Periodic liveness message from an agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heartbeat {
    pub agent_version: String,
}

/// Response body of the API health endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Health {
    pub status: String,
}

/// Body of `POST /api/v1/auth/signup` and `POST /api/v1/auth/login`.
#[derive(Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub email: String,
    pub password: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("email", &self.email)
            .field("password", &"[redacted]")
            .finish()
    }
}

/// A user account as returned by the API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub created_at: DateTime<Utc>,
}

/// Body of `POST /api/v1/server-keys`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateServerKey {
    pub name: String,
}

/// A server key without its secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerKey {
    pub id: Uuid,
    pub name: String,
    /// First characters of the secret, enough to recognise the key.
    pub prefix: String,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

/// Response of `POST /api/v1/server-keys`: the only time the secret is shown.
#[derive(Clone, Serialize, Deserialize)]
pub struct CreatedServerKey {
    #[serde(flatten)]
    pub key: ServerKey,
    pub secret: String,
}

impl std::fmt::Debug for CreatedServerKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreatedServerKey")
            .field("key", &self.key)
            .field("secret", &"[redacted]")
            .finish()
    }
}

/// Error envelope used by every non-2xx API response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorEnvelope {
    pub error: ErrorBody,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_roundtrips() {
        let hello = AgentHello {
            agent_version: "0.1.0".into(),
            hostname: "box".into(),
        };
        let json = serde_json::to_string(&hello).unwrap();
        assert_eq!(serde_json::from_str::<AgentHello>(&json).unwrap(), hello);
    }
}
