//! The `Runtime` abstraction the rest of the agent talks to.

use anyhow::{bail, Result};
use std::collections::BTreeMap;
use std::io::{self, Cursor, Read};
use std::sync::Mutex;
use trailway_proto::{VmInfo, VmNetwork, VmSpec, VmState};

pub const MIN_MEM_MIB: u32 = 64;
pub const MAX_VCPUS: u8 = 32;

/// Starts, stops and inspects workloads. `FirecrackerRuntime` is the real
/// implementation; `FakeRuntime` lets tests run without KVM.
pub trait Runtime {
    fn start(&self, spec: &VmSpec) -> Result<String>;
    fn stop(&self, id: &str) -> Result<()>;
    fn status(&self, id: &str) -> Result<VmInfo>;
    /// The VM's console output (kernel and app stdout/stderr). With `follow`
    /// the reader blocks for new output until the VM exits.
    fn logs(&self, id: &str, follow: bool) -> Result<Box<dyn Read + Send>>;

    /// Gets everything `start` needs that can be done ahead of time (pulling
    /// and unpacking the image), so a deploy can report "building" apart from
    /// "deploying". Does nothing by default.
    fn prepare(&self, _spec: &VmSpec) -> Result<()> {
        Ok(())
    }

    /// The console output from byte `offset` on, without following.
    fn logs_from(&self, id: &str, offset: u64) -> Result<Box<dyn Read + Send>> {
        let mut reader = self.logs(id, false)?;
        io::copy(&mut reader.by_ref().take(offset), &mut io::sink())?;
        Ok(reader)
    }
}

/// Rejects specs no runtime can honour.
pub fn validate_spec(spec: &VmSpec) -> Result<()> {
    if spec.image.trim().is_empty() {
        bail!("image must not be empty");
    }
    if spec.vcpus == 0 || spec.vcpus > MAX_VCPUS {
        bail!("vcpus must be between 1 and {MAX_VCPUS}");
    }
    if spec.mem_mib < MIN_MEM_MIB {
        bail!("mem must be at least {MIN_MEM_MIB} MiB");
    }
    if spec.port == Some(0) || spec.host_port == Some(0) {
        bail!("ports must be between 1 and 65535");
    }
    if spec.host_port.is_some() && spec.port.is_none() {
        bail!("host port needs an app port");
    }
    if let Some((k, _)) = spec
        .env
        .iter()
        .find(|(k, _)| k.is_empty() || k.contains('=') || k.contains('\0'))
    {
        bail!("invalid env var name {k:?}");
    }
    Ok(())
}

/// Images with this prefix fail to prepare in the [`FakeRuntime`].
pub const FAIL_IMAGE_PREFIX: &str = "fail/";

/// In-memory runtime for tests (and `TRAILWAY_FAKE_RUNTIME=1` on hosts without KVM).
#[derive(Default)]
pub struct FakeRuntime {
    vms: Mutex<BTreeMap<String, VmInfo>>,
}

impl Runtime for FakeRuntime {
    fn prepare(&self, spec: &VmSpec) -> Result<()> {
        validate_spec(spec)?;
        if spec.image.starts_with(FAIL_IMAGE_PREFIX) {
            bail!("pull access denied for {}", spec.image);
        }
        Ok(())
    }

    fn start(&self, spec: &VmSpec) -> Result<String> {
        self.prepare(spec)?;
        let mut vms = self.vms.lock().unwrap();
        let id = format!("fake-{}", vms.len() + 1);
        let n = vms.len() as u16;
        vms.insert(
            id.clone(),
            VmInfo {
                id: id.clone(),
                spec: spec.clone(),
                state: VmState::Running,
                image_digest: "sha256:fake".into(),
                network: spec.port.map(|app_port| VmNetwork {
                    ip: format!("10.200.0.{}", n + 2),
                    mac: "AA:FC:00:00:00:00".into(),
                    tap: format!("tap-{id}"),
                    host_port: Some(spec.host_port.unwrap_or(30000 + n)),
                    app_port: Some(app_port),
                }),
            },
        );
        Ok(id)
    }

    fn stop(&self, id: &str) -> Result<()> {
        match self.vms.lock().unwrap().get_mut(id) {
            Some(vm) => {
                vm.state = VmState::Stopped;
                Ok(())
            }
            None => bail!("unknown vm {id}"),
        }
    }

    fn status(&self, id: &str) -> Result<VmInfo> {
        match self.vms.lock().unwrap().get(id) {
            Some(vm) => Ok(vm.clone()),
            None => bail!("unknown vm {id}"),
        }
    }

    fn logs(&self, id: &str, _follow: bool) -> Result<Box<dyn Read + Send>> {
        self.status(id)?;
        Ok(Box::new(Cursor::new(format!("fake log for {id}\n"))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> VmSpec {
        VmSpec {
            image: "nginxdemos/hello".into(),
            vcpus: 1,
            mem_mib: 256,
            env: vec![],
            cmd: vec![],
            port: None,
            host_port: None,
        }
    }

    #[test]
    fn start_status_stop_lifecycle() {
        let rt = FakeRuntime::default();
        let id = rt.start(&spec()).unwrap();
        assert_eq!(rt.status(&id).unwrap().state, VmState::Running);
        rt.stop(&id).unwrap();
        assert_eq!(rt.status(&id).unwrap().state, VmState::Stopped);
    }

    #[test]
    fn unknown_vm_is_an_error() {
        let rt = FakeRuntime::default();
        assert!(rt.status("nope").is_err());
        assert!(rt.stop("nope").is_err());
    }

    #[test]
    fn logs_for_known_vm_only() {
        let rt = FakeRuntime::default();
        let id = rt.start(&spec()).unwrap();
        let mut out = String::new();
        rt.logs(&id, false)
            .unwrap()
            .read_to_string(&mut out)
            .unwrap();
        assert!(out.contains(&id));
        assert!(rt.logs("nope", false).is_err());
    }

    #[test]
    fn rejects_bad_specs() {
        let rt = FakeRuntime::default();
        for bad in [
            VmSpec { vcpus: 0, ..spec() },
            VmSpec {
                vcpus: 33,
                ..spec()
            },
            VmSpec {
                mem_mib: 8,
                ..spec()
            },
            VmSpec {
                port: Some(0),
                ..spec()
            },
            VmSpec {
                host_port: Some(8080),
                ..spec()
            },
            VmSpec {
                image: " ".into(),
                ..spec()
            },
            VmSpec {
                env: vec![("A=B".into(), "x".into())],
                ..spec()
            },
        ] {
            assert!(rt.start(&bad).is_err(), "{bad:?}");
        }
    }
}
