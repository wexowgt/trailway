//! Shared wire types for API <-> agent messages.

use std::collections::BTreeMap;

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

/// Lifecycle of a deployment, as stored by the API and reported by the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentStatus {
    Queued,
    Building,
    Deploying,
    Running,
    Failed,
    Stopped,
}

impl DeploymentStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Building => "building",
            Self::Deploying => "deploying",
            Self::Running => "running",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "queued" => Self::Queued,
            "building" => Self::Building,
            "deploying" => Self::Deploying,
            "running" => Self::Running,
            "failed" => Self::Failed,
            "stopped" => Self::Stopped,
            _ => return None,
        })
    }

    /// No further status changes are expected without a new job.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Failed | Self::Stopped)
    }
}

/// CPU a vCPU reserves on a server, in the millicores the heartbeat uses.
pub const MILLICORES_PER_VCPU: u64 = 1000;

/// Path of the agent WebSocket (`GET`, `Authorization: Bearer tw_st_...`).
pub const AGENT_WS_PATH: &str = "/api/v1/agent/ws";

/// A deploy job: run `spec` for `service_id`, replacing the VM that service
/// currently has on this server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployJob {
    pub deployment_id: Uuid,
    pub service_id: Uuid,
    pub spec: VmSpec,
}

/// Messages from the API to the agent over the WebSocket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApiMessage {
    Deploy(DeployJob),
    /// Stop and forget a deployment's VM.
    Stop {
        deployment_id: Uuid,
    },
}

/// What the agent knows about one deployment (sent on connect and on change).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentReport {
    pub deployment_id: Uuid,
    pub status: DeploymentStatus,
    #[serde(default)]
    pub vm_id: Option<String>,
    #[serde(default)]
    pub host_port: Option<u16>,
    /// Why a deployment failed.
    #[serde(default)]
    pub error: Option<String>,
}

/// Messages from the agent to the API over the WebSocket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentMessage {
    /// First message after connecting: the actual state of every deployment
    /// the agent still knows about.
    Hello {
        deployments: Vec<DeploymentReport>,
    },
    Status(DeploymentReport),
    /// Console output of a deployment. `offset` is the byte position of the
    /// chunk in the VM's console log and `len` its raw size, so the API can
    /// drop chunks it already has after a reconnect.
    Logs {
        deployment_id: Uuid,
        offset: u64,
        len: u64,
        text: String,
    },
}

/// Body of `POST /api/v1/projects` and `POST /api/v1/projects/{id}/environments`,
/// and of the matching `PATCH`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NameBody {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Environment {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

/// Body of `POST /api/v1/environments/{id}/services`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateService {
    pub name: String,
    /// OCI image reference, e.g. `nginxdemos/hello`.
    pub image: String,
    #[serde(default)]
    pub vcpus: Option<u8>,
    #[serde(default)]
    pub memory_mib: Option<u32>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Port the app listens on; a host port of the server is forwarded to it.
    #[serde(default)]
    pub port: Option<u16>,
    /// Target server; must belong to the caller.
    pub server_id: Uuid,
}

/// Body of `PATCH /api/v1/services/{id}`. Unset fields stay as they are; a
/// change only applies to the next deploy.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateService {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub vcpus: Option<u8>,
    #[serde(default)]
    pub memory_mib: Option<u32>,
    #[serde(default)]
    pub env: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub server_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Service {
    pub id: Uuid,
    pub environment_id: Uuid,
    pub name: String,
    pub image: String,
    pub vcpus: u8,
    pub memory_mib: u32,
    pub env: BTreeMap<String, String>,
    pub port: Option<u16>,
    pub server_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deployment {
    pub id: Uuid,
    pub service_id: Option<Uuid>,
    pub server_id: Uuid,
    pub status: DeploymentStatus,
    pub image: String,
    pub vcpus: u8,
    pub memory_mib: u32,
    /// Port on the server that forwards to the app port, once running.
    pub host_port: Option<u16>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
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
    fn messages_are_tagged() {
        let m = AgentMessage::Status(DeploymentReport {
            deployment_id: Uuid::nil(),
            status: DeploymentStatus::Running,
            vm_id: Some("vm".into()),
            host_port: Some(20000),
            error: None,
        });
        let json = serde_json::to_value(&m).unwrap();
        assert_eq!(json["type"], "status");
        assert_eq!(json["status"], "running");
        assert_eq!(serde_json::from_value::<AgentMessage>(json).unwrap(), m);
    }

    #[test]
    fn deployment_status_strings_roundtrip() {
        for s in [
            DeploymentStatus::Queued,
            DeploymentStatus::Building,
            DeploymentStatus::Deploying,
            DeploymentStatus::Running,
            DeploymentStatus::Failed,
            DeploymentStatus::Stopped,
        ] {
            assert_eq!(DeploymentStatus::parse(s.as_str()), Some(s));
        }
        assert!(DeploymentStatus::parse("nope").is_none());
    }

    #[test]
    fn vm_spec_defaults_env_and_cmd() {
        let spec: VmSpec =
            serde_json::from_str(r#"{"image":"nginx","vcpus":1,"mem_mib":256}"#).unwrap();
        assert!(spec.env.is_empty() && spec.cmd.is_empty());
        assert!(spec.port.is_none() && spec.host_port.is_none());
    }
}
