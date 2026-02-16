use clap::{Parser, Subcommand};
use std::io;
use std::path::Path;
use std::process::{self, Command, ExitStatus, Stdio};

#[derive(Parser, Debug)]
#[command(version, about = "Unified edge command", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: EdgeCommand,
}

#[derive(Subcommand, Debug)]
enum EdgeCommand {
    /// Run the orchestrator server
    Server {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run a worker node
    Node {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Submit and manage jobs
    Job {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

fn spawn_with_stdio(mut command: Command, args: &[String]) -> io::Result<ExitStatus> {
    command
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
}

fn sibling_binary_path(binary: &str) -> Option<std::path::PathBuf> {
    let current_exe = std::env::current_exe().ok()?;
    let exe_dir = current_exe.parent()?;
    let suffix = std::env::consts::EXE_SUFFIX;
    let candidate = exe_dir.join(format!("{binary}{suffix}"));
    candidate.is_file().then_some(candidate)
}

fn run_legacy_binary(binary: &str, args: &[String]) -> io::Result<ExitStatus> {
    if let Some(path) = sibling_binary_path(binary) {
        match spawn_with_stdio(Command::new(path), args) {
            Ok(status) => return Ok(status),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
    }

    match spawn_with_stdio(Command::new(binary), args) {
        Ok(status) => Ok(status),
        Err(err) if err.kind() == io::ErrorKind::NotFound && Path::new("Cargo.toml").is_file() => {
            let mut cargo_cmd = Command::new("cargo");
            cargo_cmd.args(["run", "--bin", binary, "--"]);
            spawn_with_stdio(cargo_cmd, args)
        }
        Err(err) => Err(err),
    }
}

fn forward(command: EdgeCommand) -> (&'static str, Vec<String>) {
    match command {
        EdgeCommand::Server { args } => ("server", args),
        EdgeCommand::Node { args } => ("rust-edge-compute", args),
        EdgeCommand::Job { args } => ("jobctl", args),
    }
}

fn main() {
    let cli = Cli::parse();
    let (binary, args) = forward(cli.command);

    match run_legacy_binary(binary, &args) {
        Ok(status) => {
            let code = status.code().unwrap_or(1);
            process::exit(code);
        }
        Err(err) => {
            eprintln!("failed to start '{binary}': {err}");
            process::exit(1);
        }
    }
}
