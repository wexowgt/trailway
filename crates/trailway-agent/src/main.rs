use anyhow::{bail, Result};
use clap::{Args, Parser, Subcommand};
use std::io::{self, Read, Write};
use trailway_agent::firecracker::{tail_lines, Config, FirecrackerRuntime};
use trailway_agent::runtime::Runtime;
use trailway_proto::VmSpec;

#[derive(Parser)]
#[command(name = "trailway-agent", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Manage microVMs by hand (for testing).
    #[command(subcommand)]
    Vm(VmCommand),
}

#[derive(Subcommand)]
enum VmCommand {
    /// Boot a microVM from an OCI image and print its id.
    Run(RunArgs),
    /// Show a microVM as JSON.
    Status { id: String },
    /// Stop a microVM.
    Stop { id: String },
    /// Print a microVM's console output (kernel and app stdout/stderr).
    Logs {
        id: String,
        /// Keep streaming until the VM exits.
        #[arg(short, long)]
        follow: bool,
        /// Only the last N lines of existing output.
        #[arg(long)]
        tail: Option<usize>,
    },
}

#[derive(Args)]
struct RunArgs {
    #[arg(long)]
    image: String,
    #[arg(long, default_value_t = 1)]
    vcpus: u8,
    /// Memory in MiB.
    #[arg(long = "mem", default_value_t = 256)]
    mem_mib: u32,
    /// Port the app listens on inside the VM; a host port is forwarded to it.
    #[arg(long)]
    port: Option<u16>,
    /// Host port to forward (random free port when omitted).
    #[arg(long)]
    host_port: Option<u16>,
    /// KEY=VALUE, repeatable.
    #[arg(long = "env", value_parser = parse_env)]
    env: Vec<(String, String)>,
    /// Command overriding the image's entrypoint/cmd (after `--`).
    #[arg(last = true)]
    cmd: Vec<String>,
}

fn parse_env(s: &str) -> Result<(String, String), String> {
    s.split_once('=')
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .ok_or_else(|| format!("expected KEY=VALUE, got {s:?}"))
}

fn main() -> Result<()> {
    let Command::Vm(cmd) = Cli::parse().command;
    if !cfg!(target_os = "linux") {
        bail!("vm commands need Linux with KVM");
    }
    let rt = FirecrackerRuntime::new(Config::from_env());
    match cmd {
        VmCommand::Run(a) => {
            let id = rt.start(&VmSpec {
                image: a.image,
                vcpus: a.vcpus,
                mem_mib: a.mem_mib,
                env: a.env,
                cmd: a.cmd,
                port: a.port,
                host_port: a.host_port,
            })?;
            println!("{id}");
            if let Some(n) = rt.status(&id)?.network {
                match n.host_port {
                    Some(p) => {
                        eprintln!("ip {} host port {p} -> {}", n.ip, n.app_port.unwrap_or(0))
                    }
                    None => eprintln!("ip {}", n.ip),
                }
            }
        }
        VmCommand::Status { id } => println!("{}", serde_json::to_string_pretty(&rt.status(&id)?)?),
        VmCommand::Stop { id } => {
            rt.stop(&id)?;
            println!("stopped {id}");
        }
        VmCommand::Logs { id, follow, tail } => {
            let mut reader = rt.logs(&id, follow)?;
            let mut out = io::stdout().lock();
            match tail {
                Some(n) => {
                    let mut text = String::new();
                    reader.read_to_string(&mut text)?;
                    for line in tail_lines(&text, n) {
                        writeln!(out, "{line}")?;
                    }
                }
                None => {
                    io::copy(&mut reader, &mut out)?;
                }
            }
        }
    }
    Ok(())
}
