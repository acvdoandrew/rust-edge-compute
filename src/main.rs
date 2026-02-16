use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

use rand::Rng;
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Style},
    widgets::{Block, Borders, Gauge, Paragraph},
};

use clap::{Parser, ValueEnum};
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

    #[arg(long = "telemetry-backend", value_enum, default_value_t = TelemetryBackendArg::Auto)]
    telemetry_backend: TelemetryBackendArg,

    #[arg(long = "gpu-index", default_value_t = 0)]
    gpu_index: u32,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum TelemetryBackendArg {
    Sim,
    Nvml,
    Auto,
}

impl TelemetryBackendArg {
    fn as_str(self) -> &'static str {
        match self {
            Self::Sim => "sim",
            Self::Nvml => "nvml",
            Self::Auto => "auto",
        }
    }
}

fn init_telemetry_source(
    backend: TelemetryBackendArg,
    gpu_index: u32,
) -> anyhow::Result<(Box<dyn telemetry::TelemetrySource>, String)> {
    init_telemetry_source_with_factory(backend, || init_nvml_source(gpu_index))
}

fn init_nvml_source(gpu_index: u32) -> anyhow::Result<Box<dyn telemetry::TelemetrySource>> {
    let source = telemetry::NvmlSource::new(gpu_index)
        .context("nvml backend requested but initialization failed")?;
    Ok(Box::new(source))
}

fn init_telemetry_source_with_factory<F>(
    backend: TelemetryBackendArg,
    mut init_nvml: F,
) -> anyhow::Result<(Box<dyn telemetry::TelemetrySource>, String)>
where
    F: FnMut() -> anyhow::Result<Box<dyn telemetry::TelemetrySource>>,
{
    match backend {
        TelemetryBackendArg::Sim => Ok((
            Box::new(telemetry::SimulatedSource::new()),
            "sim".to_string(),
        )),
        TelemetryBackendArg::Nvml => {
            let source = init_nvml()?;
            Ok((source, "nvml".to_string()))
        }
        TelemetryBackendArg::Auto => match init_nvml() {
            Ok(source) => Ok((source, "auto -> nvml".to_string())),
            Err(err) => Ok((
                Box::new(telemetry::SimulatedSource::new()),
                format!("auto -> sim (nvml unavailable: {err})"),
            )),
        },
    }
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
    println!(
        "Telemetry backend requested: {}",
        args.telemetry_backend.as_str()
    );
    println!("GPU index requested: {}", args.gpu_index);

    let (telemetry_source, backend_status) =
        init_telemetry_source(args.telemetry_backend, args.gpu_index)?;
    println!("Telemetry backend active: {}", backend_status);

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (tx, mut rx) = mpsc::channel(32);
    let telemetry_task = tokio::spawn(telemetry::run_monitoring_agent(
        tx,
        node_id.clone(),
        shutdown_rx.clone(),
        telemetry_source,
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

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    struct FixedSource {
        backend: &'static str,
    }

    impl telemetry::TelemetrySource for FixedSource {
        fn backend_name(&self) -> &'static str {
            self.backend
        }

        fn read_stats(&mut self, node_id: &str) -> anyhow::Result<telemetry::GpuStats> {
            Ok(telemetry::GpuStats {
                id: node_id.to_string(),
                temperature: 50.0,
                usage: 0.5,
                vram_used: 2_000_000_000,
            })
        }
    }

    fn fixed_source(backend: &'static str) -> Box<dyn telemetry::TelemetrySource> {
        Box::new(FixedSource { backend })
    }

    #[test]
    fn init_source_uses_sim_backend_without_nvml_factory() {
        let (source, status) = init_telemetry_source_with_factory(TelemetryBackendArg::Sim, || {
            panic!("nvml factory should not be called for sim backend")
        })
        .expect("sim backend should initialize");

        assert_eq!(status, "sim");
        assert_eq!(source.backend_name(), "simulated");
    }

    #[test]
    fn init_source_fails_fast_for_explicit_nvml_backend() {
        let result = init_telemetry_source_with_factory(TelemetryBackendArg::Nvml, || {
            Err(anyhow!("nvml unavailable"))
        });

        assert!(result.is_err());
        let err_text = result
            .err()
            .expect("explicit nvml backend should fail")
            .to_string();
        assert!(err_text.contains("nvml unavailable"));
    }

    #[test]
    fn init_source_auto_falls_back_to_sim_when_nvml_unavailable() {
        let (source, status) =
            init_telemetry_source_with_factory(TelemetryBackendArg::Auto, || {
                Err(anyhow!("nvml unavailable"))
            })
            .expect("auto backend should fall back to simulated source");

        assert_eq!(source.backend_name(), "simulated");
        assert!(status.starts_with("auto -> sim"));
    }

    #[test]
    fn init_source_auto_prefers_nvml_when_available() {
        let (source, status) =
            init_telemetry_source_with_factory(TelemetryBackendArg::Auto, || {
                Ok(fixed_source("nvml"))
            })
            .expect("auto backend should use nvml source when available");

        assert_eq!(status, "auto -> nvml");
        assert_eq!(source.backend_name(), "nvml");
    }
}
