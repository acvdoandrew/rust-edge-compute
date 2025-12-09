use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::{Event, KeyCode};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout},
    prelude::*,
    style::{Color, Style},
    widgets::{Block, Borders, Gauge, Paragraph},
};

use tokio::sync::mpsc;

pub mod client;
pub mod server;
pub mod telemetry;

struct AppState {
    should_quit: bool,
    latest_stats: Option<telemetry::GpuStats>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🚀 Edge Compute Node Initializing...");

    let (tx, mut rx) = mpsc::channel(32);
    tokio::spawn(telemetry::run_monitoring_agent(tx));

    let shared_state = Arc::new(Mutex::new(None));

    let client_state = shared_state.clone();
    tokio::spawn(client::start_client(client_state));

    let mut terminal = setup_terminal()?;

    let mut app_state = AppState {
        should_quit: false,
        latest_stats: None,
    };

    loop {
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
            break;
        }
    }

    restore_terminal()?;
    println!("Telemetry stream ended.");
    Ok(())

    // TODO: Initialize NVML
    // TODO: Start gRPC Client
}

fn setup_terminal(
) -> Result<Terminal<CrosstermBackend<std::io::Stdout>>, Box<dyn std::error::Error>> {
    enable_raw_mode()?;

    let mut stdout = std::io::stdout();

    execute!(stdout, EnterAlternateScreen)?;

    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn ui(frame: &mut ratatui::Frame, state: &AppState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(frame.size());

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

fn restore_terminal() -> Result<(), Box<dyn std::error::Error>> {
    disable_raw_mode()?;

    execute!(std::io::stdout(), LeaveAlternateScreen)?;

    Ok(())
}
