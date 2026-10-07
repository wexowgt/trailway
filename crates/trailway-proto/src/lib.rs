//! Shared wire types for API <-> agent messages.

use serde::{Deserialize, Serialize};

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
    }
}
