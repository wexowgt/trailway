//! `Runtime` implementation backed by Firecracker (needs Linux + /dev/kvm).

use crate::image::{make_config_drive, ImageStore};
use crate::net::{self, HostNet};
use crate::runtime::{validate_spec, Runtime};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use trailway_proto::{VmInfo, VmNetwork, VmSpec, VmState};

const BOOT_ARGS: &str =
    "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw init=/.trailway/init";
const SOCKET_TIMEOUT: Duration = Duration::from_secs(5);
const GRACEFUL_STOP: Duration = Duration::from_secs(5);

/// Host paths the runtime needs. Defaults match `scripts/setup-host.sh`.
#[derive(Debug, Clone)]
pub struct Config {
    pub data_dir: PathBuf,
    pub kernel: PathBuf,
    pub firecracker: PathBuf,
    pub busybox: PathBuf,
}

impl Config {
    /// Reads `TRAILWAY_DATA_DIR`, `TRAILWAY_KERNEL`, `TRAILWAY_FIRECRACKER`,
    /// `TRAILWAY_BUSYBOX`, falling back to the setup script's locations.
    pub fn from_env() -> Self {
        let var = |k: &str, d: &str| PathBuf::from(std::env::var(k).unwrap_or_else(|_| d.into()));
        let data_dir = var("TRAILWAY_DATA_DIR", "/var/lib/trailway");
        Self {
            kernel: var(
                "TRAILWAY_KERNEL",
                &format!("{}/vmlinux", data_dir.display()),
            ),
            firecracker: var("TRAILWAY_FIRECRACKER", "/usr/local/bin/firecracker"),
            busybox: var("TRAILWAY_BUSYBOX", "/bin/busybox"),
            data_dir,
        }
    }
}

pub struct FirecrackerRuntime {
    cfg: Config,
    images: ImageStore,
    net: HostNet,
}

impl FirecrackerRuntime {
    pub fn new(cfg: Config) -> Self {
        let images = ImageStore::new(cfg.data_dir.clone(), cfg.busybox.clone());
        let net = HostNet::new(&cfg.data_dir);
        Self { cfg, images, net }
    }

    fn vm_dir(&self, id: &str) -> PathBuf {
        self.cfg.data_dir.join("vms").join(id)
    }

    fn read_info(&self, id: &str) -> Result<VmInfo> {
        let path = self.vm_dir(id).join("info.json");
        let raw = fs::read(&path).with_context(|| format!("unknown vm {id}"))?;
        Ok(serde_json::from_slice(&raw)?)
    }

    fn pid(&self, id: &str) -> Option<u32> {
        fs::read_to_string(self.vm_dir(id).join("pid"))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    fn launch(
        &self,
        id: &str,
        spec: &VmSpec,
        rootfs: &Path,
        config: &Path,
        network: &VmNetwork,
    ) -> Result<u32> {
        let dir = self.vm_dir(id);
        let sock = dir.join("api.sock");
        let log = File::create(dir.join("console.log"))?;
        let child = Command::new(&self.cfg.firecracker)
            .arg("--api-sock")
            .arg(&sock)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            // Own process group so the VM outlives this CLI invocation.
            .process_group(0)
            .spawn()
            .with_context(|| format!("starting {}", self.cfg.firecracker.display()))?;
        let pid = child.id();
        fs::write(dir.join("pid"), pid.to_string())?;
        wait_for_socket(&sock)?;
        for (path, body) in api_requests(spec, &self.cfg.kernel, rootfs, config, Some(network)) {
            api_call(&sock, "PUT", path, &body)?;
        }
        Ok(pid)
    }
}

impl Runtime for FirecrackerRuntime {
    fn start(&self, spec: &VmSpec) -> Result<String> {
        validate_spec(spec)?;
        let image = self.images.ensure(&spec.image)?;
        let id = new_id();
        let dir = self.vm_dir(&id);
        fs::create_dir_all(&dir)?;
        let rootfs = dir.join("rootfs.ext4");
        let config = dir.join("config.ext4");

        let started = (|| {
            let network = self.net.setup(&id, spec.port, spec.host_port)?;
            let script = net::guest_script(network.ip.parse()?, &net::dns_servers());
            copy_reflink(&image.rootfs, &rootfs)?;
            make_config_drive(&config, spec, &image.config, &dir, Some(&script))?;
            let pid = self.launch(&id, spec, &rootfs, &config, &network)?;
            Ok((pid, network))
        })();
        let network = match started {
            Ok((_, network)) => network,
            Err(e) => {
                let e: anyhow::Error = e;
                if let Some(pid) = self.pid(&id) {
                    kill(pid, true);
                }
                self.net.teardown(&id);
                let _ = fs::remove_file(&rootfs);
                return Err(e.context(format!(
                    "failed to start vm {id}; see {}/console.log",
                    dir.display()
                )));
            }
        };
        let info = VmInfo {
            id: id.clone(),
            spec: spec.clone(),
            state: VmState::Running,
            image_digest: image.digest,
            network: Some(network),
        };
        fs::write(dir.join("info.json"), serde_json::to_vec_pretty(&info)?)?;
        Ok(id)
    }

    fn stop(&self, id: &str) -> Result<()> {
        self.read_info(id)?;
        let result = self.stop_process(id);
        // Always release the network, even if the VM already died on its own.
        self.net.teardown(id);
        result
    }

    fn status(&self, id: &str) -> Result<VmInfo> {
        let mut info = self.read_info(id)?;
        info.state = match self.pid(id) {
            Some(pid) if alive(pid) => VmState::Running,
            _ => VmState::Stopped,
        };
        Ok(info)
    }

    fn logs(&self, id: &str, follow: bool) -> Result<Box<dyn Read + Send>> {
        self.read_info(id)?;
        let file = File::open(self.vm_dir(id).join("console.log"))
            .with_context(|| format!("no console log for {id}"))?;
        let pid = self.pid(id);
        Ok(Box::new(LogReader {
            file,
            follow,
            running: Box::new(move || pid.is_some_and(alive)),
        }))
    }
}

impl FirecrackerRuntime {
    fn stop_process(&self, id: &str) -> Result<()> {
        let Some(pid) = self.pid(id).filter(|p| alive(*p)) else {
            return Ok(());
        };
        // Ask the guest to shut down first (x86 only), then force it.
        let sock = self.vm_dir(id).join("api.sock");
        let _ = api_call(
            &sock,
            "PUT",
            "/actions",
            &json!({"action_type": "SendCtrlAltDel"}),
        );
        let deadline = Instant::now() + GRACEFUL_STOP;
        while alive(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        if alive(pid) {
            kill(pid, true);
        }
        let dir = self.vm_dir(id);
        let _ = fs::remove_file(dir.join("rootfs.ext4"));
        let _ = fs::remove_file(dir.join("config.ext4"));
        let _ = fs::remove_file(dir.join("api.sock"));
        Ok(())
    }
}

/// Reads the console log file; in follow mode waits for more output until the
/// VM process is gone and the file is drained.
struct LogReader {
    file: File,
    follow: bool,
    running: Box<dyn Fn() -> bool + Send>,
}

impl Read for LogReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            // Check liveness before reading so output written just before exit is not lost.
            let was_running = (self.running)();
            let n = self.file.read(buf)?;
            if n > 0 || !self.follow || !was_running {
                return Ok(n);
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

/// Last `n` lines of `text`, in order (the in-memory "ring" for `logs --tail`).
pub fn tail_lines(text: &str, n: usize) -> Vec<&str> {
    let mut ring = std::collections::VecDeque::with_capacity(n + 1);
    for line in text.lines() {
        if ring.len() == n {
            ring.pop_front();
        }
        ring.push_back(line);
    }
    ring.into()
}

/// The ordered Firecracker API calls that configure and boot a VM.
pub fn api_requests(
    spec: &VmSpec,
    kernel: &Path,
    rootfs: &Path,
    config: &Path,
    network: Option<&VmNetwork>,
) -> Vec<(&'static str, Value)> {
    let mut reqs = vec![
        (
            "/machine-config",
            json!({"vcpu_count": spec.vcpus, "mem_size_mib": spec.mem_mib}),
        ),
        (
            "/boot-source",
            json!({"kernel_image_path": kernel, "boot_args": BOOT_ARGS}),
        ),
        (
            "/drives/rootfs",
            json!({"drive_id": "rootfs", "path_on_host": rootfs,
                   "is_root_device": true, "is_read_only": false}),
        ),
        (
            "/drives/config",
            json!({"drive_id": "config", "path_on_host": config,
                   "is_root_device": false, "is_read_only": true}),
        ),
    ];
    if let Some(n) = network {
        reqs.push((
            "/network-interfaces/eth0",
            json!({"iface_id": "eth0", "host_dev_name": n.tap, "guest_mac": n.mac}),
        ));
    }
    reqs.push(("/actions", json!({"action_type": "InstanceStart"})));
    reqs
}

/// Raw HTTP/1.1 request for the Firecracker unix-socket API.
pub fn http_request(method: &str, path: &str, body: &Value) -> String {
    let body = body.to_string();
    format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nAccept: application/json\r\n\
Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// Status code from the first line of an HTTP response.
pub fn parse_status(response: &str) -> Option<u16> {
    response
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

fn api_call(sock: &Path, method: &str, path: &str, body: &Value) -> Result<()> {
    let mut stream = UnixStream::connect(sock).context("connecting to firecracker socket")?;
    stream.set_read_timeout(Some(SOCKET_TIMEOUT))?;
    stream.write_all(http_request(method, path, body).as_bytes())?;
    let mut resp = String::new();
    stream.read_to_string(&mut resp)?;
    match parse_status(&resp) {
        Some(200..=299) => Ok(()),
        _ => bail!("firecracker {method} {path} failed: {}", resp.trim()),
    }
}

fn wait_for_socket(sock: &Path) -> Result<()> {
    let deadline = Instant::now() + SOCKET_TIMEOUT;
    while Instant::now() < deadline {
        if UnixStream::connect(sock).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    bail!("firecracker API socket {} did not appear", sock.display())
}

fn alive(pid: u32) -> bool {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    // Field 3 is the state; a zombie has exited.
    stat.rsplit(')')
        .next()
        .and_then(|r| r.split_whitespace().next())
        .is_some_and(|s| s != "Z")
}

fn kill(pid: u32, force: bool) {
    let sig = if force { "-KILL" } else { "-TERM" };
    let _ = Command::new("kill").args([sig, &pid.to_string()]).status();
}

fn copy_reflink(from: &Path, to: &Path) -> Result<()> {
    let status = Command::new("cp")
        .arg("--reflink=auto")
        .arg(from)
        .arg(to)
        .status()
        .context("running cp")?;
    if !status.success() {
        bail!("copying rootfs {} failed", from.display());
    }
    Ok(())
}

fn new_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "vm-{:08x}",
        (nanos ^ u128::from(std::process::id())) & 0xffff_ffff
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> VmSpec {
        VmSpec {
            image: "x".into(),
            vcpus: 2,
            mem_mib: 512,
            env: vec![],
            cmd: vec![],
            port: None,
            host_port: None,
        }
    }

    #[test]
    fn api_sequence_sets_limits_then_boots() {
        let net = VmNetwork {
            ip: "10.200.0.2".into(),
            mac: "AA:FC:00:00:00:02".into(),
            tap: "tw-x".into(),
            host_port: None,
            app_port: None,
        };
        let reqs = api_requests(
            &spec(),
            Path::new("/k"),
            Path::new("/r"),
            Path::new("/c"),
            Some(&net),
        );
        let paths: Vec<_> = reqs.iter().map(|r| r.0).collect();
        assert_eq!(
            paths,
            [
                "/machine-config",
                "/boot-source",
                "/drives/rootfs",
                "/drives/config",
                "/network-interfaces/eth0",
                "/actions"
            ]
        );
        assert_eq!(reqs[0].1, json!({"vcpu_count": 2, "mem_size_mib": 512}));
        assert_eq!(reqs[2].1["is_root_device"], true);
        assert_eq!(reqs[3].1["is_read_only"], true);
        assert_eq!(reqs[4].1["host_dev_name"], "tw-x");
        assert_eq!(reqs[5].1["action_type"], "InstanceStart");
    }

    #[test]
    fn http_request_has_correct_length() {
        let body = json!({"a": 1});
        let req = http_request("PUT", "/x", &body);
        assert!(req.starts_with("PUT /x HTTP/1.1\r\n"));
        assert!(req.contains("Content-Length: 7\r\n"));
        assert!(req.ends_with("{\"a\":1}"));
    }

    #[test]
    fn tail_keeps_last_lines() {
        assert_eq!(tail_lines("a\nb\nc\nd", 2), ["c", "d"]);
        assert_eq!(tail_lines("a\nb", 5), ["a", "b"]);
    }

    #[test]
    fn follow_reader_stops_when_vm_is_gone() {
        let dir = std::env::temp_dir().join(format!("tw-log-{}", std::process::id()));
        fs::write(&dir, "hello\n").unwrap();
        let mut r = LogReader {
            file: File::open(&dir).unwrap(),
            follow: true,
            running: Box::new(|| false),
        };
        let mut out = String::new();
        r.read_to_string(&mut out).unwrap();
        assert_eq!(out, "hello\n");
        let _ = fs::remove_file(&dir);
    }

    #[test]
    fn parses_status_line() {
        assert_eq!(parse_status("HTTP/1.1 204 No Content\r\n\r\n"), Some(204));
        assert_eq!(parse_status("HTTP/1.1 400 Bad Request\r\n"), Some(400));
        assert_eq!(parse_status(""), None);
    }

    #[test]
    fn ids_have_expected_shape() {
        let id = new_id();
        assert!(id.starts_with("vm-") && id.len() == 11);
    }
}
