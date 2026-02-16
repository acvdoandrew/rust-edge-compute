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
    CancelJobRequest, CancelJobResponse, DisconnectRequest, DisconnectResponse,
    ExtendJobLeaseRequest, ExtendJobLeaseResponse, GetJobStatusRequest, GetJobStatusResponse,
    HeartbeatRequest, HeartbeatResponse, JobPriority as JobPriorityProto, JobRunState,
    LeaseJobRequest, LeaseJobResponse, ReportJobResultRequest, ReportJobResultResponse,
    SubmitJobRequest, SubmitJobResponse,
};

const DEFAULT_BIND_ADDR: &str = "[::1]:50051";
const STALE_AFTER: Duration = Duration::from_secs(10);
const EVICT_AFTER: Duration = Duration::from_secs(60);
const JOB_LEASE_TIMEOUT: Duration = Duration::from_secs(15);
const JOB_MAX_ATTEMPTS: u32 = 3;
const JOB_RETRY_BASE_DELAY: Duration = Duration::from_secs(2);
const JOB_RETRY_CAP_DELAY: Duration = Duration::from_secs(30);
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
    recent_jobs: Vec<JobRow>,
    total_nodes: usize,
    healthy_nodes: usize,
    stale_nodes: usize,
    queued_jobs: usize,
    queued_high_jobs: usize,
    queued_normal_jobs: usize,
    queued_low_jobs: usize,
    leased_jobs: usize,
    running_jobs: usize,
    succeeded_jobs: usize,
    failed_jobs: usize,
    total_evicted_nodes: u64,
    total_heartbeats: u64,
}

#[derive(Debug, Clone)]
struct JobRow {
    job_id: String,
    state: JobState,
    priority: JobPriority,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobState {
    Queued,
    Leased,
    Running,
    CancelRequested,
    Cancelled,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobPriority {
    Low,
    Normal,
    High,
}

impl JobPriority {
    fn as_str(self) -> &'static str {
        match self {
            Self::High => "High",
            Self::Normal => "Normal",
            Self::Low => "Low",
        }
    }

    fn rank_for_queue(self) -> u8 {
        match self {
            Self::High => 0,
            Self::Normal => 1,
            Self::Low => 2,
        }
    }

    fn from_proto(value: i32) -> Self {
        match JobPriorityProto::try_from(value).unwrap_or(JobPriorityProto::Unspecified) {
            JobPriorityProto::High => Self::High,
            JobPriorityProto::Low => Self::Low,
            JobPriorityProto::Normal | JobPriorityProto::Unspecified => Self::Normal,
        }
    }
}

impl JobState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Leased => "Leased",
            Self::Running => "Running",
            Self::CancelRequested => "Cancel Requested",
            Self::Cancelled => "Cancelled",
            Self::Succeeded => "Succeeded",
            Self::Failed => "Failed",
        }
    }

    fn as_proto(self) -> JobRunState {
        match self {
            Self::Queued => JobRunState::Queued,
            Self::Leased => JobRunState::Leased,
            Self::Running => JobRunState::Running,
            Self::CancelRequested => JobRunState::CancelRequested,
            Self::Cancelled => JobRunState::Cancelled,
            Self::Succeeded => JobRunState::Succeeded,
            Self::Failed => JobRunState::Failed,
        }
    }
}

#[derive(Debug, Clone)]
struct JobRetryState {
    attempt: u32,
    max_attempts: u32,
    next_eligible_at: Instant,
}

impl JobRetryState {
    fn new(now: Instant) -> Self {
        Self {
            attempt: 0,
            max_attempts: JOB_MAX_ATTEMPTS,
            next_eligible_at: now,
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
    required_capabilities: Vec<String>,
    priority: JobPriority,
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
    job_retries: Arc<DashMap<String, JobRetryState>>,
    job_sequence: Arc<AtomicU64>,
}

fn retry_delay_for_attempt(attempt: u32) -> Duration {
    let shift = attempt.saturating_sub(1).min(31);
    let multiplier = 1u64 << shift;
    let base_secs = JOB_RETRY_BASE_DELAY.as_secs().max(1);
    let delay_secs = base_secs.saturating_mul(multiplier);
    Duration::from_secs(delay_secs.min(JOB_RETRY_CAP_DELAY.as_secs()))
}

fn normalize_capabilities(values: &[String]) -> Vec<String> {
    let mut normalized: Vec<String> = values
        .iter()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect();

    normalized.sort();
    normalized.dedup();
    normalized
}

fn worker_satisfies_capabilities(worker: &[String], required: &[String]) -> bool {
    if required.is_empty() {
        return true;
    }

    required
        .iter()
        .all(|requirement| worker.binary_search(requirement).is_ok())
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

fn requeue_expired_jobs(jobs: &DashMap<String, JobRecord>, now: Instant) -> usize {
    let expired_job_ids: Vec<String> = jobs
        .iter()
        .filter_map(|entry| {
            let job = entry.value();
            let is_leased_state = matches!(
                job.state,
                JobState::Leased | JobState::Running | JobState::CancelRequested
            );
            let is_expired = job
                .lease
                .as_ref()
                .map(|lease| lease.is_expired(now))
                .unwrap_or(false);

            if is_leased_state && is_expired {
                Some(job.job_id.clone())
            } else {
                None
            }
        })
        .collect();

    let mut requeued = 0usize;
    for job_id in expired_job_ids {
        if let Some(mut job) = jobs.get_mut(&job_id) {
            let expired = job
                .lease
                .as_ref()
                .map(|lease| lease.is_expired(now))
                .unwrap_or(false);

            if expired {
                match job.state {
                    JobState::Leased | JobState::Running => {
                        job.state = JobState::Queued;
                        job.lease = None;
                        job.updated_at = now;
                        requeued += 1;
                    }
                    JobState::CancelRequested => {
                        job.state = JobState::Cancelled;
                        job.lease = None;
                        job.updated_at = now;
                    }
                    _ => {}
                }
            }
        }
    }

    requeued
}

fn build_dashboard_snapshot(
    state: &DashMap<String, NodeStatus>,
    jobs: &DashMap<String, JobRecord>,
    total_heartbeats: u64,
    total_evicted_nodes: u64,
    stale_after: Duration,
) -> DashboardSnapshot {
    let now = Instant::now();
    let mut rows = Vec::new();
    let mut recent_jobs = Vec::new();
    let mut healthy_nodes = 0usize;
    let mut stale_nodes = 0usize;
    let mut queued_jobs = 0usize;
    let mut queued_high_jobs = 0usize;
    let mut queued_normal_jobs = 0usize;
    let mut queued_low_jobs = 0usize;
    let mut leased_jobs = 0usize;
    let mut running_jobs = 0usize;
    let mut succeeded_jobs = 0usize;
    let mut failed_jobs = 0usize;

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

    for entry in jobs.iter() {
        let job = entry.value();

        match job.state {
            JobState::Queued => {
                queued_jobs += 1;
                match job.priority {
                    JobPriority::High => queued_high_jobs += 1,
                    JobPriority::Normal => queued_normal_jobs += 1,
                    JobPriority::Low => queued_low_jobs += 1,
                }
            }
            JobState::Leased => leased_jobs += 1,
            JobState::Running => running_jobs += 1,
            JobState::CancelRequested => running_jobs += 1,
            JobState::Cancelled => failed_jobs += 1,
            JobState::Succeeded => succeeded_jobs += 1,
            JobState::Failed => failed_jobs += 1,
        }

        recent_jobs.push(JobRow {
            job_id: job.job_id.clone(),
            state: job.state,
            priority: job.priority,
        });
    }

    rows.sort_by(|a, b| a.node_id.cmp(&b.node_id));
    recent_jobs.sort_by(|a, b| b.job_id.cmp(&a.job_id));

    DashboardSnapshot {
        total_nodes: rows.len(),
        healthy_nodes,
        stale_nodes,
        queued_jobs,
        queued_high_jobs,
        queued_normal_jobs,
        queued_low_jobs,
        leased_jobs,
        running_jobs,
        succeeded_jobs,
        failed_jobs,
        total_evicted_nodes,
        total_heartbeats,
        rows,
        recent_jobs,
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
            Constraint::Min(7),
            Constraint::Length(6),
            Constraint::Length(3),
        ])
        .split(frame.area());

    let summary = Paragraph::new(format!(
        "Active: {}  |  Stale: {}  |  Evicted: {}  |  Total Nodes: {}  |  Total Heartbeats: {}  |  Jobs Q/L/R/S/F: {}/{}/{}/{}/{}  |  Queued H/N/L: {}/{}/{}",
        snapshot.healthy_nodes,
        snapshot.stale_nodes,
        snapshot.total_evicted_nodes,
        snapshot.total_nodes,
        snapshot.total_heartbeats,
        snapshot.queued_jobs,
        snapshot.leased_jobs,
        snapshot.running_jobs,
        snapshot.succeeded_jobs,
        snapshot.failed_jobs,
        snapshot.queued_high_jobs,
        snapshot.queued_normal_jobs,
        snapshot.queued_low_jobs,
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

    let jobs_text = if snapshot.recent_jobs.is_empty() {
        "No jobs submitted yet".to_string()
    } else {
        snapshot
            .recent_jobs
            .iter()
            .take(3)
            .map(|job| {
                format!(
                    "{}  [{} | {}]",
                    job.job_id,
                    job.state.as_str(),
                    job.priority.as_str()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let jobs_panel =
        Paragraph::new(jobs_text).block(Block::default().title(" Jobs ").borders(Borders::ALL));
    frame.render_widget(jobs_panel, chunks[2]);

    let footer = Paragraph::new("Press q or Ctrl+C to stop the orchestrator")
        .block(Block::default().title(" Controls ").borders(Borders::ALL));
    frame.render_widget(footer, chunks[3]);
}

async fn run_dashboard(
    state: Arc<DashMap<String, NodeStatus>>,
    total_heartbeats: Arc<AtomicU64>,
    jobs: Arc<DashMap<String, JobRecord>>,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
) -> Result<(), Box<dyn Error>> {
    let mut terminal = ratatui::init();
    let dashboard_result = run_dashboard_loop(
        &mut terminal,
        state,
        total_heartbeats,
        jobs,
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
    jobs: Arc<DashMap<String, JobRecord>>,
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
        let _ = requeue_expired_jobs(&jobs, Instant::now());

        let snapshot = build_dashboard_snapshot(
            &state,
            &jobs,
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
        let required_capabilities = normalize_capabilities(&req.required_capabilities);
        let priority = JobPriority::from_proto(req.priority);
        let job_seq = self.job_sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let job_id = format!("job-{job_seq:06}");

        self.jobs.insert(
            job_id.clone(),
            JobRecord {
                job_id: job_id.clone(),
                kind: req.kind,
                payload: req.payload,
                required_capabilities,
                priority,
                state: JobState::Queued,
                lease: None,
                output: None,
                error: None,
                created_at: now,
                updated_at: now,
            },
        );
        self.job_retries
            .insert(job_id.clone(), JobRetryState::new(now));

        Ok(Response::new(SubmitJobResponse { job_id }))
    }

    async fn lease_job(
        &self,
        request: Request<LeaseJobRequest>,
    ) -> Result<Response<LeaseJobResponse>, Status> {
        let req = request.into_inner();

        if req.worker_id.trim().is_empty() {
            return Err(Status::invalid_argument("worker_id cannot be empty"));
        }

        let now = Instant::now();
        let worker_capabilities = normalize_capabilities(&req.worker_capabilities);
        let _ = requeue_expired_jobs(&self.jobs, now);

        let Some(job_id) = self
            .jobs
            .iter()
            .filter_map(|entry| {
                let job = entry.value();
                if job.state != JobState::Queued {
                    return None;
                }

                if !worker_satisfies_capabilities(&worker_capabilities, &job.required_capabilities)
                {
                    return None;
                }

                let is_eligible = self
                    .job_retries
                    .get(&job.job_id)
                    .map(|retry| now >= retry.next_eligible_at)
                    .unwrap_or(true);

                if is_eligible {
                    Some((job.priority.rank_for_queue(), job.job_id.clone()))
                } else {
                    None
                }
            })
            .min()
            .map(|(_, job_id)| job_id)
        else {
            return Ok(Response::new(LeaseJobResponse {
                has_job: false,
                job_id: String::new(),
                kind: String::new(),
                payload: String::new(),
                lease_timeout_seconds: JOB_LEASE_TIMEOUT.as_secs(),
            }));
        };

        let mut job = self
            .jobs
            .get_mut(&job_id)
            .ok_or_else(|| Status::internal("selected job disappeared before leasing"))?;

        if job.state != JobState::Queued {
            return Ok(Response::new(LeaseJobResponse {
                has_job: false,
                job_id: String::new(),
                kind: String::new(),
                payload: String::new(),
                lease_timeout_seconds: JOB_LEASE_TIMEOUT.as_secs(),
            }));
        }

        job.state = JobState::Leased;
        job.updated_at = now;
        job.lease = Some(JobLease {
            worker_id: req.worker_id,
            leased_at: now,
            lease_timeout: JOB_LEASE_TIMEOUT,
        });

        let mut retry = self
            .job_retries
            .entry(job.job_id.clone())
            .or_insert_with(|| JobRetryState::new(now));
        retry.attempt = retry.attempt.saturating_add(1);
        retry.next_eligible_at = now;

        Ok(Response::new(LeaseJobResponse {
            has_job: true,
            job_id: job.job_id.clone(),
            kind: job.kind.clone(),
            payload: job.payload.clone(),
            lease_timeout_seconds: JOB_LEASE_TIMEOUT.as_secs(),
        }))
    }

    async fn extend_job_lease(
        &self,
        request: Request<ExtendJobLeaseRequest>,
    ) -> Result<Response<ExtendJobLeaseResponse>, Status> {
        let req = request.into_inner();

        if req.worker_id.trim().is_empty() {
            return Err(Status::invalid_argument("worker_id cannot be empty"));
        }
        if req.job_id.trim().is_empty() {
            return Err(Status::invalid_argument("job_id cannot be empty"));
        }

        let now = Instant::now();
        let mut job = self
            .jobs
            .get_mut(&req.job_id)
            .ok_or_else(|| Status::not_found(format!("job {} not found", req.job_id)))?;

        if !matches!(
            job.state,
            JobState::Leased | JobState::Running | JobState::CancelRequested
        ) {
            return Err(Status::failed_precondition(
                "job is not currently leased by any worker",
            ));
        }

        {
            let lease = job
                .lease
                .as_mut()
                .ok_or_else(|| Status::failed_precondition("job lease metadata is missing"))?;

            if lease.worker_id != req.worker_id {
                return Err(Status::permission_denied(
                    "worker does not own the current lease for this job",
                ));
            }

            if lease.is_expired(now) {
                return Err(Status::failed_precondition("job lease is already expired"));
            }

            lease.leased_at = now;
            lease.lease_timeout = JOB_LEASE_TIMEOUT;
        }

        if job.state == JobState::Leased {
            job.state = JobState::Running;
        }

        job.updated_at = now;

        Ok(Response::new(ExtendJobLeaseResponse {
            acknowledged: true,
            lease_timeout_seconds: JOB_LEASE_TIMEOUT.as_secs(),
            cancel_requested: job.state == JobState::CancelRequested,
        }))
    }

    async fn cancel_job(
        &self,
        request: Request<CancelJobRequest>,
    ) -> Result<Response<CancelJobResponse>, Status> {
        let req = request.into_inner();

        if req.job_id.trim().is_empty() {
            return Err(Status::invalid_argument("job_id cannot be empty"));
        }

        let now = Instant::now();
        let mut job = self
            .jobs
            .get_mut(&req.job_id)
            .ok_or_else(|| Status::not_found(format!("job {} not found", req.job_id)))?;

        let reason = if req.reason.trim().is_empty() {
            None
        } else {
            Some(req.reason.trim().to_string())
        };

        match job.state {
            JobState::Queued => {
                job.state = JobState::Cancelled;
                job.lease = None;
                job.updated_at = now;
                job.error = reason
                    .map(|r| format!("cancelled before execution: {r}"))
                    .or_else(|| Some("cancelled before execution".to_string()));
            }
            JobState::Leased | JobState::Running | JobState::CancelRequested => {
                job.state = JobState::CancelRequested;
                job.updated_at = now;
                if let Some(r) = reason {
                    job.error = Some(format!("cancel requested: {r}"));
                } else if job.error.is_none() {
                    job.error = Some("cancel requested".to_string());
                }
            }
            JobState::Succeeded | JobState::Failed | JobState::Cancelled => {}
        }

        Ok(Response::new(CancelJobResponse {
            acknowledged: true,
            state: job.state.as_proto() as i32,
        }))
    }

    async fn report_job_result(
        &self,
        request: Request<ReportJobResultRequest>,
    ) -> Result<Response<ReportJobResultResponse>, Status> {
        let req = request.into_inner();
        let now = Instant::now();

        if req.worker_id.trim().is_empty() {
            return Err(Status::invalid_argument("worker_id cannot be empty"));
        }
        if req.job_id.trim().is_empty() {
            return Err(Status::invalid_argument("job_id cannot be empty"));
        }

        let mut job = self
            .jobs
            .get_mut(&req.job_id)
            .ok_or_else(|| Status::not_found(format!("job {} not found", req.job_id)))?;

        let lease = job
            .lease
            .as_ref()
            .ok_or_else(|| Status::failed_precondition("job is not currently leased"))?;

        if lease.worker_id != req.worker_id {
            return Err(Status::permission_denied(
                "worker does not own the current lease for this job",
            ));
        }

        if matches!(
            job.state,
            JobState::Succeeded | JobState::Failed | JobState::Cancelled
        ) {
            return Err(Status::failed_precondition(
                "job is already in a terminal state",
            ));
        }

        if job.state == JobState::Leased {
            job.state = JobState::Running;
        }

        let output = if req.output.trim().is_empty() {
            None
        } else {
            Some(req.output.clone())
        };

        let mut error = if req.error.trim().is_empty() {
            None
        } else {
            Some(req.error.clone())
        };

        if job.state == JobState::CancelRequested {
            job.state = JobState::Cancelled;
            job.output = None;
            job.error = error
                .take()
                .or_else(|| Some("job cancelled by operator".to_string()));
            job.updated_at = now;
            job.lease = None;
            return Ok(Response::new(ReportJobResultResponse {
                acknowledged: true,
            }));
        }

        if req.success {
            job.state = JobState::Succeeded;
            job.output = output;
            job.error = error;
            job.updated_at = now;
            job.lease = None;
            return Ok(Response::new(ReportJobResultResponse {
                acknowledged: true,
            }));
        }

        let mut retry_state = self
            .job_retries
            .entry(job.job_id.clone())
            .or_insert_with(|| JobRetryState::new(now));

        let should_retry = retry_state.attempt < retry_state.max_attempts;
        if should_retry {
            let delay = retry_delay_for_attempt(retry_state.attempt);
            retry_state.next_eligible_at = now + delay;
            job.state = JobState::Queued;

            let message = error
                .take()
                .unwrap_or_else(|| "worker reported failure".to_string());
            job.error = Some(format!(
                "{message}; retrying ({}/{}) in {}s",
                retry_state.attempt,
                retry_state.max_attempts,
                delay.as_secs()
            ));
        } else {
            retry_state.next_eligible_at = now;
            job.state = JobState::Failed;
            job.error = Some(
                error
                    .take()
                    .unwrap_or_else(|| "worker reported failure".to_string()),
            );
        }

        job.output = output;
        job.updated_at = now;
        job.lease = None;

        Ok(Response::new(ReportJobResultResponse {
            acknowledged: true,
        }))
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
    let job_retries = Arc::new(DashMap::new());
    let job_sequence = Arc::new(AtomicU64::new(0));

    let service = MyNodeService {
        state: state.clone(),
        total_heartbeats: total_heartbeats.clone(),
        jobs: jobs.clone(),
        job_retries: job_retries.clone(),
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
        jobs,
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
    use tokio::sync::oneshot;
    use tokio::time::{sleep, timeout};
    use tonic::transport::Server;

    fn test_service() -> MyNodeService {
        MyNodeService {
            state: Arc::new(DashMap::new()),
            total_heartbeats: Arc::new(AtomicU64::new(0)),
            jobs: Arc::new(DashMap::new()),
            job_retries: Arc::new(DashMap::new()),
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

    async fn spawn_job_service_server(
        service: MyNodeService,
    ) -> (
        node::job_service_client::JobServiceClient<tonic::transport::Channel>,
        oneshot::Sender<()>,
        tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
    ) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let addr = listener.local_addr().expect("read bound addr");
        drop(listener);

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let server_task = tokio::spawn(async move {
            Server::builder()
                .add_service(JobServiceServer::new(service))
                .serve_with_shutdown(addr, async move {
                    let _ = shutdown_rx.await;
                })
                .await
        });

        let mut client = None;
        let mut last_err = None;
        for _ in 0..40 {
            match node::job_service_client::JobServiceClient::connect(format!("http://{addr}"))
                .await
            {
                Ok(connected) => {
                    client = Some(connected);
                    break;
                }
                Err(err) => {
                    last_err = Some(err);
                    sleep(Duration::from_millis(25)).await;
                }
            }
        }

        let client = client.unwrap_or_else(|| {
            panic!(
                "connect job service client: {}",
                last_err.expect("expected connection error when client is unavailable")
            )
        });

        (client, shutdown_tx, server_task)
    }

    async fn stop_job_service_server(
        shutdown_tx: oneshot::Sender<()>,
        server_task: tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
    ) {
        let _ = shutdown_tx.send(());
        timeout(Duration::from_secs(8), server_task)
            .await
            .expect("server task timed out")
            .expect("server task join failed")
            .expect("server returned error");
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
        let jobs = DashMap::new();
        let snapshot = build_dashboard_snapshot(&state, &jobs, 0, 0, STALE_AFTER);

        assert_eq!(snapshot.total_nodes, 0);
        assert_eq!(snapshot.healthy_nodes, 0);
        assert_eq!(snapshot.stale_nodes, 0);
        assert_eq!(snapshot.queued_jobs, 0);
        assert_eq!(snapshot.queued_high_jobs, 0);
        assert_eq!(snapshot.queued_normal_jobs, 0);
        assert_eq!(snapshot.queued_low_jobs, 0);
        assert_eq!(snapshot.leased_jobs, 0);
        assert_eq!(snapshot.running_jobs, 0);
        assert_eq!(snapshot.succeeded_jobs, 0);
        assert_eq!(snapshot.failed_jobs, 0);
        assert_eq!(snapshot.total_evicted_nodes, 0);
        assert_eq!(snapshot.total_heartbeats, 0);
        assert!(snapshot.rows.is_empty());
        assert!(snapshot.recent_jobs.is_empty());
    }

    #[test]
    fn build_dashboard_snapshot_sorts_rows_and_counts_health() {
        let state = DashMap::new();
        let jobs = DashMap::new();
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

        let snapshot = build_dashboard_snapshot(&state, &jobs, 5, 2, STALE_AFTER);

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
    fn build_dashboard_snapshot_counts_jobs_by_state() {
        let state = DashMap::new();
        let jobs = DashMap::new();
        let now = Instant::now();

        jobs.insert(
            "job-000001".to_string(),
            JobRecord {
                job_id: "job-000001".to_string(),
                kind: "simulated".to_string(),
                payload: "{}".to_string(),
                required_capabilities: Vec::new(),
                priority: JobPriority::Normal,
                state: JobState::Queued,
                lease: None,
                output: None,
                error: None,
                created_at: now,
                updated_at: now,
            },
        );
        jobs.insert(
            "job-000002".to_string(),
            JobRecord {
                job_id: "job-000002".to_string(),
                kind: "simulated".to_string(),
                payload: "{}".to_string(),
                required_capabilities: Vec::new(),
                priority: JobPriority::Normal,
                state: JobState::Leased,
                lease: Some(JobLease {
                    worker_id: "Worker-A".to_string(),
                    leased_at: now,
                    lease_timeout: Duration::from_secs(15),
                }),
                output: None,
                error: None,
                created_at: now,
                updated_at: now,
            },
        );
        jobs.insert(
            "job-000003".to_string(),
            JobRecord {
                job_id: "job-000003".to_string(),
                kind: "simulated".to_string(),
                payload: "{}".to_string(),
                required_capabilities: Vec::new(),
                priority: JobPriority::Normal,
                state: JobState::Running,
                lease: Some(JobLease {
                    worker_id: "Worker-B".to_string(),
                    leased_at: now,
                    lease_timeout: Duration::from_secs(15),
                }),
                output: None,
                error: None,
                created_at: now,
                updated_at: now,
            },
        );
        jobs.insert(
            "job-000004".to_string(),
            JobRecord {
                job_id: "job-000004".to_string(),
                kind: "simulated".to_string(),
                payload: "{}".to_string(),
                required_capabilities: Vec::new(),
                priority: JobPriority::Normal,
                state: JobState::Succeeded,
                lease: None,
                output: Some("ok".to_string()),
                error: None,
                created_at: now,
                updated_at: now,
            },
        );
        jobs.insert(
            "job-000005".to_string(),
            JobRecord {
                job_id: "job-000005".to_string(),
                kind: "simulated".to_string(),
                payload: "{}".to_string(),
                required_capabilities: Vec::new(),
                priority: JobPriority::Normal,
                state: JobState::Failed,
                lease: None,
                output: None,
                error: Some("boom".to_string()),
                created_at: now,
                updated_at: now,
            },
        );

        let snapshot = build_dashboard_snapshot(&state, &jobs, 0, 0, STALE_AFTER);

        assert_eq!(snapshot.queued_jobs, 1);
        assert_eq!(snapshot.queued_high_jobs, 0);
        assert_eq!(snapshot.queued_normal_jobs, 1);
        assert_eq!(snapshot.queued_low_jobs, 0);
        assert_eq!(snapshot.leased_jobs, 1);
        assert_eq!(snapshot.running_jobs, 1);
        assert_eq!(snapshot.succeeded_jobs, 1);
        assert_eq!(snapshot.failed_jobs, 1);
        assert_eq!(snapshot.recent_jobs[0].job_id, "job-000005");
    }

    #[test]
    fn build_dashboard_snapshot_counts_queued_priorities() {
        let state = DashMap::new();
        let jobs = DashMap::new();
        let now = Instant::now();

        jobs.insert(
            "job-000010".to_string(),
            JobRecord {
                job_id: "job-000010".to_string(),
                kind: "simulated".to_string(),
                payload: "{}".to_string(),
                required_capabilities: Vec::new(),
                priority: JobPriority::High,
                state: JobState::Queued,
                lease: None,
                output: None,
                error: None,
                created_at: now,
                updated_at: now,
            },
        );
        jobs.insert(
            "job-000011".to_string(),
            JobRecord {
                job_id: "job-000011".to_string(),
                kind: "simulated".to_string(),
                payload: "{}".to_string(),
                required_capabilities: Vec::new(),
                priority: JobPriority::Normal,
                state: JobState::Queued,
                lease: None,
                output: None,
                error: None,
                created_at: now,
                updated_at: now,
            },
        );
        jobs.insert(
            "job-000012".to_string(),
            JobRecord {
                job_id: "job-000012".to_string(),
                kind: "simulated".to_string(),
                payload: "{}".to_string(),
                required_capabilities: Vec::new(),
                priority: JobPriority::Low,
                state: JobState::Queued,
                lease: None,
                output: None,
                error: None,
                created_at: now,
                updated_at: now,
            },
        );

        let snapshot = build_dashboard_snapshot(&state, &jobs, 0, 0, STALE_AFTER);

        assert_eq!(snapshot.queued_jobs, 3);
        assert_eq!(snapshot.queued_high_jobs, 1);
        assert_eq!(snapshot.queued_normal_jobs, 1);
        assert_eq!(snapshot.queued_low_jobs, 1);
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
                required_capabilities: Vec::new(),
                priority: JobPriorityProto::Unspecified as i32,
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

    #[tokio::test]
    async fn lease_job_assigns_oldest_queued_job_to_worker() {
        let service = test_service();

        let first_job = service
            .submit_job(Request::new(SubmitJobRequest {
                kind: "simulated".to_string(),
                payload: "payload-1".to_string(),
                required_capabilities: Vec::new(),
                priority: JobPriorityProto::Unspecified as i32,
            }))
            .await
            .expect("first job submit should succeed")
            .into_inner();

        service
            .submit_job(Request::new(SubmitJobRequest {
                kind: "simulated".to_string(),
                payload: "payload-2".to_string(),
                required_capabilities: Vec::new(),
                priority: JobPriorityProto::Unspecified as i32,
            }))
            .await
            .expect("second job submit should succeed");

        let lease = service
            .lease_job(Request::new(LeaseJobRequest {
                worker_id: "Worker-A".to_string(),
                worker_capabilities: Vec::new(),
            }))
            .await
            .expect("lease should succeed")
            .into_inner();

        assert!(lease.has_job);
        assert_eq!(lease.job_id, first_job.job_id);
        assert_eq!(lease.kind, "simulated");
        assert_eq!(lease.payload, "payload-1");

        let leased_record = service
            .jobs
            .get(&lease.job_id)
            .expect("leased job should be present in job map");
        assert_eq!(leased_record.state, JobState::Leased);
        assert_eq!(
            leased_record
                .lease
                .as_ref()
                .expect("lease metadata should be set")
                .worker_id,
            "Worker-A"
        );
    }

    #[tokio::test]
    async fn lease_job_returns_empty_response_when_queue_is_empty() {
        let service = test_service();

        let lease = service
            .lease_job(Request::new(LeaseJobRequest {
                worker_id: "Worker-A".to_string(),
                worker_capabilities: Vec::new(),
            }))
            .await
            .expect("lease call should succeed")
            .into_inner();

        assert!(!lease.has_job);
        assert!(lease.job_id.is_empty());
        assert!(lease.kind.is_empty());
    }

    #[tokio::test]
    async fn lease_job_respects_required_capabilities() {
        let service = test_service();

        let constrained_job = service
            .submit_job(Request::new(SubmitJobRequest {
                kind: "simulated".to_string(),
                payload: "{}".to_string(),
                required_capabilities: vec!["GPU:NVML".to_string(), "region:us".to_string()],
                priority: JobPriorityProto::Unspecified as i32,
            }))
            .await
            .expect("submit should succeed")
            .into_inner();

        let missing_caps = service
            .lease_job(Request::new(LeaseJobRequest {
                worker_id: "Worker-A".to_string(),
                worker_capabilities: vec!["gpu:amd".to_string(), "region:us".to_string()],
            }))
            .await
            .expect("lease call should succeed")
            .into_inner();

        assert!(!missing_caps.has_job);

        let matched_caps = service
            .lease_job(Request::new(LeaseJobRequest {
                worker_id: "Worker-B".to_string(),
                worker_capabilities: vec![
                    "region:us".to_string(),
                    "gpu:nvml".to_string(),
                    "extra:label".to_string(),
                ],
            }))
            .await
            .expect("lease call should succeed")
            .into_inner();

        assert!(matched_caps.has_job);
        assert_eq!(matched_caps.job_id, constrained_job.job_id);
    }

    #[tokio::test]
    async fn lease_job_prioritizes_high_then_normal_then_low() {
        let service = test_service();

        let low = service
            .submit_job(Request::new(SubmitJobRequest {
                kind: "simulated".to_string(),
                payload: "low".to_string(),
                required_capabilities: Vec::new(),
                priority: JobPriorityProto::Low as i32,
            }))
            .await
            .expect("submit low should succeed")
            .into_inner();

        let high = service
            .submit_job(Request::new(SubmitJobRequest {
                kind: "simulated".to_string(),
                payload: "high".to_string(),
                required_capabilities: Vec::new(),
                priority: JobPriorityProto::High as i32,
            }))
            .await
            .expect("submit high should succeed")
            .into_inner();

        let normal = service
            .submit_job(Request::new(SubmitJobRequest {
                kind: "simulated".to_string(),
                payload: "normal".to_string(),
                required_capabilities: Vec::new(),
                priority: JobPriorityProto::Normal as i32,
            }))
            .await
            .expect("submit normal should succeed")
            .into_inner();

        let first = service
            .lease_job(Request::new(LeaseJobRequest {
                worker_id: "Worker-A".to_string(),
                worker_capabilities: Vec::new(),
            }))
            .await
            .expect("first lease should succeed")
            .into_inner();
        assert!(first.has_job);
        assert_eq!(first.job_id, high.job_id);

        let second = service
            .lease_job(Request::new(LeaseJobRequest {
                worker_id: "Worker-B".to_string(),
                worker_capabilities: Vec::new(),
            }))
            .await
            .expect("second lease should succeed")
            .into_inner();
        assert!(second.has_job);
        assert_eq!(second.job_id, normal.job_id);

        let third = service
            .lease_job(Request::new(LeaseJobRequest {
                worker_id: "Worker-C".to_string(),
                worker_capabilities: Vec::new(),
            }))
            .await
            .expect("third lease should succeed")
            .into_inner();
        assert!(third.has_job);
        assert_eq!(third.job_id, low.job_id);
    }

    #[tokio::test]
    async fn report_job_result_moves_leased_job_to_terminal_state() {
        let service = test_service();

        let job = service
            .submit_job(Request::new(SubmitJobRequest {
                kind: "simulated".to_string(),
                payload: "{}".to_string(),
                required_capabilities: Vec::new(),
                priority: JobPriorityProto::Unspecified as i32,
            }))
            .await
            .expect("submit should succeed")
            .into_inner();

        service
            .lease_job(Request::new(LeaseJobRequest {
                worker_id: "Worker-A".to_string(),
                worker_capabilities: Vec::new(),
            }))
            .await
            .expect("lease should succeed");

        let report = service
            .report_job_result(Request::new(ReportJobResultRequest {
                worker_id: "Worker-A".to_string(),
                job_id: job.job_id.clone(),
                success: true,
                output: "done".to_string(),
                error: String::new(),
            }))
            .await
            .expect("report result should succeed")
            .into_inner();

        assert!(report.acknowledged);

        let status = service
            .get_job_status(Request::new(GetJobStatusRequest { job_id: job.job_id }))
            .await
            .expect("status request should succeed")
            .into_inner();

        assert_eq!(status.state, JobRunState::Succeeded as i32);
        assert_eq!(status.output, "done");
        assert!(status.assigned_worker_id.is_empty());
        assert!(status.error.is_empty());
    }

    #[tokio::test]
    async fn report_job_result_rejects_worker_without_lease() {
        let service = test_service();

        let job = service
            .submit_job(Request::new(SubmitJobRequest {
                kind: "simulated".to_string(),
                payload: "{}".to_string(),
                required_capabilities: Vec::new(),
                priority: JobPriorityProto::Unspecified as i32,
            }))
            .await
            .expect("submit should succeed")
            .into_inner();

        service
            .lease_job(Request::new(LeaseJobRequest {
                worker_id: "Worker-A".to_string(),
                worker_capabilities: Vec::new(),
            }))
            .await
            .expect("lease should succeed");

        let err = service
            .report_job_result(Request::new(ReportJobResultRequest {
                worker_id: "Worker-B".to_string(),
                job_id: job.job_id,
                success: false,
                output: String::new(),
                error: "failed".to_string(),
            }))
            .await
            .expect_err("report should fail for non-owner worker");

        assert_eq!(err.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn extend_job_lease_transitions_job_to_running() {
        let service = test_service();

        let job = service
            .submit_job(Request::new(SubmitJobRequest {
                kind: "simulated".to_string(),
                payload: "{}".to_string(),
                required_capabilities: Vec::new(),
                priority: JobPriorityProto::Unspecified as i32,
            }))
            .await
            .expect("submit should succeed")
            .into_inner();

        service
            .lease_job(Request::new(LeaseJobRequest {
                worker_id: "Worker-A".to_string(),
                worker_capabilities: Vec::new(),
            }))
            .await
            .expect("lease should succeed");

        let extension = service
            .extend_job_lease(Request::new(ExtendJobLeaseRequest {
                worker_id: "Worker-A".to_string(),
                job_id: job.job_id.clone(),
            }))
            .await
            .expect("lease extension should succeed")
            .into_inner();

        assert!(extension.acknowledged);
        assert!(!extension.cancel_requested);
        assert_eq!(extension.lease_timeout_seconds, JOB_LEASE_TIMEOUT.as_secs());

        let status = service
            .get_job_status(Request::new(GetJobStatusRequest { job_id: job.job_id }))
            .await
            .expect("status should succeed")
            .into_inner();

        assert_eq!(status.state, JobRunState::Running as i32);
        assert_eq!(status.assigned_worker_id, "Worker-A");
    }

    #[tokio::test]
    async fn cancel_job_marks_queued_job_as_cancelled() {
        let service = test_service();

        let job = service
            .submit_job(Request::new(SubmitJobRequest {
                kind: "simulated".to_string(),
                payload: "{}".to_string(),
                required_capabilities: Vec::new(),
                priority: JobPriorityProto::Unspecified as i32,
            }))
            .await
            .expect("submit should succeed")
            .into_inner();

        let cancel = service
            .cancel_job(Request::new(CancelJobRequest {
                job_id: job.job_id.clone(),
                reason: "operator request".to_string(),
            }))
            .await
            .expect("cancel should succeed")
            .into_inner();

        assert!(cancel.acknowledged);
        assert_eq!(cancel.state, JobRunState::Cancelled as i32);

        let status = service
            .get_job_status(Request::new(GetJobStatusRequest { job_id: job.job_id }))
            .await
            .expect("status should succeed")
            .into_inner();

        assert_eq!(status.state, JobRunState::Cancelled as i32);
        assert!(status.assigned_worker_id.is_empty());
        assert!(status.error.contains("cancel"));
    }

    #[tokio::test]
    async fn cancel_job_for_leased_job_waits_for_worker_ack() {
        let service = test_service();

        let job = service
            .submit_job(Request::new(SubmitJobRequest {
                kind: "simulated".to_string(),
                payload: "{}".to_string(),
                required_capabilities: Vec::new(),
                priority: JobPriorityProto::Unspecified as i32,
            }))
            .await
            .expect("submit should succeed")
            .into_inner();

        service
            .lease_job(Request::new(LeaseJobRequest {
                worker_id: "Worker-A".to_string(),
                worker_capabilities: Vec::new(),
            }))
            .await
            .expect("lease should succeed");

        let cancel = service
            .cancel_job(Request::new(CancelJobRequest {
                job_id: job.job_id.clone(),
                reason: "draining node".to_string(),
            }))
            .await
            .expect("cancel should succeed")
            .into_inner();

        assert_eq!(cancel.state, JobRunState::CancelRequested as i32);

        let extension = service
            .extend_job_lease(Request::new(ExtendJobLeaseRequest {
                worker_id: "Worker-A".to_string(),
                job_id: job.job_id.clone(),
            }))
            .await
            .expect("lease extension should succeed")
            .into_inner();

        assert!(extension.cancel_requested);

        service
            .report_job_result(Request::new(ReportJobResultRequest {
                worker_id: "Worker-A".to_string(),
                job_id: job.job_id.clone(),
                success: false,
                output: String::new(),
                error: "worker cancelled task".to_string(),
            }))
            .await
            .expect("report should succeed for cooperative cancel");

        let status = service
            .get_job_status(Request::new(GetJobStatusRequest { job_id: job.job_id }))
            .await
            .expect("status should succeed")
            .into_inner();

        assert_eq!(status.state, JobRunState::Cancelled as i32);
        assert!(status.error.contains("cancel"));
    }

    #[tokio::test]
    async fn failed_job_retries_before_terminal_failure() {
        let service = test_service();

        let job = service
            .submit_job(Request::new(SubmitJobRequest {
                kind: "simulated".to_string(),
                payload: "{}".to_string(),
                required_capabilities: Vec::new(),
                priority: JobPriorityProto::Unspecified as i32,
            }))
            .await
            .expect("submit should succeed")
            .into_inner();

        for attempt in 1..=JOB_MAX_ATTEMPTS {
            let lease = service
                .lease_job(Request::new(LeaseJobRequest {
                    worker_id: format!("Worker-{attempt}"),
                    worker_capabilities: Vec::new(),
                }))
                .await
                .expect("lease should succeed")
                .into_inner();

            assert!(lease.has_job);
            assert_eq!(lease.job_id, job.job_id);

            service
                .report_job_result(Request::new(ReportJobResultRequest {
                    worker_id: format!("Worker-{attempt}"),
                    job_id: job.job_id.clone(),
                    success: false,
                    output: String::new(),
                    error: format!("attempt {attempt} failed"),
                }))
                .await
                .expect("report should succeed");

            let status = service
                .get_job_status(Request::new(GetJobStatusRequest {
                    job_id: job.job_id.clone(),
                }))
                .await
                .expect("status should succeed")
                .into_inner();

            if attempt < JOB_MAX_ATTEMPTS {
                assert_eq!(status.state, JobRunState::Queued as i32);
                let mut retry = service
                    .job_retries
                    .get_mut(&job.job_id)
                    .expect("retry metadata should exist");
                retry.next_eligible_at = Instant::now() - Duration::from_millis(1);
            } else {
                assert_eq!(status.state, JobRunState::Failed as i32);
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn grpc_happy_path_submit_lease_and_complete_job() {
        let service = test_service();
        let (mut client, shutdown_tx, server_task) =
            spawn_job_service_server(service.clone()).await;

        let submit = client
            .submit_job(Request::new(SubmitJobRequest {
                kind: "simulated".to_string(),
                payload: "{\"work\":1}".to_string(),
                required_capabilities: Vec::new(),
                priority: JobPriorityProto::Unspecified as i32,
            }))
            .await
            .expect("submit should succeed")
            .into_inner();

        let leased = client
            .lease_job(Request::new(LeaseJobRequest {
                worker_id: "Worker-IT-A".to_string(),
                worker_capabilities: Vec::new(),
            }))
            .await
            .expect("lease should succeed")
            .into_inner();

        assert!(leased.has_job);
        assert_eq!(leased.job_id, submit.job_id);

        let leased_status = client
            .get_job_status(Request::new(GetJobStatusRequest {
                job_id: submit.job_id.clone(),
            }))
            .await
            .expect("leased status should succeed")
            .into_inner();

        assert_eq!(leased_status.state, JobRunState::Leased as i32);
        assert_eq!(leased_status.assigned_worker_id, "Worker-IT-A");

        let report = client
            .report_job_result(Request::new(ReportJobResultRequest {
                worker_id: "Worker-IT-A".to_string(),
                job_id: submit.job_id.clone(),
                success: true,
                output: "integration-ok".to_string(),
                error: String::new(),
            }))
            .await
            .expect("report should succeed")
            .into_inner();

        assert!(report.acknowledged);

        let done_status = client
            .get_job_status(Request::new(GetJobStatusRequest {
                job_id: submit.job_id,
            }))
            .await
            .expect("final status should succeed")
            .into_inner();

        assert_eq!(done_status.state, JobRunState::Succeeded as i32);
        assert!(done_status.assigned_worker_id.is_empty());
        assert_eq!(done_status.output, "integration-ok");
        assert!(done_status.error.is_empty());

        stop_job_service_server(shutdown_tx, server_task).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn grpc_requeues_timed_out_lease_for_new_worker() {
        let service = test_service();
        let (mut client, shutdown_tx, server_task) =
            spawn_job_service_server(service.clone()).await;

        let submit = client
            .submit_job(Request::new(SubmitJobRequest {
                kind: "simulated".to_string(),
                payload: "{\"work\":2}".to_string(),
                required_capabilities: Vec::new(),
                priority: JobPriorityProto::Unspecified as i32,
            }))
            .await
            .expect("submit should succeed")
            .into_inner();

        let first_lease = client
            .lease_job(Request::new(LeaseJobRequest {
                worker_id: "Worker-IT-A".to_string(),
                worker_capabilities: Vec::new(),
            }))
            .await
            .expect("first lease should succeed")
            .into_inner();

        assert!(first_lease.has_job);
        assert_eq!(first_lease.job_id, submit.job_id);

        {
            let mut record = service
                .jobs
                .get_mut(&submit.job_id)
                .expect("leased job should exist");
            let lease = record
                .lease
                .as_mut()
                .expect("leased job should include lease metadata");
            lease.leased_at = Instant::now() - Duration::from_secs(30);
        }

        let second_lease = client
            .lease_job(Request::new(LeaseJobRequest {
                worker_id: "Worker-IT-B".to_string(),
                worker_capabilities: Vec::new(),
            }))
            .await
            .expect("second lease should succeed")
            .into_inner();

        assert!(second_lease.has_job);
        assert_eq!(second_lease.job_id, submit.job_id);

        let status = client
            .get_job_status(Request::new(GetJobStatusRequest {
                job_id: submit.job_id,
            }))
            .await
            .expect("status should succeed")
            .into_inner();

        assert_eq!(status.state, JobRunState::Leased as i32);
        assert_eq!(status.assigned_worker_id, "Worker-IT-B");

        stop_job_service_server(shutdown_tx, server_task).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn grpc_cancel_requested_job_becomes_cancelled_after_lease_timeout() {
        let service = test_service();
        let (mut client, shutdown_tx, server_task) =
            spawn_job_service_server(service.clone()).await;

        let submit = client
            .submit_job(Request::new(SubmitJobRequest {
                kind: "simulated".to_string(),
                payload: "{\"work\":3}".to_string(),
                required_capabilities: Vec::new(),
                priority: JobPriorityProto::Unspecified as i32,
            }))
            .await
            .expect("submit should succeed")
            .into_inner();

        client
            .lease_job(Request::new(LeaseJobRequest {
                worker_id: "Worker-IT-A".to_string(),
                worker_capabilities: Vec::new(),
            }))
            .await
            .expect("lease should succeed");

        let cancel = client
            .cancel_job(Request::new(CancelJobRequest {
                job_id: submit.job_id.clone(),
                reason: "node drained".to_string(),
            }))
            .await
            .expect("cancel should succeed")
            .into_inner();

        assert_eq!(cancel.state, JobRunState::CancelRequested as i32);

        {
            let mut record = service
                .jobs
                .get_mut(&submit.job_id)
                .expect("cancel-requested job should exist");
            let lease = record
                .lease
                .as_mut()
                .expect("cancel-requested job should include lease metadata");
            lease.leased_at = Instant::now() - Duration::from_secs(30);
        }

        let lease_after_timeout = client
            .lease_job(Request::new(LeaseJobRequest {
                worker_id: "Worker-IT-B".to_string(),
                worker_capabilities: Vec::new(),
            }))
            .await
            .expect("lease call should succeed")
            .into_inner();

        assert!(!lease_after_timeout.has_job);

        let status = client
            .get_job_status(Request::new(GetJobStatusRequest {
                job_id: submit.job_id,
            }))
            .await
            .expect("status should succeed")
            .into_inner();

        assert_eq!(status.state, JobRunState::Cancelled as i32);
        assert!(status.assigned_worker_id.is_empty());

        stop_job_service_server(shutdown_tx, server_task).await;
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

    #[test]
    fn retry_delay_scales_and_caps() {
        assert_eq!(retry_delay_for_attempt(1), Duration::from_secs(2));
        assert_eq!(retry_delay_for_attempt(2), Duration::from_secs(4));
        assert_eq!(retry_delay_for_attempt(3), Duration::from_secs(8));
        assert_eq!(retry_delay_for_attempt(10), Duration::from_secs(30));
    }

    #[test]
    fn requeue_expired_jobs_returns_lease_to_queue() {
        let jobs = DashMap::new();
        let now = Instant::now();

        jobs.insert(
            "job-000001".to_string(),
            JobRecord {
                job_id: "job-000001".to_string(),
                kind: "simulated".to_string(),
                payload: "{}".to_string(),
                required_capabilities: Vec::new(),
                priority: JobPriority::Normal,
                state: JobState::Leased,
                lease: Some(JobLease {
                    worker_id: "Worker-A".to_string(),
                    leased_at: now - Duration::from_secs(20),
                    lease_timeout: Duration::from_secs(10),
                }),
                output: None,
                error: None,
                created_at: now - Duration::from_secs(30),
                updated_at: now - Duration::from_secs(20),
            },
        );

        jobs.insert(
            "job-000002".to_string(),
            JobRecord {
                job_id: "job-000002".to_string(),
                kind: "simulated".to_string(),
                payload: "{}".to_string(),
                required_capabilities: Vec::new(),
                priority: JobPriority::Normal,
                state: JobState::Leased,
                lease: Some(JobLease {
                    worker_id: "Worker-B".to_string(),
                    leased_at: now - Duration::from_secs(3),
                    lease_timeout: Duration::from_secs(10),
                }),
                output: None,
                error: None,
                created_at: now - Duration::from_secs(10),
                updated_at: now - Duration::from_secs(3),
            },
        );

        let requeued = requeue_expired_jobs(&jobs, now);

        assert_eq!(requeued, 1);

        let expired_job = jobs
            .get("job-000001")
            .expect("expired job should still exist in map");
        assert_eq!(expired_job.state, JobState::Queued);
        assert!(expired_job.lease.is_none());

        let fresh_job = jobs
            .get("job-000002")
            .expect("fresh leased job should still exist in map");
        assert_eq!(fresh_job.state, JobState::Leased);
        assert!(fresh_job.lease.is_some());
    }

    #[test]
    fn requeue_expired_cancel_requested_job_marks_cancelled() {
        let jobs = DashMap::new();
        let now = Instant::now();

        jobs.insert(
            "job-000010".to_string(),
            JobRecord {
                job_id: "job-000010".to_string(),
                kind: "simulated".to_string(),
                payload: "{}".to_string(),
                required_capabilities: Vec::new(),
                priority: JobPriority::Normal,
                state: JobState::CancelRequested,
                lease: Some(JobLease {
                    worker_id: "Worker-A".to_string(),
                    leased_at: now - Duration::from_secs(30),
                    lease_timeout: Duration::from_secs(10),
                }),
                output: None,
                error: Some("cancel requested".to_string()),
                created_at: now - Duration::from_secs(60),
                updated_at: now - Duration::from_secs(30),
            },
        );

        let requeued = requeue_expired_jobs(&jobs, now);
        assert_eq!(requeued, 0);

        let cancelled = jobs
            .get("job-000010")
            .expect("cancelled job should still exist in map");
        assert_eq!(cancelled.state, JobState::Cancelled);
        assert!(cancelled.lease.is_none());
    }
}
