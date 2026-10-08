//! Builds an image from a public git repository on this host: with the repo's
//! own Dockerfile (BuildKit) if it has one, else with Nixpacks. Images are
//! kept in the local Docker daemon, tagged per repository and commit, so
//! deploying the same commit again reuses the image.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use trailway_proto::GitSource;
use uuid::Uuid;

/// Prefix the image store understands: the image sits in the local Docker daemon.
pub const DAEMON_PREFIX: &str = "docker-daemon:";
/// A build that runs longer than this (seconds) is killed.
const BUILD_TIMEOUT_SECS: u32 = 30 * 60;

/// A finished build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Built {
    /// Image reference the runtime can prepare, e.g. `docker-daemon:trailway-build/...:<commit>`.
    pub image: String,
    pub commit: String,
    /// The image of this commit already existed, so nothing was built.
    pub reused: bool,
}

/// Turns a repository into an image, writing the build output line by line to `log`.
pub trait Builder {
    fn build(&self, id: Uuid, src: &GitSource, log: &mut dyn FnMut(&str)) -> Result<Built>;
}

pub struct DockerBuilder {
    work_dir: PathBuf,
}

impl DockerBuilder {
    /// Clones go below `work_dir` and are removed after the build.
    pub fn new(work_dir: PathBuf) -> Self {
        Self { work_dir }
    }
}

/// `trailway-build/<repo>:<commit>`, a valid Docker reference.
pub fn image_tag(url: &str, commit: &str) -> String {
    let repo: String = url
        .trim_start_matches("https://")
        .trim_end_matches(".git")
        .chars()
        .map(|c| match c.to_ascii_lowercase() {
            c @ ('a'..='z' | '0'..='9') => c,
            _ => '-',
        })
        .collect();
    let repo = repo.trim_matches('-');
    let repo = &repo[..repo.len().min(80)];
    format!("trailway-build/{repo}:{commit}")
}

fn is_commit(s: &str) -> bool {
    (40..=64).contains(&s.len()) && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Runs `cmd args` in `dir`, merging stderr into stdout and sending each line
/// to `log`. Fails with the exit status when the command does.
fn run_logged(dir: &Path, cmd: &str, args: &[&str], log: &mut dyn FnMut(&str)) -> Result<()> {
    let mut child = Command::new("sh")
        .args(["-c", r#"if command -v timeout >/dev/null 2>&1; then exec timeout "$0" "$@" 2>&1; fi; exec "$@" 2>&1"#])
        .arg(BUILD_TIMEOUT_SECS.to_string())
        .arg(cmd)
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("DOCKER_BUILDKIT", "1")
        .env("DOCKER_CLI_HINTS", "false")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting {cmd} (is it installed? see scripts/setup-host.sh)"))?;
    let mut out = BufReader::new(child.stdout.take().context("no stdout")?);
    let mut line = Vec::new();
    while out.read_until(b'\n', &mut line)? > 0 {
        let mut text = String::from_utf8_lossy(&line).into_owned();
        if !text.ends_with('\n') {
            text.push('\n');
        }
        log(&text);
        line.clear();
    }
    let status = child.wait()?;
    if !status.success() {
        match status.code() {
            Some(124) => bail!("{cmd} timed out after {} minutes", BUILD_TIMEOUT_SECS / 60),
            Some(code) => bail!("{cmd} exited with status {code}"),
            None => bail!("{cmd} was killed"),
        }
    }
    Ok(())
}

/// Runs a command and returns its trimmed stdout, or `None` when it fails.
fn output(dir: &Path, cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd)
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn image_exists(tag: &str) -> bool {
    output(
        Path::new("."),
        "docker",
        &["image", "inspect", "--format", "{{.Id}}", tag],
    )
    .is_some()
}

impl Builder for DockerBuilder {
    fn build(&self, id: Uuid, src: &GitSource, log: &mut dyn FnMut(&str)) -> Result<Built> {
        src.validate().map_err(anyhow::Error::msg)?;
        let reused = |commit: &str| -> Option<Built> {
            let tag = image_tag(&src.url, commit);
            image_exists(&tag).then(|| Built {
                image: format!("{DAEMON_PREFIX}{tag}"),
                commit: commit.to_string(),
                reused: true,
            })
        };

        log(&format!("Resolving {} ({})\n", src.url, src.branch));
        let here = Path::new(".");
        let remote = output(
            here,
            "git",
            &[
                "ls-remote",
                "--exit-code",
                "--",
                &src.url,
                &format!("refs/heads/{}", src.branch),
            ],
        )
        .with_context(|| format!("branch {} not found in {}", src.branch, src.url))?;
        let head = remote.split_whitespace().next().unwrap_or_default();
        if is_commit(head) {
            if let Some(hit) = reused(head) {
                log(&format!(
                    "Commit {head} is already built, reusing its image\n"
                ));
                return Ok(hit);
            }
        }

        let work = self.work_dir.join(id.to_string());
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work)?;
        let result = (|| {
            log("Cloning\n");
            run_logged(
                &work,
                "git",
                &[
                    "clone",
                    "--depth",
                    "1",
                    "--branch",
                    &src.branch,
                    "--",
                    &src.url,
                    "src",
                ],
                log,
            )?;
            let dir = work.join("src");
            let commit = output(&dir, "git", &["rev-parse", "HEAD"]).unwrap_or_default();
            if !is_commit(&commit) {
                bail!("could not read the cloned commit");
            }
            if let Some(hit) = reused(&commit) {
                log(&format!(
                    "Commit {commit} is already built, reusing its image\n"
                ));
                return Ok(hit);
            }
            let tag = image_tag(&src.url, &commit);
            if dir.join("Dockerfile").is_file() {
                log(&format!(
                    "Dockerfile found, building commit {commit} with BuildKit\n"
                ));
                run_logged(
                    &dir,
                    "docker",
                    &[
                        "build",
                        "--progress=plain",
                        "-t",
                        &tag,
                        "-f",
                        "Dockerfile",
                        ".",
                    ],
                    log,
                )?;
            } else {
                log(&format!(
                    "No Dockerfile, building commit {commit} with Nixpacks\n"
                ));
                run_logged(&dir, "nixpacks", &["build", ".", "--name", &tag], log)?;
            }
            log("Build finished\n");
            Ok(Built {
                image: format!("{DAEMON_PREFIX}{tag}"),
                commit,
                reused: false,
            })
        })();
        let _ = std::fs::remove_dir_all(&work);
        result
    }
}

/// Test builder: no network, no Docker. A repo URL containing `broken` fails.
#[derive(Default)]
pub struct FakeBuilder {
    built: std::sync::Mutex<Vec<String>>,
}

impl FakeBuilder {
    /// Commits built so far (reused ones are not counted).
    pub fn builds(&self) -> usize {
        self.built.lock().unwrap().len()
    }
}

impl Builder for FakeBuilder {
    fn build(&self, _id: Uuid, src: &GitSource, log: &mut dyn FnMut(&str)) -> Result<Built> {
        log("Cloning\n");
        if src.url.contains("broken") {
            log("error: the build exploded\n");
            bail!("docker exited with status 1");
        }
        let commit = format!("{:0>40}", src.branch.len());
        let tag = image_tag(&src.url, &commit);
        let mut built = self.built.lock().unwrap();
        let reused = built.contains(&tag);
        if reused {
            log("Reusing image\n");
        } else {
            log("Building\n");
            built.push(tag.clone());
        }
        Ok(Built {
            image: format!("{DAEMON_PREFIX}{tag}"),
            commit,
            reused,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_valid_docker_references() {
        let commit = "a".repeat(40);
        assert_eq!(
            image_tag("https://github.com/Org/My_App.git", &commit),
            format!("trailway-build/github-com-org-my-app:{commit}")
        );
        assert!(image_tag(&"https://x.io/".repeat(30), &commit).len() < 150);
    }

    #[test]
    fn logged_commands_merge_stderr_and_report_failure() {
        let mut lines = vec![];
        let mut log = |l: &str| lines.push(l.to_string());
        run_logged(
            Path::new("."),
            "sh",
            &["-c", "echo out; echo err >&2"],
            &mut log,
        )
        .unwrap();
        assert_eq!(lines, ["out\n", "err\n"]);
        let err = run_logged(
            Path::new("."),
            "sh",
            &["-c", "echo no; exit 3"],
            &mut |_| {},
        )
        .unwrap_err();
        assert!(err.to_string().contains("status 3"));
    }

    /// Needs git, Docker and network: `cargo test -p trailway-agent -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn builds_a_dockerfile_repo_and_reuses_the_image() {
        let dir = std::env::temp_dir().join(format!("tw-build-{}", Uuid::new_v4()));
        let builder = DockerBuilder::new(dir.clone());
        let src = GitSource {
            url: std::env::var("TW_TEST_REPO")
                .unwrap_or_else(|_| "https://github.com/docker/welcome-to-docker".into()),
            branch: std::env::var("TW_TEST_BRANCH").unwrap_or_else(|_| "main".into()),
        };
        let mut log = |l: &str| print!("{l}");
        let first = builder.build(Uuid::new_v4(), &src, &mut log).unwrap();
        assert!(!first.reused);
        let second = builder.build(Uuid::new_v4(), &src, &mut log).unwrap();
        assert!(second.reused);
        assert_eq!(first.image, second.image);
        let broken = GitSource {
            branch: "no-such-branch".into(),
            ..src
        };
        let err = builder
            .build(Uuid::new_v4(), &broken, &mut log)
            .unwrap_err();
        println!("expected failure: {err:#}");
        assert!(!dir.join("nothing").exists());
    }
}
