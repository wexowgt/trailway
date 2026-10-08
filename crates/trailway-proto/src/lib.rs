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

/// What to run in a microVM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmSpec {
    /// OCI image reference, e.g. `nginxdemos/hello` or `ghcr.io/org/app:1.2`.
    pub image: String,
    pub vcpus: u8,
    pub mem_mib: u32,
    /// Extra environment variables, applied on top of the image's own env.
    #[serde(default)]
    pub env: Vec<(String, String)>,
    /// Overrides the image's entrypoint and cmd when non-empty.
    #[serde(default)]
    pub cmd: Vec<String>,
    /// Port the app listens on inside the VM. When set, a host port is forwarded to it.
    #[serde(default)]
    pub port: Option<u16>,
    /// Host port to forward to `port`; a random free one is chosen when unset.
    #[serde(default)]
    pub host_port: Option<u16>,
}

/// Network attachment of a running microVM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmNetwork {
    /// Private guest address on the `trailway0` bridge.
    pub ip: String,
    pub mac: String,
    pub tap: String,
    pub host_port: Option<u16>,
    pub app_port: Option<u16>,
}

/// Lifecycle state of a microVM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VmState {
    Running,
    Stopped,
}

/// Snapshot of one microVM, as reported by `Runtime::status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmInfo {
    pub id: String,
    pub spec: VmSpec,
    pub state: VmState,
    /// Resolved image digest the rootfs was built from.
    pub image_digest: String,
    #[serde(default)]
    pub network: Option<VmNetwork>,
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

    #[test]
    fn vm_spec_defaults_env_and_cmd() {
        let spec: VmSpec =
            serde_json::from_str(r#"{"image":"nginx","vcpus":1,"mem_mib":256}"#).unwrap();
        assert!(spec.env.is_empty() && spec.cmd.is_empty());
        assert!(spec.port.is_none() && spec.host_port.is_none());
    }
}
