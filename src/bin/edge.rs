use clap::{Args as ClapArgs, Parser, Subcommand, ValueEnum};
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
    Server(ServerArgs),
    /// Run a worker node
    Node(NodeArgs),
    /// Submit and manage jobs
    Job(JobArgs),
}

const DEFAULT_BIND_ADDR: &str = "[::1]:50051";
const DEFAULT_SERVER_ADDR: &str = "http://[::1]:50051";

#[derive(ClapArgs, Debug)]
struct ServerArgs {
    #[arg(short = 'b', long, default_value = DEFAULT_BIND_ADDR)]
    bind: String,

    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    extra_args: Vec<String>,
}

#[derive(ClapArgs, Debug)]
struct NodeArgs {
    #[arg(short = 's', long, default_value = DEFAULT_SERVER_ADDR)]
    server: String,

    #[arg(short = 'i', long)]
    id: Option<String>,

    #[arg(short = 'p', long, value_enum, default_value_t = NodeProfile::Auto)]
    profile: NodeProfile,

    #[arg(short = 'g', long, default_value_t = 0)]
    gpu_index: u32,

    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    extra_args: Vec<String>,
}

#[derive(ClapArgs, Debug)]
struct JobArgs {
    #[arg(short = 's', long, default_value = DEFAULT_SERVER_ADDR)]
    server: String,

    #[command(subcommand)]
    command: JobCommand,
}

#[derive(Subcommand, Debug)]
enum JobCommand {
    /// Submit a job
    Submit {
        #[arg(long, default_value = "simulated")]
        kind: String,

        #[arg(long, default_value = "{}")]
        payload: String,

        #[arg(long = "require")]
        required_capabilities: Vec<String>,

        #[arg(long, value_enum, default_value_t = JobPriorityArg::Normal)]
        priority: JobPriorityArg,
    },
    /// Fetch a job status once
    Status { job_id: String },
    /// Poll job status until terminal state
    Watch {
        job_id: String,

        #[arg(long, default_value_t = 1000)]
        interval_ms: u64,
    },
    /// Request cancellation for a job
    Cancel {
        job_id: String,

        #[arg(long, default_value = "")]
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum NodeProfile {
    Auto,
    Sim,
    Nvml,
    AmdSysfs,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum JobPriorityArg {
    Low,
    Normal,
    High,
}

impl JobPriorityArg {
    fn as_jobctl_value(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Normal => "normal",
            Self::High => "high",
        }
    }
}

impl NodeProfile {
    fn telemetry_backend(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Sim => "sim",
            Self::Nvml => "nvml",
            Self::AmdSysfs => "amd-sysfs",
        }
    }
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
        EdgeCommand::Server(server) => {
            let mut args = vec!["--bind".to_string(), server.bind];
            args.extend(server.extra_args);
            ("server", args)
        }
        EdgeCommand::Node(node) => {
            let mut args = vec!["--server".to_string(), node.server];

            if let Some(id) = node.id {
                args.push("--id".to_string());
                args.push(id);
            }

            args.push("--telemetry-backend".to_string());
            args.push(node.profile.telemetry_backend().to_string());
            args.push("--gpu-index".to_string());
            args.push(node.gpu_index.to_string());
            args.extend(node.extra_args);

            ("rust-edge-compute", args)
        }
        EdgeCommand::Job(job) => {
            let mut args = vec!["--server".to_string(), job.server];

            match job.command {
                JobCommand::Submit {
                    kind,
                    payload,
                    required_capabilities,
                    priority,
                } => {
                    args.push("submit".to_string());
                    args.push("--kind".to_string());
                    args.push(kind);
                    args.push("--payload".to_string());
                    args.push(payload);

                    for capability in required_capabilities {
                        args.push("--require".to_string());
                        args.push(capability);
                    }

                    args.push("--priority".to_string());
                    args.push(priority.as_jobctl_value().to_string());
                }
                JobCommand::Status { job_id } => {
                    args.push("status".to_string());
                    args.push(job_id);
                }
                JobCommand::Watch {
                    job_id,
                    interval_ms,
                } => {
                    args.push("watch".to_string());
                    args.push(job_id);
                    args.push("--interval-ms".to_string());
                    args.push(interval_ms.to_string());
                }
                JobCommand::Cancel { job_id, reason } => {
                    args.push("cancel".to_string());
                    args.push(job_id);
                    args.push("--reason".to_string());
                    args.push(reason);
                }
            }

            ("jobctl", args)
        }
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
