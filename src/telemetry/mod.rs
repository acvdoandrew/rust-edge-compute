use std::fmt::{self};

use tokio::sync::{mpsc, watch};
use tokio::time::{sleep, Duration};

mod amd_sysfs;
mod nvml;
mod simulated;

pub use amd_sysfs::AmdSysfsSource;
pub use nvml::NvmlSource;
pub use simulated::SimulatedSource;

#[derive(Debug, Clone)]
pub struct GpuStats {
    pub id: String,
    pub temperature: f32,
    pub usage: f32,
    pub vram_used: u64,
}

pub trait TelemetrySource: Send {
    fn backend_name(&self) -> &'static str;
    fn read_stats(&mut self, node_id: &str) -> anyhow::Result<GpuStats>;
}

impl fmt::Display for GpuStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let vram_as_gb = self.vram_used as f64 / 1_073_741_824.0;
        let usage_pct = self.usage * 100.0;

        write!(
            f,
            "{} | Temp: {:.1} | Usage: {:.1}% | VRAM: {:.2} GB",
            self.id, self.temperature, usage_pct, vram_as_gb,
        )
    }
}

pub async fn run_monitoring_agent(
    sending_channel: mpsc::Sender<GpuStats>,
    node_id: String,
    shutdown_rx: watch::Receiver<bool>,
    mut source: Box<dyn TelemetrySource>,
) {
    let mut shutdown_rx = shutdown_rx;

    loop {
        if *shutdown_rx.borrow() {
            break;
        }

        let gpu_info = match source.read_stats(&node_id) {
            Ok(gpu_info) => gpu_info,
            Err(err) => {
                eprintln!(
                    "telemetry read failed for {} (backend={}): {err}",
                    node_id,
                    source.backend_name()
                );

                tokio::select! {
                    _ = sleep(Duration::from_millis(1000)) => {}
                    changed = shutdown_rx.changed() => {
                        if changed.is_err() || *shutdown_rx.borrow() {
                            break;
                        }
                    }
                }

                continue;
            }
        };

        if sending_channel.send(gpu_info).await.is_err() {
            break;
        }

        tokio::select! {
            _ = sleep(Duration::from_millis(1000)) => {}
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow() {
                    break;
                }
            }
        }
    }
}
