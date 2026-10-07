use anyhow::{bail, Result};
use clap::{Args, Parser, Subcommand};
use trailway_agent::firecracker::{Config, FirecrackerRuntime};
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
            })?;
            println!("{id}");
        }
        VmCommand::Status { id } => println!("{}", serde_json::to_string_pretty(&rt.status(&id)?)?),
        VmCommand::Stop { id } => {
            rt.stop(&id)?;
            println!("stopped {id}");
        }
    }
    Ok(())
}
