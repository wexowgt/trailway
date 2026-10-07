//! The `Runtime` abstraction the rest of the agent talks to.

use anyhow::{bail, Result};
use std::collections::BTreeMap;
use std::sync::Mutex;
use trailway_proto::{VmInfo, VmSpec, VmState};

pub const MIN_MEM_MIB: u32 = 64;
pub const MAX_VCPUS: u8 = 32;

/// Starts, stops and inspects workloads. `FirecrackerRuntime` is the real
/// implementation; `FakeRuntime` lets tests run without KVM.
pub trait Runtime {
    fn start(&self, spec: &VmSpec) -> Result<String>;
    fn stop(&self, id: &str) -> Result<()>;
    fn status(&self, id: &str) -> Result<VmInfo>;
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
    if let Some((k, _)) = spec
        .env
        .iter()
        .find(|(k, _)| k.is_empty() || k.contains('=') || k.contains('\0'))
    {
        bail!("invalid env var name {k:?}");
    }
    Ok(())
}

/// In-memory runtime for tests.
#[derive(Default)]
pub struct FakeRuntime {
    vms: Mutex<BTreeMap<String, VmInfo>>,
}

impl Runtime for FakeRuntime {
    fn start(&self, spec: &VmSpec) -> Result<String> {
        validate_spec(spec)?;
        let mut vms = self.vms.lock().unwrap();
        let id = format!("fake-{}", vms.len() + 1);
        vms.insert(
            id.clone(),
            VmInfo {
                id: id.clone(),
                spec: spec.clone(),
                state: VmState::Running,
                image_digest: "sha256:fake".into(),
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
