use crate::telemetry::GpuStats;
use std::sync::{Arc, Mutex};

use std::time::Duration;
use tokio::time::sleep;

pub mod node {
    tonic::include_proto!("node");
}

use node::node_service_client::NodeServiceClient;
use node::HeartbeatRequest;

pub async fn start_client(state: Arc<Mutex<Option<GpuStats>>>) {
    loop {
        match NodeServiceClient::connect("http://[::1]:50051").await {
            Ok(mut client) => {
                // Inner loop
                loop {
                    let temp = {
                        let lock = state.lock().unwrap();
                        match &*lock {
                            Some(s) => s.temperature,
                            None => 0.0,
                        }
                    };

                    let request = tonic::Request::new(HeartbeatRequest {
                        node_id: "Node-01".to_string(),
                        gpu_temp: temp,
                    });

                    if let Err(_) = client.heartbeat(request).await {
                        break;
                    }

                    sleep(Duration::from_secs(2)).await
                }
            }
            Err(_) => {
                sleep(Duration::from_secs(5)).await;
            }
        }
    }
}
