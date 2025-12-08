use std::fmt::{self};

use rand::prelude::*;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

#[derive(Debug, Clone)]
pub struct GpuStats {
    id: String,
    temperature: f32,
    usage: f32,
    vram_used: u64,
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

pub async fn run_monitoring_agent(sending_channel: mpsc::Sender<GpuStats>) {
    loop {
        let gpu_info = GpuStats {
            id: String::from("GPU-0"),
            temperature: rand::thread_rng().gen_range(40.0..90.0),
            usage: rand::thread_rng().gen_range(0.0..1.0),
            vram_used: rand::thread_rng().gen_range(1_000_000_000..24_000_000_000),
        };

        sending_channel.send(gpu_info).await.unwrap();

        sleep(Duration::from_millis(1000)).await;
    }
}
