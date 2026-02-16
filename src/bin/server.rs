use std::error::Error;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use clap::Parser;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use dashmap::{mapref::entry::Entry, DashMap};
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
};
use tokio::sync::watch;
use tonic::{transport::Server, Request, Response, Status};

pub mod node {
    tonic::include_proto!("node");
}

use node::job_service_server::{JobService, JobServiceServer};
use node::node_service_server::{NodeService, NodeServiceServer};
use node::{
    DisconnectRequest, DisconnectResponse, GetJobStatusRequest, GetJobStatusResponse,
    HeartbeatRequest, HeartbeatResponse, JobRunState, LeaseJobRequest, LeaseJobResponse,
    ReportJobResultRequest, ReportJobResultResponse, SubmitJobRequest, SubmitJobResponse,
};

const DEFAULT_BIND_ADDR: &str = "[::1]:50051";
const STALE_AFTER: Duration = Duration::from_secs(10);
const EVICT_AFTER: Duration = Duration::from_secs(60);
const FRAME_POLL: Duration = Duration::from_millis(100);

#[derive(Parser, Debug)]
#[command(version, about = "Rust Edge Orchestrator", long_about = None)]
struct Args {
    #[arg(long, default_value = DEFAULT_BIND_ADDR)]
    bind: String,
}

#[derive(Debug, Clone)]
struct NodeStatus {
    node_id: String,
    last_temp_c: f32,
    last_usage: f32,
    last_vram_used_bytes: u64,
    last_uptime_seconds: u64,
    client_version: String,
    last_seen: Instant,
    heartbeat_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeHealth {
    Healthy,
    Stale,
}

impl NodeHealth {
    fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "Healthy",
            Self::Stale => "Stale",
        }
    }
}

#[derive(Debug, Clone)]
struct NodeRow {
    node_id: String,
    last_temp_c: f32,
    last_usage: f32,
    last_vram_used_bytes: u64,
    last_uptime_seconds: u64,
    client_version: String,
    last_seen_secs: u64,
    heartbeat_count: u64,
    health: NodeHealth,
}

#[derive(Debug, Clone)]
struct DashboardSnapshot {
    rows: Vec<NodeRow>,
    total_nodes: usize,
    healthy_nodes: usize,
    stale_nodes: usize,
    total_evicted_nodes: u64,
    total_heartbeats: u64,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobState {
    Queued,
    Leased,
    Running,
    Succeeded,
    Failed,
}

impl JobState {
    fn as_proto(self) -> JobRunState {
        match self {
            Self::Queued => JobRunState::Queued,
            Self::Leased => JobRunState::Leased,
            Self::Running => JobRunState::Running,
            Self::Succeeded => JobRunState::Succeeded,
            Self::Failed => JobRunState::Failed,
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
struct JobLease {
    worker_id: String,
    leased_at: Instant,
    lease_timeout: Duration,
}

#[allow(dead_code)]
impl JobLease {
    fn is_expired(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.leased_at) > self.lease_timeout
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
struct JobRecord {
    job_id: String,
    kind: String,
    payload: String,
    state: JobState,
    lease: Option<JobLease>,
    output: Option<String>,
    error: Option<String>,
    created_at: Instant,
    updated_at: Instant,
}

#[derive(Debug, Clone)]
pub struct MyNodeService {
    state: Arc<DashMap<String, NodeStatus>>,
    total_heartbeats: Arc<AtomicU64>,
    jobs: Arc<DashMap<String, JobRecord>>,
    job_sequence: Arc<AtomicU64>,
}

fn update_node_status(
    state: &DashMap<String, NodeStatus>,
    node_id: &str,
    heartbeat: &HeartbeatRequest,
    now: Instant,
) {
    match state.entry(node_id.to_string()) {
        Entry::Occupied(mut entry) => {
            let status = entry.get_mut();
            status.last_temp_c = heartbeat.gpu_temp;
            status.last_usage = heartbeat.gpu_usage;
            status.last_vram_used_bytes = heartbeat.vram_used_bytes;
            status.last_uptime_seconds = heartbeat.uptime_seconds;
            status.client_version = heartbeat.client_version.clone();
            status.last_seen = now;
            status.heartbeat_count += 1;
        }
        Entry::Vacant(entry) => {
            entry.insert(NodeStatus {
                node_id: node_id.to_string(),
                last_temp_c: heartbeat.gpu_temp,
                last_usage: heartbeat.gpu_usage,
                last_vram_used_bytes: heartbeat.vram_used_bytes,
                last_uptime_seconds: heartbeat.uptime_seconds,
                client_version: heartbeat.client_version.clone(),
                last_seen: now,
                heartbeat_count: 1,
            });
        }
    }
}

fn remove_node(state: &DashMap<String, NodeStatus>, node_id: &str) -> bool {
    state.remove(node_id).is_some()
}

fn node_health(last_seen: Instant, now: Instant, stale_after: Duration) -> NodeHealth {
    let age = now.saturating_duration_since(last_seen);
    if age > stale_after {
        NodeHealth::Stale
    } else {
        NodeHealth::Healthy
    }
}

fn prune_stale_nodes(
    state: &DashMap<String, NodeStatus>,
    now: Instant,
    evict_after: Duration,
) -> usize {
    let keys_to_remove: Vec<String> = state
        .iter()
        .filter_map(|entry| {
            let status = entry.value();
            let age = now.saturating_duration_since(status.last_seen);
            if age > evict_after {
                Some(entry.key().clone())
            } else {
                None
            }
        })
        .collect();

    keys_to_remove
        .iter()
        .filter(|node_id| state.remove(*node_id).is_some())
        .count()
}

fn build_dashboard_snapshot(
    state: &DashMap<String, NodeStatus>,
    total_heartbeats: u64,
    total_evicted_nodes: u64,
    stale_after: Duration,
) -> DashboardSnapshot {
    let now = Instant::now();
    let mut rows = Vec::new();
    let mut healthy_nodes = 0usize;
    let mut stale_nodes = 0usize;

    for entry in state.iter() {
        let status = entry.value();
        let age = now.saturating_duration_since(status.last_seen);
        let health = node_health(status.last_seen, now, stale_after);

        match health {
            NodeHealth::Healthy => healthy_nodes += 1,
            NodeHealth::Stale => stale_nodes += 1,
        }

        rows.push(NodeRow {
            node_id: status.node_id.clone(),
            last_temp_c: status.last_temp_c,
            last_usage: status.last_usage,
            last_vram_used_bytes: status.last_vram_used_bytes,
            last_uptime_seconds: status.last_uptime_seconds,
            client_version: status.client_version.clone(),
            last_seen_secs: age.as_secs(),
            heartbeat_count: status.heartbeat_count,
            health,
        });
    }

    rows.sort_by(|a, b| a.node_id.cmp(&b.node_id));

    DashboardSnapshot {
        total_nodes: rows.len(),
        healthy_nodes,
        stale_nodes,
        total_evicted_nodes,
        total_heartbeats,
        rows,
    }
}

fn is_quit_key(key: KeyEvent) -> bool {
    key.code == KeyCode::Char('q')
        || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
}

fn render_dashboard(frame: &mut ratatui::Frame, snapshot: &DashboardSnapshot) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(3),
        ])
        .split(frame.area());

    let summary = Paragraph::new(format!(
        "Active: {}  |  Stale: {}  |  Evicted: {}  |  Total Nodes: {}  |  Total Heartbeats: {}",
        snapshot.healthy_nodes,
        snapshot.stale_nodes,
        snapshot.total_evicted_nodes,
        snapshot.total_nodes,
        snapshot.total_heartbeats
    ))
    .block(
        Block::default()
            .title(" Cluster Summary ")
            .borders(Borders::ALL),
    );
    frame.render_widget(summary, chunks[0]);

    let table_rows = snapshot.rows.iter().map(|row| {
        let status_style = match row.health {
            NodeHealth::Healthy => Style::default().fg(Color::Green),
            NodeHealth::Stale => Style::default().fg(Color::Yellow),
        };

        Row::new(vec![
            Cell::from(row.node_id.clone()),
            Cell::from(format!("{:.1}", row.last_temp_c)),
            Cell::from(format!("{:.0}%", row.last_usage * 100.0)),
            Cell::from(format!(
                "{:.2}",
                row.last_vram_used_bytes as f64 / 1_073_741_824.0
            )),
            Cell::from(format!("{}s", row.last_uptime_seconds)),
            Cell::from(format!("{}s ago", row.last_seen_secs)),
            Cell::from(row.health.as_str()).style(status_style),
            Cell::from(row.heartbeat_count.to_string()),
            Cell::from(row.client_version.clone()),
        ])
    });

    let table = Table::new(
        table_rows,
        [
            Constraint::Length(16),
            Constraint::Length(9),
            Constraint::Length(8),
            Constraint::Length(10),
            Constraint::Length(9),
            Constraint::Length(12),
            Constraint::Length(10),
            Constraint::Length(11),
            Constraint::Min(8),
        ],
    )
    .header(Row::new([
        "Node ID",
        "Temp (C)",
        "Usage",
        "VRAM (GiB)",
        "Uptime",
        "Last Seen",
        "Status",
        "Heartbeats",
        "Version",
    ]))
    .block(
        Block::default()
            .title(" Connected Nodes ")
            .borders(Borders::ALL),
    );
    frame.render_widget(table, chunks[1]);

    let footer = Paragraph::new("Press q or Ctrl+C to stop the orchestrator")
        .block(Block::default().title(" Controls ").borders(Borders::ALL));
    frame.render_widget(footer, chunks[2]);
}

async fn run_dashboard(
    state: Arc<DashMap<String, NodeStatus>>,
    total_heartbeats: Arc<AtomicU64>,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
) -> Result<(), Box<dyn Error>> {
    let mut terminal = ratatui::init();
    let dashboard_result = run_dashboard_loop(
        &mut terminal,
        state,
        total_heartbeats,
        shutdown_tx,
        shutdown_rx,
    )
    .await;
    ratatui::restore();
    dashboard_result
}

async fn run_dashboard_loop(
    terminal: &mut ratatui::DefaultTerminal,
    state: Arc<DashMap<String, NodeStatus>>,
    total_heartbeats: Arc<AtomicU64>,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
) -> Result<(), Box<dyn Error>> {
    let mut total_evicted_nodes = 0u64;

    loop {
        if *shutdown_rx.borrow() {
            break;
        }

        total_evicted_nodes = total_evicted_nodes.saturating_add(prune_stale_nodes(
            &state,
            Instant::now(),
            EVICT_AFTER,
        ) as u64);

        let snapshot = build_dashboard_snapshot(
            &state,
            total_heartbeats.load(Ordering::Relaxed),
            total_evicted_nodes,
            STALE_AFTER,
        );
        terminal.draw(|frame| render_dashboard(frame, &snapshot))?;

        if crossterm::event::poll(FRAME_POLL)? {
            if let Event::Key(key) = crossterm::event::read()? {
                if is_quit_key(key) {
                    let _ = shutdown_tx.send(true);
                    break;
                }
            }
        }
    }

    Ok(())
}

#[tonic::async_trait]
impl NodeService for MyNodeService {
    async fn heartbeat(
        &self,
        request: Request<HeartbeatRequest>,
    ) -> Result<Response<HeartbeatResponse>, Status> {
        let req = request.into_inner();
        let now = Instant::now();

        update_node_status(&self.state, &req.node_id, &req, now);
        self.total_heartbeats.fetch_add(1, Ordering::Relaxed);

        Ok(Response::new(HeartbeatResponse { acknowledged: true }))
    }

    async fn disconnect(
        &self,
        request: Request<DisconnectRequest>,
    ) -> Result<Response<DisconnectResponse>, Status> {
        let req = request.into_inner();
        remove_node(&self.state, &req.node_id);

        Ok(Response::new(DisconnectResponse { acknowledged: true }))
    }
}

#[tonic::async_trait]
impl JobService for MyNodeService {
    async fn submit_job(
        &self,
        request: Request<SubmitJobRequest>,
    ) -> Result<Response<SubmitJobResponse>, Status> {
        let req = request.into_inner();

        if req.kind.trim().is_empty() {
            return Err(Status::invalid_argument("job kind cannot be empty"));
        }

        let now = Instant::now();
        let job_seq = self.job_sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let job_id = format!("job-{job_seq:06}");

        self.jobs.insert(
            job_id.clone(),
            JobRecord {
                job_id: job_id.clone(),
                kind: req.kind,
                payload: req.payload,
                state: JobState::Queued,
                lease: None,
                output: None,
                error: None,
                created_at: now,
                updated_at: now,
            },
        );

        Ok(Response::new(SubmitJobResponse { job_id }))
    }

    async fn lease_job(
        &self,
        _request: Request<LeaseJobRequest>,
    ) -> Result<Response<LeaseJobResponse>, Status> {
        Err(Status::unimplemented(
            "job leasing is not implemented yet in this sprint step",
        ))
    }

    async fn report_job_result(
        &self,
        _request: Request<ReportJobResultRequest>,
    ) -> Result<Response<ReportJobResultResponse>, Status> {
        Err(Status::unimplemented(
            "job result reporting is not implemented yet in this sprint step",
        ))
    }

    async fn get_job_status(
        &self,
        request: Request<GetJobStatusRequest>,
    ) -> Result<Response<GetJobStatusResponse>, Status> {
        let req = request.into_inner();

        if req.job_id.trim().is_empty() {
            return Err(Status::invalid_argument("job_id cannot be empty"));
        }

        let job = self
            .jobs
            .get(&req.job_id)
            .ok_or_else(|| Status::not_found(format!("job {} not found", req.job_id)))?;

        let state = job.state.as_proto() as i32;
        let assigned_worker_id = job
            .lease
            .as_ref()
            .map(|lease| lease.worker_id.clone())
            .unwrap_or_default();

        Ok(Response::new(GetJobStatusResponse {
            job_id: job.job_id.clone(),
            state,
            assigned_worker_id,
            output: job.output.clone().unwrap_or_default(),
            error: job.error.clone().unwrap_or_default(),
        }))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let addr = args.bind.parse()?;
    println!("Orchestrator listening on {}", addr);

    let state = Arc::new(DashMap::new());
    let total_heartbeats = Arc::new(AtomicU64::new(0));
    let jobs = Arc::new(DashMap::new());
    let job_sequence = Arc::new(AtomicU64::new(0));

    let service = MyNodeService {
        state: state.clone(),
        total_heartbeats: total_heartbeats.clone(),
        jobs: jobs.clone(),
        job_sequence: job_sequence.clone(),
    };

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut server_shutdown_rx = shutdown_rx.clone();

    let server_task = tokio::spawn(async move {
        Server::builder()
            .add_service(NodeServiceServer::new(service.clone()))
            .add_service(JobServiceServer::new(service))
            .serve_with_shutdown(addr, async move {
                while !*server_shutdown_rx.borrow() {
                    if server_shutdown_rx.changed().await.is_err() {
                        break;
                    }
                }
            })
            .await
    });

    let signal_shutdown_tx = shutdown_tx.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = signal_shutdown_tx.send(true);
        }
    });

    let dashboard_result = run_dashboard(
        state,
        total_heartbeats,
        shutdown_tx.clone(),
        shutdown_rx.clone(),
    )
    .await;

    let _ = shutdown_tx.send(true);

    let server_result = match server_task.await {
        Ok(result) => result,
        Err(err) => return Err(Box::new(err) as Box<dyn Error>),
    };

    dashboard_result?;
    server_result?;

    println!("Orchestrator stopped.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_service() -> MyNodeService {
        MyNodeService {
            state: Arc::new(DashMap::new()),
            total_heartbeats: Arc::new(AtomicU64::new(0)),
            jobs: Arc::new(DashMap::new()),
            job_sequence: Arc::new(AtomicU64::new(0)),
        }
    }

    fn heartbeat(
        node_id: &str,
        gpu_temp: f32,
        gpu_usage: f32,
        vram_used_bytes: u64,
        uptime_seconds: u64,
        client_version: &str,
    ) -> HeartbeatRequest {
        HeartbeatRequest {
            node_id: node_id.to_string(),
            gpu_temp,
            gpu_usage,
            vram_used_bytes,
            uptime_seconds,
            client_version: client_version.to_string(),
        }
    }

    #[test]
    fn update_node_status_inserts_and_updates_existing_node() {
        let state = DashMap::new();
        let first = Instant::now();
        let second = first + Duration::from_secs(2);

        update_node_status(
            &state,
            "Node-1",
            &heartbeat("Node-1", 63.5, 0.44, 2_000_000_000, 120, "0.1.0"),
            first,
        );
        update_node_status(
            &state,
            "Node-1",
            &heartbeat("Node-1", 71.2, 0.86, 4_000_000_000, 122, "0.1.0"),
            second,
        );

        let node = state.get("Node-1").expect("node should exist");
        assert_eq!(node.heartbeat_count, 2);
        assert_eq!(node.last_temp_c, 71.2);
        assert!((node.last_usage - 0.86).abs() < f32::EPSILON);
        assert_eq!(node.last_vram_used_bytes, 4_000_000_000);
        assert_eq!(node.last_uptime_seconds, 122);
        assert_eq!(node.client_version, "0.1.0");
        assert_eq!(node.last_seen, second);
    }

    #[test]
    fn node_health_marks_stale_after_threshold() {
        let now = Instant::now();
        let fresh_seen = now - Duration::from_secs(5);
        let stale_seen = now - Duration::from_secs(15);

        assert_eq!(
            node_health(fresh_seen, now, STALE_AFTER),
            NodeHealth::Healthy
        );
        assert_eq!(node_health(stale_seen, now, STALE_AFTER), NodeHealth::Stale);
    }

    #[test]
    fn build_dashboard_snapshot_handles_empty_state() {
        let state = DashMap::new();
        let snapshot = build_dashboard_snapshot(&state, 0, 0, STALE_AFTER);

        assert_eq!(snapshot.total_nodes, 0);
        assert_eq!(snapshot.healthy_nodes, 0);
        assert_eq!(snapshot.stale_nodes, 0);
        assert_eq!(snapshot.total_evicted_nodes, 0);
        assert_eq!(snapshot.total_heartbeats, 0);
        assert!(snapshot.rows.is_empty());
    }

    #[test]
    fn build_dashboard_snapshot_sorts_rows_and_counts_health() {
        let state = DashMap::new();
        let now = Instant::now();

        state.insert(
            "Node-B".to_string(),
            NodeStatus {
                node_id: "Node-B".to_string(),
                last_temp_c: 60.0,
                last_usage: 0.52,
                last_vram_used_bytes: 3_000_000_000,
                last_uptime_seconds: 40,
                client_version: "0.1.0".to_string(),
                last_seen: now - Duration::from_secs(3),
                heartbeat_count: 4,
            },
        );
        state.insert(
            "Node-A".to_string(),
            NodeStatus {
                node_id: "Node-A".to_string(),
                last_temp_c: 80.0,
                last_usage: 0.12,
                last_vram_used_bytes: 1_000_000_000,
                last_uptime_seconds: 99,
                client_version: "0.1.0".to_string(),
                last_seen: now - Duration::from_secs(15),
                heartbeat_count: 1,
            },
        );

        let snapshot = build_dashboard_snapshot(&state, 5, 2, STALE_AFTER);

        assert_eq!(snapshot.total_nodes, 2);
        assert_eq!(snapshot.healthy_nodes, 1);
        assert_eq!(snapshot.stale_nodes, 1);
        assert_eq!(snapshot.total_evicted_nodes, 2);
        assert_eq!(snapshot.total_heartbeats, 5);
        assert_eq!(snapshot.rows[0].node_id, "Node-A");
        assert_eq!(snapshot.rows[0].health, NodeHealth::Stale);
        assert!((snapshot.rows[0].last_usage - 0.12).abs() < f32::EPSILON);
        assert_eq!(snapshot.rows[0].last_vram_used_bytes, 1_000_000_000);
        assert_eq!(snapshot.rows[0].last_uptime_seconds, 99);
        assert_eq!(snapshot.rows[0].client_version, "0.1.0");
        assert_eq!(snapshot.rows[1].node_id, "Node-B");
        assert_eq!(snapshot.rows[1].health, NodeHealth::Healthy);
        assert!((snapshot.rows[1].last_usage - 0.52).abs() < f32::EPSILON);
        assert_eq!(snapshot.rows[1].last_vram_used_bytes, 3_000_000_000);
        assert_eq!(snapshot.rows[1].last_uptime_seconds, 40);
        assert_eq!(snapshot.rows[1].client_version, "0.1.0");
    }

    #[test]
    fn prune_stale_nodes_removes_entries_past_ttl() {
        let state = DashMap::new();
        let now = Instant::now();

        state.insert(
            "Node-Old".to_string(),
            NodeStatus {
                node_id: "Node-Old".to_string(),
                last_temp_c: 70.0,
                last_usage: 0.90,
                last_vram_used_bytes: 9_000_000_000,
                last_uptime_seconds: 500,
                client_version: "0.1.0".to_string(),
                last_seen: now - Duration::from_secs(90),
                heartbeat_count: 5,
            },
        );
        state.insert(
            "Node-Fresh".to_string(),
            NodeStatus {
                node_id: "Node-Fresh".to_string(),
                last_temp_c: 55.0,
                last_usage: 0.40,
                last_vram_used_bytes: 2_000_000_000,
                last_uptime_seconds: 100,
                client_version: "0.1.0".to_string(),
                last_seen: now - Duration::from_secs(5),
                heartbeat_count: 2,
            },
        );

        let removed = prune_stale_nodes(&state, now, Duration::from_secs(60));

        assert_eq!(removed, 1);
        assert!(state.get("Node-Old").is_none());
        assert!(state.get("Node-Fresh").is_some());
    }

    #[test]
    fn remove_node_deletes_existing_node() {
        let state = DashMap::new();
        state.insert(
            "Node-X".to_string(),
            NodeStatus {
                node_id: "Node-X".to_string(),
                last_temp_c: 55.0,
                last_usage: 0.33,
                last_vram_used_bytes: 2_400_000_000,
                last_uptime_seconds: 12,
                client_version: "0.1.0".to_string(),
                last_seen: Instant::now(),
                heartbeat_count: 3,
            },
        );

        let removed = remove_node(&state, "Node-X");

        assert!(removed);
        assert!(state.get("Node-X").is_none());
    }

    #[tokio::test]
    async fn submit_and_get_job_status_returns_queued_job() {
        let service = test_service();

        let submit_response = service
            .submit_job(Request::new(SubmitJobRequest {
                kind: "simulated".to_string(),
                payload: "{}".to_string(),
            }))
            .await
            .expect("submit job should succeed")
            .into_inner();

        let status_response = service
            .get_job_status(Request::new(GetJobStatusRequest {
                job_id: submit_response.job_id.clone(),
            }))
            .await
            .expect("get job status should succeed")
            .into_inner();

        assert_eq!(status_response.job_id, submit_response.job_id);
        assert_eq!(status_response.state, JobRunState::Queued as i32);
        assert!(status_response.assigned_worker_id.is_empty());
        assert!(status_response.output.is_empty());
        assert!(status_response.error.is_empty());
    }

    #[test]
    fn job_lease_expiration_tracks_timeout_boundary() {
        let start = Instant::now();
        let lease = JobLease {
            worker_id: "Worker-1".to_string(),
            leased_at: start,
            lease_timeout: Duration::from_secs(10),
        };

        assert!(!lease.is_expired(start + Duration::from_secs(10)));
        assert!(lease.is_expired(start + Duration::from_secs(11)));
    }
}
