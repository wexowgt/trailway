//! Shared wire types for API <-> agent messages.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Prefix of the per-server token returned by register.
pub const SERVER_TOKEN_PREFIX: &str = "tw_st_";

/// Seconds without a heartbeat after which a server counts as offline.
pub const OFFLINE_AFTER_SECS: i64 = 30;

/// Body of `POST /api/v1/agent/register`, authenticated with the server key
/// (`Authorization: Bearer tw_sk_...`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterRequest {
    /// Stable id of the host (`/etc/machine-id`); re-registering the same
    /// machine under the same user updates the existing server.
    pub machine_id: String,
    pub hostname: String,
    pub agent_version: String,
}

/// Response of `POST /api/v1/agent/register`. The token is only shown here.
#[derive(Clone, Serialize, Deserialize)]
pub struct RegisterResponse {
    pub server_id: Uuid,
    pub token: String,
}

impl std::fmt::Debug for RegisterResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisterResponse")
            .field("server_id", &self.server_id)
            .field("token", &"[redacted]")
            .finish()
    }
}

/// A total and the part of it in use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resource {
    pub total: u64,
    pub used: u64,
}

/// Body of `POST /api/v1/agent/heartbeat`, authenticated with the server
/// token (`Authorization: Bearer tw_st_...`). CPU is in millicores (1000 per
/// core), memory and disk in bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heartbeat {
    pub agent_version: String,
    pub cpu: Resource,
    pub memory: Resource,
    pub disk: Resource,
    /// `/dev/kvm` is available, so microVMs can run on this host.
    pub kvm: bool,
}

/// Whether a server is currently sending heartbeats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServerStatus {
    Online,
    Offline,
}

/// A server as listed by `GET /api/v1/servers`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Server {
    pub id: Uuid,
    pub hostname: String,
    pub status: ServerStatus,
    pub cpu: Resource,
    pub memory: Resource,
    pub disk: Resource,
    pub kvm: bool,
    pub agent_version: String,
    pub last_heartbeat_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
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
    fn heartbeat_roundtrips() {
        let hb = Heartbeat {
            agent_version: "0.1.0".into(),
            cpu: Resource {
                total: 4000,
                used: 250,
            },
            memory: Resource::default(),
            disk: Resource::default(),
            kvm: true,
        };
        let json = serde_json::to_string(&hb).unwrap();
        assert_eq!(serde_json::from_str::<Heartbeat>(&json).unwrap(), hb);
    }

    #[test]
    fn status_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&ServerStatus::Online).unwrap(),
            "\"online\""
        );
    }

    #[test]
    fn register_response_debug_hides_token() {
        let r = RegisterResponse {
            server_id: Uuid::nil(),
            token: "tw_st_secret".into(),
        };
        assert!(!format!("{r:?}").contains("secret"));
    }

    #[test]
    fn vm_spec_defaults_env_and_cmd() {
        let spec: VmSpec =
            serde_json::from_str(r#"{"image":"nginx","vcpus":1,"mem_mib":256}"#).unwrap();
        assert!(spec.env.is_empty() && spec.cmd.is_empty());
        assert!(spec.port.is_none() && spec.host_port.is_none());
    }
}
