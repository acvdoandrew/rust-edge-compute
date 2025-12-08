use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

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

    let mut terminal = setup_terminal()?;

    let mut app_state = AppState {
        should_quit: false,
        latest_stats: None,
    };

    loop {
        // DRAW PHASE
        terminal.draw(|frame| ui(frame, &app_state))?;

        if crossterm::event::poll(std::time::Duration::from_millis(16))? {
            if let crossterm::event::Event::Key(key) = crossterm::event::read()? {
                if key.code == crossterm::event::KeyCode::Char('q') {
                    app_state.should_quit = true;
                }
            }
        }

        match rx.try_recv() {
            Ok(stats) => {
                app_state.latest_stats = Some(stats);
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
    let size = frame.size();
    let block = ratatui::widgets::Block::default()
        .title(" Edge Compute Node ")
        .borders(ratatui::widgets::Borders::ALL);

    frame.render_widget(block, size);
}

fn restore_terminal() -> Result<(), Box<dyn std::error::Error>> {
    disable_raw_mode()?;

    execute!(std::io::stdout(), LeaveAlternateScreen)?;

    Ok(())
}
