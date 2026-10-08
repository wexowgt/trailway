use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Written by the installer to `/etc/trailway/agent.json`.
#[derive(Clone, Deserialize)]
pub struct Config {
    pub api: String,
    pub key: String,
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let mut config: Self =
            serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
        config.api = config.api.trim_end_matches('/').to_string();
        anyhow::ensure!(
            config.key.starts_with("tw_sk_"),
            "key must start with tw_sk_"
        );
        anyhow::ensure!(
            config.api.starts_with("http://") || config.api.starts_with("https://"),
            "api must be an http(s) URL"
        );
        Ok(config)
    }
}

/// What the API gave us at registration, kept across restarts.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct State {
    pub server_id: Uuid,
    pub token: String,
}

impl State {
    /// A missing or unreadable state file means "not registered yet".
    pub fn load(path: &Path) -> Option<Self> {
        let raw = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&raw).ok()
    }

    /// Writes atomically with mode 0600 (the token is a credential).
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        use std::io::Write;
        #[cfg(unix)]
        use std::os::unix::fs::OpenOptionsExt;

        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        opts.mode(0o600);
        let mut file = opts.open(&tmp)?;
        file.write_all(serde_json::to_string(self)?.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("tw-agent-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn config_validates_and_trims() {
        let p = tmp("c.json");
        std::fs::write(&p, r#"{"api":"http://x:1/","key":"tw_sk_abc"}"#).unwrap();
        assert_eq!(Config::load(&p).unwrap().api, "http://x:1");
        std::fs::write(&p, r#"{"api":"http://x","key":"nope"}"#).unwrap();
        assert!(Config::load(&p).is_err());
        std::fs::write(&p, r#"{"api":"ftp://x","key":"tw_sk_a"}"#).unwrap();
        assert!(Config::load(&p).is_err());
    }

    #[test]
    fn state_roundtrips_and_missing_is_none() {
        let p = tmp("state.json");
        assert!(State::load(&p).is_none());
        let s = State {
            server_id: Uuid::new_v4(),
            token: "tw_st_x".into(),
        };
        s.save(&p).unwrap();
        assert!(State::load(&p) == Some(s));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
