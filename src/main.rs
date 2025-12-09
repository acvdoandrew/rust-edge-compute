use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::{Event, KeyCode};

use rand::Rng;
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Style},
    widgets::{Block, Borders, Gauge, Paragraph},
};

use tokio::sync::mpsc;

pub mod client;
pub mod telemetry;

struct AppState {
    should_quit: bool,
    latest_stats: Option<telemetry::GpuStats>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let node_id = format!("Node-{}", rand::thread_rng().gen_range(1000..9999));
    println!("🚀 Edge Compute Node Initializing ID: {}...", node_id);

    let (tx, mut rx) = mpsc::channel(32);
    tokio::spawn(telemetry::run_monitoring_agent(tx, node_id.clone()));

    let shared_state = Arc::new(Mutex::new(None));

    let client_state = shared_state.clone();
    tokio::spawn(client::start_client(client_state, node_id.clone()));

    let mut terminal = ratatui::init();

    let mut app_state = AppState {
        should_quit: false,
        latest_stats: None,
    };

    let app_result = loop {
        // DRAW PHASE
        terminal.draw(|frame| ui(frame, &app_state))?;

        if crossterm::event::poll(Duration::from_millis(16))? {
            if let Event::Key(key) = crossterm::event::read()? {
                if key.code == KeyCode::Char('q') {
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

    ratatui::restore();
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
