//! Host networking for microVMs: one tap device per VM on a `trailway0`
//! bridge with a private /24, NAT for egress and port forwarding via nftables.
//!
//! Isolation: tap ports are bridge-isolated (no L2 path between VMs) and the
//! `forward` chain drops anything routed from the bridge back onto it, so a
//! guest cannot hairpin through the host either.

use anyhow::{bail, Context, Result};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use trailway_proto::VmNetwork;

pub const BRIDGE: &str = "trailway0";
pub const GATEWAY: Ipv4Addr = Ipv4Addr::new(10, 200, 0, 1);
pub const PREFIX_LEN: u8 = 24;
const SUBNET: &str = "10.200.0.0/24";
const TABLE: &str = "trailway";
const FIRST_HOST: u8 = 2;
const LAST_HOST: u8 = 254;
const DEFAULT_DNS: &str = "1.1.1.1 8.8.8.8";

/// Tap device name for a VM id (`vm-1a2b3c4d` -> `tw-1a2b3c4d`, 11 chars).
pub fn tap_name(id: &str) -> String {
    format!("tw{}", id.trim_start_matches("vm"))
}

pub fn guest_ip(octet: u8) -> Ipv4Addr {
    Ipv4Addr::new(10, 200, 0, octet)
}

/// Deterministic MAC derived from the guest's last octet.
pub fn guest_mac(octet: u8) -> String {
    format!("AA:FC:00:00:00:{octet:02X}")
}

/// Guest-side network setup, run by init from the config drive.
pub fn guest_script(ip: Ipv4Addr, dns: &str) -> String {
    format!(
        "#!/.trailway/busybox sh\n\
B=/.trailway/busybox\n\
$B ip link set lo up\n\
$B ip addr add {ip}/{PREFIX_LEN} dev eth0\n\
$B ip link set eth0 up\n\
$B ip route add default via {GATEWAY}\n\
$B mkdir -p /etc\n\
: > /etc/resolv.conf\n\
for ns in {dns}; do echo \"nameserver $ns\" >> /etc/resolv.conf; done\n"
    )
}

/// Nameservers handed to guests: `TRAILWAY_DNS` (space separated) or public resolvers.
pub fn dns_servers() -> String {
    std::env::var("TRAILWAY_DNS").unwrap_or_else(|_| DEFAULT_DNS.into())
}

/// Ruleset created once per host (the whole table is ours).
pub fn base_ruleset() -> String {
    format!(
        "add table ip {TABLE}\n\
add chain ip {TABLE} prerouting {{ type nat hook prerouting priority dstnat; policy accept; }}\n\
add chain ip {TABLE} output {{ type nat hook output priority -100; policy accept; }}\n\
add chain ip {TABLE} postrouting {{ type nat hook postrouting priority srcnat; policy accept; }}\n\
add chain ip {TABLE} forward {{ type filter hook forward priority 0; policy accept; }}\n\
add rule ip {TABLE} postrouting ip saddr {SUBNET} oifname != \"{BRIDGE}\" masquerade\n\
add rule ip {TABLE} postrouting oifname \"{BRIDGE}\" ip saddr 127.0.0.0/8 masquerade\n\
add rule ip {TABLE} forward iifname \"{BRIDGE}\" oifname \"{BRIDGE}\" drop\n"
    )
}

/// Forwarding rules for one VM: external traffic and host-local traffic.
pub fn forward_rules(id: &str, ip: Ipv4Addr, host_port: u16, app_port: u16) -> String {
    let to = format!("dnat to {ip}:{app_port} comment \"{id}\"");
    format!(
        "add rule ip {TABLE} prerouting tcp dport {host_port} {to}\n\
add rule ip {TABLE} output fib daddr type local tcp dport {host_port} {to}\n"
    )
}

/// Rule handles in an `nft -a list chain` listing that carry this VM's comment.
pub fn handles_for(listing: &str, id: &str) -> Vec<u64> {
    let needle = format!("comment \"{id}\"");
    listing
        .lines()
        .filter(|l| l.contains(&needle))
        .filter_map(|l| l.rsplit_once("# handle ")?.1.trim().parse().ok())
        .collect()
}

/// Whether a TCP port can be bound on all interfaces right now.
pub fn port_is_free(port: u16) -> bool {
    TcpListener::bind(("0.0.0.0", port)).is_ok()
}

/// Asks the kernel for a free port.
pub fn random_free_port() -> Result<u16> {
    Ok(TcpListener::bind(("0.0.0.0", 0))?.local_addr()?.port())
}

fn run(prog: &str, args: &[&str]) -> Result<()> {
    let out = Command::new(prog)
        .args(args)
        .output()
        .with_context(|| format!("spawning {prog}"))?;
    if !out.status.success() {
        bail!(
            "{prog} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

fn nft_stdin(script: &str) -> Result<()> {
    let mut child = Command::new("nft")
        .args(["-f", "-"])
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .context("spawning nft")?;
    child
        .stdin
        .take()
        .context("nft stdin")?
        .write_all(script.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!(
            "nft failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Host-side network manager. Needs root, `ip` and `nft`.
pub struct HostNet {
    ips_dir: PathBuf,
}

impl HostNet {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            ips_dir: data_dir.join("net").join("ips"),
        }
    }

    /// Idempotently creates the bridge, enables forwarding and loads the base nft table.
    pub fn ensure_host(&self) -> Result<()> {
        if run("ip", &["link", "show", BRIDGE]).is_err() {
            run("ip", &["link", "add", BRIDGE, "type", "bridge"])?;
            run(
                "ip",
                &[
                    "addr",
                    "add",
                    &format!("{GATEWAY}/{PREFIX_LEN}"),
                    "dev",
                    BRIDGE,
                ],
            )?;
        }
        run("ip", &["link", "set", BRIDGE, "up"])?;
        fs::write("/proc/sys/net/ipv4/ip_forward", "1").context("enabling ip_forward")?;
        // Lets 127.0.0.1:<host port> be DNATed to a VM (as docker does).
        fs::write(
            format!("/proc/sys/net/ipv4/conf/{BRIDGE}/route_localnet"),
            "1",
        )
        .context("enabling route_localnet")?;
        if run("nft", &["list", "table", "ip", TABLE]).is_err() {
            nft_stdin(&base_ruleset())?;
        }
        Ok(())
    }

    /// Reserves the lowest free guest address for `id`.
    fn reserve_ip(&self, id: &str) -> Result<u8> {
        fs::create_dir_all(&self.ips_dir)?;
        for octet in FIRST_HOST..=LAST_HOST {
            let path = self.ips_dir.join(octet.to_string());
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => {
                    f.write_all(id.as_bytes())?;
                    return Ok(octet);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
        bail!("no free addresses left in {SUBNET}")
    }

    fn release_ip(&self, id: &str) {
        let Ok(entries) = fs::read_dir(&self.ips_dir) else {
            return;
        };
        for e in entries.flatten() {
            if fs::read_to_string(e.path()).is_ok_and(|s| s == id) {
                let _ = fs::remove_file(e.path());
            }
        }
    }

    /// Creates the tap, rules and address for a VM. On error nothing is left behind.
    pub fn setup(
        &self,
        id: &str,
        app_port: Option<u16>,
        host_port: Option<u16>,
    ) -> Result<VmNetwork> {
        self.ensure_host()?;
        let result = self.setup_inner(id, app_port, host_port);
        if result.is_err() {
            self.teardown(id);
        }
        result
    }

    fn setup_inner(
        &self,
        id: &str,
        app_port: Option<u16>,
        host_port: Option<u16>,
    ) -> Result<VmNetwork> {
        let octet = self.reserve_ip(id)?;
        let ip = guest_ip(octet);
        let tap = tap_name(id);
        run("ip", &["tuntap", "add", "dev", &tap, "mode", "tap"])?;
        run("ip", &["link", "set", &tap, "master", BRIDGE])?;
        run(
            "ip",
            &[
                "link",
                "set",
                &tap,
                "type",
                "bridge_slave",
                "isolated",
                "on",
            ],
        )?;
        run("ip", &["link", "set", &tap, "up"])?;
        let forward = match app_port {
            Some(app) => {
                let host = match host_port {
                    Some(p) if port_is_free(p) => p,
                    Some(p) => bail!("host port {p} is already in use"),
                    None => random_free_port()?,
                };
                nft_stdin(&forward_rules(id, ip, host, app))?;
                Some((host, app))
            }
            None => None,
        };
        Ok(VmNetwork {
            ip: ip.to_string(),
            mac: guest_mac(octet),
            tap,
            host_port: forward.map(|f| f.0),
            app_port: forward.map(|f| f.1),
        })
    }

    /// Removes the tap, forwarding rules and address reservation. Idempotent.
    pub fn teardown(&self, id: &str) {
        for chain in ["prerouting", "output"] {
            let Ok(out) = Command::new("nft")
                .args(["-a", "list", "chain", "ip", TABLE, chain])
                .output()
            else {
                continue;
            };
            for h in handles_for(&String::from_utf8_lossy(&out.stdout), id) {
                let _ = run(
                    "nft",
                    &[
                        "delete",
                        "rule",
                        "ip",
                        TABLE,
                        chain,
                        "handle",
                        &h.to_string(),
                    ],
                );
            }
        }
        let _ = run("ip", &["link", "del", &tap_name(id)]);
        self.release_ip(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tap_names_fit_ifnamsiz() {
        assert_eq!(tap_name("vm-1a2b3c4d"), "tw-1a2b3c4d");
        assert!(tap_name("vm-1a2b3c4d").len() <= 15);
    }

    #[test]
    fn guest_script_configures_address_route_and_dns() {
        let s = guest_script(guest_ip(5), "1.1.1.1 8.8.8.8");
        assert!(s.contains("ip addr add 10.200.0.5/24 dev eth0"));
        assert!(s.contains("ip route add default via 10.200.0.1"));
        assert!(s.contains("for ns in 1.1.1.1 8.8.8.8;"));
    }

    #[test]
    fn forward_rules_cover_external_and_local_traffic() {
        let r = forward_rules("vm-ab", guest_ip(2), 8080, 80);
        assert!(r.contains("prerouting tcp dport 8080 dnat to 10.200.0.2:80 comment \"vm-ab\""));
        assert!(r.contains("output fib daddr type local tcp dport 8080 dnat to 10.200.0.2:80"));
    }

    #[test]
    fn base_ruleset_masquerades_and_blocks_hairpin() {
        let r = base_ruleset();
        assert!(r.contains("masquerade"));
        assert!(r.contains("iifname \"trailway0\" oifname \"trailway0\" drop"));
    }

    #[test]
    fn finds_rule_handles_by_comment() {
        let listing = "table ip trailway {\n chain prerouting {\n  tcp dport 8080 dnat to 10.200.0.2:80 comment \"vm-ab\" # handle 7\n  tcp dport 9 dnat to 10.200.0.3:80 comment \"vm-cd\" # handle 8\n }\n}";
        assert_eq!(handles_for(listing, "vm-ab"), vec![7]);
        assert!(handles_for(listing, "vm-zz").is_empty());
    }

    #[test]
    fn requested_port_in_use_is_detected() {
        let l = TcpListener::bind(("0.0.0.0", 0)).unwrap();
        let p = l.local_addr().unwrap().port();
        assert!(!port_is_free(p));
        assert!(random_free_port().unwrap() > 0);
    }
}
