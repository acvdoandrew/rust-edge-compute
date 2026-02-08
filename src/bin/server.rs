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

use node::node_service_server::{NodeService, NodeServiceServer};
use node::{DisconnectRequest, DisconnectResponse, HeartbeatRequest, HeartbeatResponse};

const DEFAULT_BIND_ADDR: &str = "[::1]:50051";
const STALE_AFTER: Duration = Duration::from_secs(10);
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
    total_heartbeats: u64,
}

#[derive(Debug)]
pub struct MyNodeService {
    state: Arc<DashMap<String, NodeStatus>>,
    total_heartbeats: Arc<AtomicU64>,
}

fn update_node_status(
    state: &DashMap<String, NodeStatus>,
    node_id: &str,
    gpu_temp: f32,
    now: Instant,
) {
    match state.entry(node_id.to_string()) {
        Entry::Occupied(mut entry) => {
            let status = entry.get_mut();
            status.last_temp_c = gpu_temp;
            status.last_seen = now;
            status.heartbeat_count += 1;
        }
        Entry::Vacant(entry) => {
            entry.insert(NodeStatus {
                node_id: node_id.to_string(),
                last_temp_c: gpu_temp,
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

fn build_dashboard_snapshot(
    state: &DashMap<String, NodeStatus>,
    total_heartbeats: u64,
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
        "Active: {}  |  Stale: {}  |  Total Nodes: {}  |  Total Heartbeats: {}",
        snapshot.healthy_nodes,
        snapshot.stale_nodes,
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
            Cell::from(format!("{}s ago", row.last_seen_secs)),
            Cell::from(row.health.as_str()).style(status_style),
            Cell::from(row.heartbeat_count.to_string()),
        ])
    });

    let table = Table::new(
        table_rows,
        [
            Constraint::Length(20),
            Constraint::Length(10),
            Constraint::Length(12),
            Constraint::Length(10),
            Constraint::Min(10),
        ],
    )
    .header(Row::new([
        "Node ID",
        "Temp (C)",
        "Last Seen",
        "Status",
        "Heartbeats",
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
    loop {
        if *shutdown_rx.borrow() {
            break;
        }

        let snapshot = build_dashboard_snapshot(
            &state,
            total_heartbeats.load(Ordering::Relaxed),
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

        update_node_status(&self.state, &req.node_id, req.gpu_temp, now);
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let addr = args.bind.parse()?;
    println!("Orchestrator listening on {}", addr);

    let state = Arc::new(DashMap::new());
    let total_heartbeats = Arc::new(AtomicU64::new(0));

    let service = MyNodeService {
        state: state.clone(),
        total_heartbeats: total_heartbeats.clone(),
    };

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut server_shutdown_rx = shutdown_rx.clone();

    let server_task = tokio::spawn(async move {
        Server::builder()
            .add_service(NodeServiceServer::new(service))
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

    #[test]
    fn update_node_status_inserts_and_updates_existing_node() {
        let state = DashMap::new();
        let first = Instant::now();
        let second = first + Duration::from_secs(2);

        update_node_status(&state, "Node-1", 63.5, first);
        update_node_status(&state, "Node-1", 71.2, second);

        let node = state.get("Node-1").expect("node should exist");
        assert_eq!(node.heartbeat_count, 2);
        assert_eq!(node.last_temp_c, 71.2);
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
        let snapshot = build_dashboard_snapshot(&state, 0, STALE_AFTER);

        assert_eq!(snapshot.total_nodes, 0);
        assert_eq!(snapshot.healthy_nodes, 0);
        assert_eq!(snapshot.stale_nodes, 0);
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
                last_seen: now - Duration::from_secs(3),
                heartbeat_count: 4,
            },
        );
        state.insert(
            "Node-A".to_string(),
            NodeStatus {
                node_id: "Node-A".to_string(),
                last_temp_c: 80.0,
                last_seen: now - Duration::from_secs(15),
                heartbeat_count: 1,
            },
        );

        let snapshot = build_dashboard_snapshot(&state, 5, STALE_AFTER);

        assert_eq!(snapshot.total_nodes, 2);
        assert_eq!(snapshot.healthy_nodes, 1);
        assert_eq!(snapshot.stale_nodes, 1);
        assert_eq!(snapshot.total_heartbeats, 5);
        assert_eq!(snapshot.rows[0].node_id, "Node-A");
        assert_eq!(snapshot.rows[0].health, NodeHealth::Stale);
        assert_eq!(snapshot.rows[1].node_id, "Node-B");
        assert_eq!(snapshot.rows[1].health, NodeHealth::Healthy);
    }

    #[test]
    fn remove_node_deletes_existing_node() {
        let state = DashMap::new();
        state.insert(
            "Node-X".to_string(),
            NodeStatus {
                node_id: "Node-X".to_string(),
                last_temp_c: 55.0,
                last_seen: Instant::now(),
                heartbeat_count: 3,
            },
        );

        let removed = remove_node(&state, "Node-X");

        assert!(removed);
        assert!(state.get("Node-X").is_none());
    }
}
