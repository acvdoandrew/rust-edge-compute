pub mod client;
pub mod server;
pub mod telemetry;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Edge Compute Node Initializing...");
    // TODO: Initialize NVML
    // TODO: Start gRPC Client
    Ok(())
}
