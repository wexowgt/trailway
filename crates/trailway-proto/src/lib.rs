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
