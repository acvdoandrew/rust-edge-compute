use tokio::sync::mpsc;

pub mod client;
pub mod server;
pub mod telemetry;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🚀 Edge Compute Node Initializing...");

    let (tx, mut rx) = mpsc::channel(32);

    tokio::spawn(telemetry::run_monitoring_agent(tx));

    println!("🚀 Listening to telemetry...");

    while let Some(data) = rx.recv().await {
        println!("Received: {:?}", data);
    }

    println!("Telemetry stream ended");

    // TODO: Initialize NVML
    // TODO: Start gRPC Client
    Ok(())
}
