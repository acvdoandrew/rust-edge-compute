use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

use rand::Rng;
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Style},
    widgets::{Block, Borders, Gauge, Paragraph},
};

use clap::Parser;
use tokio::sync::{mpsc, watch};

pub mod client;
pub mod telemetry;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(short = 's', long = "server", default_value = "http://[::1]:50051")]
    server_addr: String,

    #[arg(short = 'i', long = "id")]
    node_id: Option<String>,
}

struct AppState {
    should_quit: bool,
    latest_stats: Option<telemetry::GpuStats>,
}

fn is_quit_key(key: KeyEvent) -> bool {
    key.code == KeyCode::Char('q')
        || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let node_id = args
        .node_id
        .unwrap_or_else(|| format!("Node-{}", rand::thread_rng().gen_range(1000..9999)));

    println!("🚀 Edge Compute Node Initializing ID: {}...", node_id);

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (tx, mut rx) = mpsc::channel(32);
    let telemetry_task = tokio::spawn(telemetry::run_monitoring_agent(
        tx,
        node_id.clone(),
        shutdown_rx.clone(),
    ));

    let shared_state = Arc::new(Mutex::new(None));

    let client_state = shared_state.clone();
    let client_task = tokio::spawn(client::start_client(
        client_state,
        node_id.clone(),
        args.server_addr,
        shutdown_rx.clone(),
    ));

    let signal_shutdown_tx = shutdown_tx.clone();
    let signal_task = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = signal_shutdown_tx.send(true);
        }
    });

    let mut terminal = ratatui::init();

    let mut app_state = AppState {
        should_quit: false,
        latest_stats: None,
    };

    let app_result: Result<(), Box<dyn std::error::Error>> = loop {
        // DRAW PHASE
        terminal.draw(|frame| ui(frame, &app_state))?;

        if *shutdown_rx.borrow() {
            app_state.should_quit = true;
        }

        if crossterm::event::poll(Duration::from_millis(16))? {
            if let Event::Key(key) = crossterm::event::read()? {
                if is_quit_key(key) {
                    let _ = shutdown_tx.send(true);
                    app_state.should_quit = true;
                }
            }
        }

        match rx.try_recv() {
            Ok(stats) => {
                app_state.latest_stats = Some(stats.clone());

                let mut lock = shared_state.lock().unwrap();
                *lock = Some(stats);
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                // No data yet, we do nothing.
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                app_state.should_quit = true;
            }
        }

        if app_state.should_quit {
            break Ok(());
        }
    };

    let _ = shutdown_tx.send(true);
    ratatui::restore();

    signal_task.abort();
    if let Err(err) = telemetry_task.await {
        return Err(Box::new(err) as Box<dyn std::error::Error>);
    }
    if let Err(err) = client_task.await {
        return Err(Box::new(err) as Box<dyn std::error::Error>);
    }

    println!("Telemetry stream ended.");
    app_result
}

fn ui(frame: &mut ratatui::Frame, state: &AppState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(frame.area());

    let (text_content, usage_ratio) = match &state.latest_stats {
        Some(stats) => (format!("{}", stats), stats.usage as f64),

        None => ("Initializing...".to_string(), 0.0),
    };

    let paragraph = Paragraph::new(text_content)
        .block(Block::default().title(" Telemetry ").borders(Borders::ALL))
        .alignment(Alignment::Center);

    frame.render_widget(paragraph, chunks[0]);

    let gauge = Gauge::default()
        .block(Block::default().title(" GPU Load ").borders(Borders::ALL))
        .gauge_style(Style::default().fg(Color::Cyan))
        .ratio(usage_ratio);

    frame.render_widget(gauge, chunks[1]);
}
