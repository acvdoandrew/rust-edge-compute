use std::time::Duration;
use tokio::time::sleep;

pub mod node {
    tonic::include_proto!("node");
}

use node::node_service_client::NodeServiceClient;
use node::HeartbeatRequest;

pub async fn start_client() {
    println!("📡 Client connecting to Orchestrator...");

    loop {
        match NodeServiceClient::connect("http://[::1]:50051").await {
            Ok(mut client) => {
                println!("✅ Connected to Orchestrator!");

                // Inner loop
                loop {
                    let request = tonic::Request::new(HeartbeatRequest {
                        node_id: "Node-01".to_string(),

                        gpu_temp: 65.0,
                    });

                    match client.heartbeat(request).await {
                        Ok(_) => {
                            // It works, silent success as I don't want to spam TUI logs
                        }
                        Err(e) => {
                            println!("❌ Heartbeat failed: {}", e);
                            break; // We break inner loop and trigger a reconnect
                        }
                    }

                    sleep(Duration::from_secs(2)).await;
                }
            }
            Err(e) => {
                // Server could be offline, wait and retry
                println!("⚠️ Connection failed: {}. Retrying in 5s...", e);
                sleep(Duration::from_secs(5)).await;
            }
        }
    }
}
