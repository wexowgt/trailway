use std::path::Path;

use sysinfo::{Disks, System};
use trailway_proto::{Heartbeat, RegisterRequest, Resource};

pub const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const KVM_DEVICE: &str = "/dev/kvm";

/// `TRAILWAY_FAKE_RUNTIME=1` simulates VMs, so the host counts as able to run them.
pub fn fake_runtime() -> bool {
    std::env::var("TRAILWAY_FAKE_RUNTIME").is_ok_and(|v| v == "1")
}
const MACHINE_ID_PATHS: [&str; 2] = ["/etc/machine-id", "/var/lib/dbus/machine-id"];

/// `TRAILWAY_MACHINE_ID` overrides detection (development on non-Linux hosts).
pub fn machine_id() -> anyhow::Result<String> {
    if let Ok(id) = std::env::var("TRAILWAY_MACHINE_ID") {
        if !id.trim().is_empty() {
            return Ok(id.trim().to_string());
        }
    }
    for path in MACHINE_ID_PATHS {
        if let Ok(raw) = std::fs::read_to_string(path) {
            let id = raw.trim();
            if !id.is_empty() {
                return Ok(id.to_string());
            }
        }
    }
    anyhow::bail!("no machine id found in {MACHINE_ID_PATHS:?}")
}

const PUBLIC_IP_URL: &str = "https://api.ipify.org";

/// Parses an address the way the API wants it: a plain IP, nothing else.
pub fn parse_ip(text: &str) -> Option<String> {
    text.trim()
        .parse::<std::net::IpAddr>()
        .ok()
        .map(|ip| ip.to_string())
}

/// The host's public IP: `TRAILWAY_PUBLIC_IP` when set, otherwise what an
/// outside service sees. `None` when it cannot be found right now.
pub async fn public_ip(http: &reqwest::Client) -> Option<String> {
    if let Ok(ip) = std::env::var("TRAILWAY_PUBLIC_IP") {
        return parse_ip(&ip);
    }
    let body = http
        .get(PUBLIC_IP_URL)
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;
    parse_ip(&body)
}

pub fn register_request() -> anyhow::Result<RegisterRequest> {
    Ok(RegisterRequest {
        machine_id: machine_id()?,
        hostname: System::host_name().unwrap_or_else(|| "unknown".into()),
        agent_version: AGENT_VERSION.into(),
    })
}

/// Millicores in use from a 0-100 per-core-summed usage percentage.
pub fn cpu_resource(cores: usize, usage_percent_sum: f32) -> Resource {
    let total = cores as u64 * 1000;
    let used = ((usage_percent_sum.max(0.0) as f64) * 10.0).round() as u64;
    Resource {
        total,
        used: used.min(total),
    }
}

/// Total/used of the largest real disk (the one workloads will live on).
pub fn disk_resource(disks: &Disks) -> Resource {
    disks
        .list()
        .iter()
        .max_by_key(|d| d.total_space())
        .map(|d| Resource {
            total: d.total_space(),
            used: d.total_space().saturating_sub(d.available_space()),
        })
        .unwrap_or_default()
}

/// Samples the host. Keep one `Sampler` alive: CPU usage is a delta between
/// two refreshes.
pub struct Sampler {
    sys: System,
}

impl Sampler {
    pub fn new() -> Self {
        let mut sys = System::new();
        sys.refresh_cpu_usage();
        Self { sys }
    }

    pub fn heartbeat(&mut self) -> Heartbeat {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        let cores = self.sys.cpus().len().max(1);
        let usage: f32 = self.sys.cpus().iter().map(|c| c.cpu_usage()).sum();
        let disks = Disks::new_with_refreshed_list();
        Heartbeat {
            agent_version: AGENT_VERSION.into(),
            cpu: cpu_resource(cores, usage),
            memory: Resource {
                total: self.sys.total_memory(),
                used: self.sys.used_memory().min(self.sys.total_memory()),
            },
            disk: disk_resource(&disks),
            kvm: Path::new(KVM_DEVICE).exists() || fake_runtime(),
            public_ip: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_plain_ips() {
        assert_eq!(
            parse_ip(" 178.104.208.91\n").as_deref(),
            Some("178.104.208.91")
        );
        assert_eq!(parse_ip("<html>"), None);
        assert_eq!(parse_ip(""), None);
    }

    #[test]
    fn cpu_millicores() {
        assert_eq!(
            cpu_resource(4, 100.0),
            Resource {
                total: 4000,
                used: 1000
            }
        );
        assert_eq!(cpu_resource(2, 500.0).used, 2000, "clamped to total");
        assert_eq!(cpu_resource(2, -1.0).used, 0);
    }

    #[test]
    fn heartbeat_is_sane() {
        let hb = Sampler::new().heartbeat();
        assert!(hb.cpu.total >= 1000);
        assert!(hb.memory.total > 0);
        assert!(hb.memory.used <= hb.memory.total);
        assert!(hb.disk.used <= hb.disk.total);
    }
}
