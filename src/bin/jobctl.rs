use std::error::Error;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};
use tokio::time::sleep;
use tonic::Request;

pub mod node {
    tonic::include_proto!("node");
}

use node::job_service_client::JobServiceClient;
use node::{CancelJobRequest, GetJobStatusRequest, JobPriority, JobRunState, SubmitJobRequest};

#[derive(Parser, Debug)]
#[command(version, about = "Job orchestration CLI", long_about = None)]
struct Args {
    #[arg(long, default_value = "http://[::1]:50051")]
    server: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Submit {
        #[arg(long, default_value = "simulated")]
        kind: String,

        #[arg(long, default_value = "{}")]
        payload: String,

        #[arg(long = "require")]
        required_capabilities: Vec<String>,

        #[arg(long, value_enum, default_value_t = PriorityArg::Normal)]
        priority: PriorityArg,
    },
    Status {
        job_id: String,
    },
    Cancel {
        job_id: String,

        #[arg(long, default_value = "")]
        reason: String,
    },
    Watch {
        job_id: String,

        #[arg(long, default_value_t = 1000)]
        interval_ms: u64,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum PriorityArg {
    Low,
    Normal,
    High,
}

impl PriorityArg {
    fn as_proto(self) -> i32 {
        match self {
            Self::Low => JobPriority::Low as i32,
            Self::Normal => JobPriority::Normal as i32,
            Self::High => JobPriority::High as i32,
        }
    }
}

fn run_state_label(state: i32) -> &'static str {
    match JobRunState::try_from(state).unwrap_or(JobRunState::Unspecified) {
        JobRunState::Queued => "QUEUED",
        JobRunState::Leased => "LEASED",
        JobRunState::Running => "RUNNING",
        JobRunState::Succeeded => "SUCCEEDED",
        JobRunState::Failed => "FAILED",
        JobRunState::CancelRequested => "CANCEL_REQUESTED",
        JobRunState::Cancelled => "CANCELLED",
        JobRunState::Unspecified => "UNSPECIFIED",
    }
}

fn is_terminal_state(state: i32) -> bool {
    matches!(
        JobRunState::try_from(state).unwrap_or(JobRunState::Unspecified),
        JobRunState::Succeeded | JobRunState::Failed | JobRunState::Cancelled
    )
}

fn print_status(state: node::GetJobStatusResponse) {
    println!("job_id: {}", state.job_id);
    println!("state: {}", run_state_label(state.state));

    if !state.assigned_worker_id.is_empty() {
        println!("assigned_worker_id: {}", state.assigned_worker_id);
    }
    if !state.output.is_empty() {
        println!("output: {}", state.output);
    }
    if !state.error.is_empty() {
        println!("error: {}", state.error);
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let mut client = JobServiceClient::connect(args.server).await?;

    match args.command {
        Command::Submit {
            kind,
            payload,
            required_capabilities,
            priority,
        } => {
            let response = client
                .submit_job(Request::new(SubmitJobRequest {
                    kind,
                    payload,
                    required_capabilities,
                    priority: priority.as_proto(),
                }))
                .await?
                .into_inner();

            println!("submitted: {}", response.job_id);
        }
        Command::Status { job_id } => {
            let response = client
                .get_job_status(Request::new(GetJobStatusRequest { job_id }))
                .await?
                .into_inner();
            print_status(response);
        }
        Command::Cancel { job_id, reason } => {
            let response = client
                .cancel_job(Request::new(CancelJobRequest { job_id, reason }))
                .await?
                .into_inner();

            println!("acknowledged: {}", response.acknowledged);
            println!("state: {}", run_state_label(response.state));
        }
        Command::Watch {
            job_id,
            interval_ms,
        } => {
            let poll = Duration::from_millis(interval_ms.max(100));
            let mut last_state = i32::MIN;

            loop {
                let response = client
                    .get_job_status(Request::new(GetJobStatusRequest {
                        job_id: job_id.clone(),
                    }))
                    .await?
                    .into_inner();

                if response.state != last_state {
                    println!("----");
                    print_status(response.clone());
                    last_state = response.state;
                }

                if is_terminal_state(response.state) {
                    break;
                }

                sleep(poll).await;
            }
        }
    }

    Ok(())
}
