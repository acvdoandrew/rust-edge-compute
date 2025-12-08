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

pub async fn run_monitoring_agent(sending_channel: mpsc::Sender<GpuStats>) {
    loop {
        let gpu_info = GpuStats {
            id: String::from("GPU-0"),
            temperature: rand::thread_rng().gen(),
            usage: rand::thread_rng().gen(),
            vram_used: rand::thread_rng().gen(),
        };

        sending_channel.send(gpu_info).await.unwrap();

        sleep(Duration::from_millis(1000)).await;
    }
}
