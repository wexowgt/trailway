//! Guest-side boot scripts. The rootfs ships a static busybox and a generic
//! init; the per-VM command and env live on a small config drive.

use std::collections::BTreeMap;

/// Path of the init binary inside the guest rootfs.
pub const INIT_PATH: &str = "/.trailway/init";
pub const BUSYBOX_PATH: &str = "/.trailway/busybox";

/// Quotes a string for POSIX sh using single quotes.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Generic PID 1: mounts the pseudo filesystems, mounts the config drive
/// (second virtio disk) and runs the generated `run.sh`. Powers off when the
/// workload exits.
pub fn init_script() -> String {
    format!(
        "#!{BUSYBOX_PATH} sh\n\
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/.trailway\n\
B={BUSYBOX_PATH}\n\
$B mount -t proc proc /proc\n\
$B mount -t sysfs sys /sys\n\
$B mount -t devtmpfs dev /dev 2>/dev/null\n\
$B mkdir -p /dev/pts /tmp /run /mnt/cfg\n\
$B mount -t devpts devpts /dev/pts\n\
$B mount -t tmpfs tmpfs /tmp\n\
$B mount -t tmpfs tmpfs /run\n\
$B mount -o ro /dev/vdb /mnt/cfg || {{ echo 'trailway: cannot mount config drive'; $B poweroff -f; }}\n\
$B sh /mnt/cfg/run.sh\n\
echo \"trailway: workload exited with $?\"\n\
$B poweroff -f\n"
    )
}

/// Builds `run.sh`: exports the env, enters the workdir and execs the command.
pub fn run_script(env: &BTreeMap<String, String>, cwd: &str, cmd: &[String]) -> String {
    let mut out = String::from("#!/.trailway/busybox sh\n");
    for (k, v) in env {
        out.push_str(&format!("export {k}={}\n", sh_quote(v)));
    }
    out.push_str(&format!("cd {} || cd /\n", sh_quote(cwd)));
    out.push_str("exec");
    for arg in cmd {
        out.push(' ');
        out.push_str(&sh_quote(arg));
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_single_quotes() {
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        assert_eq!(sh_quote("a b;$x"), "'a b;$x'");
    }

    #[test]
    fn run_script_exports_env_and_execs() {
        let env = BTreeMap::from([("A".to_string(), "1 2".to_string())]);
        let s = run_script(
            &env,
            "/app",
            &["nginx".into(), "-g".into(), "daemon off;".into()],
        );
        assert!(s.contains("export A='1 2'\n"));
        assert!(s.contains("cd '/app' || cd /\n"));
        assert!(s.ends_with("exec 'nginx' '-g' 'daemon off;'\n"));
    }

    #[test]
    fn init_mounts_config_and_powers_off() {
        let s = init_script();
        assert!(s.starts_with("#!/.trailway/busybox sh"));
        assert!(s.contains("/dev/vdb"));
        assert!(s.contains("poweroff -f"));
    }
}
