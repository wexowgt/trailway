//! Turns an OCI image into a bootable ext4 rootfs, cached per image digest.
//!
//! Uses `skopeo` (pull), `umoci` (unpack layers) and `mkfs.ext4 -d` (build).
//! `scripts/setup-host.sh` installs them.

use crate::init::{init_script, run_script, BUSYBOX_PATH, INIT_PATH};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use trailway_proto::VmSpec;

/// Slack added on top of the unpacked size so the guest can write to disk.
const ROOTFS_HEADROOM_MIB: u64 = 128;
const CONFIG_DRIVE_MIB: u64 = 2;

/// Process settings baked into the image (entrypoint, cmd, env, workdir).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageConfig {
    pub args: Vec<String>,
    pub env: Vec<String>,
    pub cwd: String,
}

/// A built rootfs in the cache.
#[derive(Debug, Clone)]
pub struct CachedImage {
    pub digest: String,
    pub rootfs: PathBuf,
    pub config: ImageConfig,
}

pub struct ImageStore {
    root: PathBuf,
    busybox: PathBuf,
}

impl ImageStore {
    pub fn new(root: PathBuf, busybox: PathBuf) -> Self {
        Self { root, busybox }
    }

    /// Directory holding the cached rootfs for one digest.
    pub fn cache_dir(&self, digest: &str) -> PathBuf {
        self.root.join("images").join(digest.replace(':', "-"))
    }

    /// Returns the cached rootfs for `image`, building it on a cache miss.
    pub fn ensure(&self, image: &str) -> Result<CachedImage> {
        let digest = resolve_digest(image)?;
        let dir = self.cache_dir(&digest);
        if let Some(hit) = load_cached(&dir, &digest)? {
            return Ok(hit);
        }
        let tmp = dir.with_extension("building");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp)?;
        let built = self.build(image, &digest, &tmp);
        if built.is_err() {
            let _ = fs::remove_dir_all(&tmp);
        }
        let config = built?;
        let _ = fs::remove_dir_all(&dir);
        fs::rename(&tmp, &dir).context("publishing rootfs cache entry")?;
        Ok(CachedImage {
            digest,
            rootfs: dir.join("rootfs.ext4"),
            config,
        })
    }

    fn build(&self, image: &str, digest: &str, out: &Path) -> Result<ImageConfig> {
        let oci = out.join("oci");
        let bundle = out.join("bundle");
        // Pin the pull to the digest we resolved so the cache key is truthful.
        let pinned = pinned_reference(image, digest);
        run(Command::new("skopeo")
            .arg("copy")
            .arg(format!("docker://{pinned}"))
            .arg(format!("oci:{}:img", oci.display())))?;
        run(Command::new("umoci")
            .args(["unpack", "--image"])
            .arg(format!("{}:img", oci.display()))
            .arg(&bundle))?;

        let config = parse_oci_config(&fs::read_to_string(bundle.join("config.json"))?)?;
        let rootfs_dir = bundle.join("rootfs");
        let guest = rootfs_dir.join(".trailway");
        fs::create_dir_all(&guest)?;
        fs::copy(
            &self.busybox,
            rootfs_dir.join(BUSYBOX_PATH.trim_start_matches('/')),
        )
        .with_context(|| format!("copying busybox from {}", self.busybox.display()))?;
        fs::write(
            rootfs_dir.join(INIT_PATH.trim_start_matches('/')),
            init_script(),
        )?;
        for f in ["busybox", "init"] {
            fs::set_permissions(guest.join(f), fs::Permissions::from_mode(0o755))?;
        }

        let size = dir_size_mib(&rootfs_dir)? + ROOTFS_HEADROOM_MIB;
        make_ext4(&out.join("rootfs.ext4"), &rootfs_dir, size)?;
        fs::write(out.join("config.json"), serde_json::to_vec(&config)?)?;
        fs::write(out.join("digest"), digest)?;
        fs::remove_dir_all(&oci)?;
        fs::remove_dir_all(&bundle)?;
        Ok(config)
    }
}

fn load_cached(dir: &Path, digest: &str) -> Result<Option<CachedImage>> {
    let (rootfs, cfg) = (dir.join("rootfs.ext4"), dir.join("config.json"));
    if !rootfs.exists() || !cfg.exists() {
        return Ok(None);
    }
    Ok(Some(CachedImage {
        digest: digest.to_string(),
        rootfs,
        config: serde_json::from_slice(&fs::read(cfg)?)?,
    }))
}

/// `nginx:1.2` + `sha256:abc` -> `nginx@sha256:abc` (tag dropped, digest wins).
pub fn pinned_reference(image: &str, digest: &str) -> String {
    let name = image.split('@').next().unwrap_or(image);
    // A colon after the last slash is a tag, not a registry port.
    let name = match name.rfind(':') {
        Some(i) if !name[i..].contains('/') => &name[..i],
        _ => name,
    };
    format!("{name}@{digest}")
}

fn resolve_digest(image: &str) -> Result<String> {
    let out = Command::new("skopeo")
        .args(["inspect", "--format", "{{.Digest}}"])
        .arg(format!("docker://{image}"))
        .output()
        .context("running skopeo (is it installed? see scripts/setup-host.sh)")?;
    if !out.status.success() {
        bail!(
            "skopeo inspect {image} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let digest = String::from_utf8(out.stdout)?.trim().to_string();
    if !digest.starts_with("sha256:") {
        bail!("unexpected digest {digest:?} for {image}");
    }
    Ok(digest)
}

#[derive(Deserialize)]
struct OciRuntimeSpec {
    process: OciProcess,
}

#[derive(Deserialize)]
struct OciProcess {
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: Vec<String>,
    #[serde(default)]
    cwd: String,
}

/// Reads the process section of the runtime spec `umoci unpack` writes.
pub fn parse_oci_config(json: &str) -> Result<ImageConfig> {
    let spec: OciRuntimeSpec = serde_json::from_str(json).context("parsing OCI config.json")?;
    let p = spec.process;
    Ok(ImageConfig {
        args: p.args,
        env: p.env,
        cwd: if p.cwd.is_empty() { "/".into() } else { p.cwd },
    })
}

/// Final env (image env, then spec overrides) and command for a VM.
pub fn resolve_process(
    spec: &VmSpec,
    image: &ImageConfig,
) -> Result<(BTreeMap<String, String>, Vec<String>)> {
    let mut env: BTreeMap<String, String> = image
        .env
        .iter()
        .filter_map(|e| e.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    env.extend(spec.env.iter().cloned());
    let cmd = if spec.cmd.is_empty() {
        image.args.clone()
    } else {
        spec.cmd.clone()
    };
    if cmd.is_empty() {
        bail!("image has no entrypoint or cmd and the spec gave none");
    }
    Ok((env, cmd))
}

/// Builds the small ext4 config drive holding `run.sh` for one VM.
pub fn make_config_drive(
    path: &Path,
    spec: &VmSpec,
    image: &ImageConfig,
    scratch: &Path,
    net_script: Option<&str>,
) -> Result<()> {
    let (env, cmd) = resolve_process(spec, image)?;
    let dir = scratch.join("cfg");
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("run.sh"), run_script(&env, &image.cwd, &cmd))?;
    if let Some(net) = net_script {
        fs::write(dir.join("net.sh"), net)?;
    }
    make_ext4(path, &dir, CONFIG_DRIVE_MIB)
}

fn make_ext4(image: &Path, dir: &Path, size_mib: u64) -> Result<()> {
    run(Command::new("mkfs.ext4")
        .args(["-q", "-F", "-O", "^has_journal", "-d"])
        .arg(dir)
        .arg(image)
        .arg(format!("{size_mib}M")))
}

fn dir_size_mib(dir: &Path) -> Result<u64> {
    let out = Command::new("du").args(["-sm"]).arg(dir).output()?;
    if !out.status.success() {
        bail!("du failed on {}", dir.display());
    }
    String::from_utf8(out.stdout)?
        .split_whitespace()
        .next()
        .and_then(|n| n.parse().ok())
        .context("parsing du output")
}

fn run(cmd: &mut Command) -> Result<()> {
    let out = cmd.output().with_context(|| format!("spawning {cmd:?}"))?;
    if !out.status.success() {
        bail!(
            "{cmd:?} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CFG: &str = r#"{"ociVersion":"1.0.2","process":{"args":["nginx","-g","daemon off;"],
        "env":["PATH=/bin","NGINX_VERSION=1.2"],"cwd":"/"},"root":{"path":"rootfs"}}"#;

    fn spec() -> VmSpec {
        VmSpec {
            image: "x".into(),
            vcpus: 1,
            mem_mib: 256,
            env: vec![],
            cmd: vec![],
            port: None,
            host_port: None,
        }
    }

    #[test]
    fn parses_process_section() {
        let c = parse_oci_config(CFG).unwrap();
        assert_eq!(c.args, ["nginx", "-g", "daemon off;"]);
        assert_eq!(c.cwd, "/");
        assert_eq!(c.env.len(), 2);
    }

    #[test]
    fn spec_env_overrides_image_env_and_cmd_overrides_args() {
        let c = parse_oci_config(CFG).unwrap();
        let s = VmSpec {
            env: vec![("NGINX_VERSION".into(), "9".into())],
            cmd: vec!["nproc".into()],
            ..spec()
        };
        let (env, cmd) = resolve_process(&s, &c).unwrap();
        assert_eq!(env["NGINX_VERSION"], "9");
        assert_eq!(env["PATH"], "/bin");
        assert_eq!(cmd, ["nproc"]);
    }

    #[test]
    fn image_defaults_used_without_override() {
        let c = parse_oci_config(CFG).unwrap();
        let (_, cmd) = resolve_process(&spec(), &c).unwrap();
        assert_eq!(cmd[0], "nginx");
    }

    #[test]
    fn errors_when_nothing_to_run() {
        let c = ImageConfig {
            args: vec![],
            env: vec![],
            cwd: "/".into(),
        };
        assert!(resolve_process(&spec(), &c).is_err());
    }

    #[test]
    fn pins_reference_to_digest() {
        assert_eq!(pinned_reference("nginx:1.2", "sha256:a"), "nginx@sha256:a");
        assert_eq!(
            pinned_reference("localhost:5000/app", "sha256:a"),
            "localhost:5000/app@sha256:a"
        );
        assert_eq!(pinned_reference("a/b@sha256:z", "sha256:a"), "a/b@sha256:a");
    }

    #[test]
    fn cache_dir_is_keyed_by_digest() {
        let s = ImageStore::new("/d".into(), "/bb".into());
        assert_eq!(
            s.cache_dir("sha256:ab"),
            PathBuf::from("/d/images/sha256-ab")
        );
    }

    #[test]
    fn cache_hit_needs_both_files() {
        let tmp = std::env::temp_dir().join(format!("tw-cache-{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();
        assert!(load_cached(&tmp, "sha256:a").unwrap().is_none());
        fs::write(tmp.join("rootfs.ext4"), b"x").unwrap();
        fs::write(
            tmp.join("config.json"),
            serde_json::to_vec(&parse_oci_config(CFG).unwrap()).unwrap(),
        )
        .unwrap();
        assert!(load_cached(&tmp, "sha256:a").unwrap().is_some());
        fs::remove_dir_all(&tmp).unwrap();
    }
}
