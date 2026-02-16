use crate::telemetry::GpuStats;
use std::sync::{Arc, Mutex};

use std::time::Duration;
use tokio::sync::watch;
use tokio::time::sleep;

mod backoff;
use backoff::ExponentialBackoff;

pub mod node {
    tonic::include_proto!("node");
}

use node::node_service_client::NodeServiceClient;
use node::{DisconnectRequest, HeartbeatRequest};

pub async fn start_client(
    state: Arc<Mutex<Option<GpuStats>>>,
    node_id: String,
    server_addr: String,
    shutdown_rx: watch::Receiver<bool>,
) {
    let mut shutdown_rx = shutdown_rx;
    let mut reconnect_backoff = ExponentialBackoff::new(Duration::from_secs(1), Duration::from_secs(30));
    let mut should_backoff = false;

    loop {
        if *shutdown_rx.borrow() {
            break;
        }

        if should_backoff {
            let delay = reconnect_backoff.next_delay();

            tokio::select! {
                _ = sleep(delay) => {}
                changed = shutdown_rx.changed() => {
                    if changed.is_err() || *shutdown_rx.borrow() {
                        break;
                    }
                }
            }
        }

        let connect_result = tokio::select! {
            result = NodeServiceClient::connect(server_addr.clone()) => Some(result),
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow() {
                    None
                } else {
                    continue;
                }
            }
        };

        match connect_result {
            Some(Ok(mut client)) => {
                reconnect_backoff.reset();

                loop {
                    if *shutdown_rx.borrow() {
                        let _ = client
                            .disconnect(tonic::Request::new(DisconnectRequest {
                                node_id: node_id.clone(),
                            }))
                            .await;
                        return;
                    }

                    let temp = {
                        let lock = state.lock().unwrap();
                        match &*lock {
                            Some(s) => s.temperature,
                            None => 0.0,
                        }
                    };

                    let request = tonic::Request::new(HeartbeatRequest {
                        node_id: node_id.clone(),
                        gpu_temp: temp,
                    });

                    if client.heartbeat(request).await.is_err() {
                        should_backoff = true;
                        break;
                    }

                    tokio::select! {
                        _ = sleep(Duration::from_secs(2)) => {}
                        changed = shutdown_rx.changed() => {
                            if changed.is_err() || *shutdown_rx.borrow() {
                                let _ = client
                                    .disconnect(tonic::Request::new(DisconnectRequest {
                                        node_id: node_id.clone(),
                                    }))
                                    .await;
                                return;
                            }
                        }
                    }
                }
            }
            Some(Err(_)) => {
                should_backoff = true;
            }
            None => break,
        }
    }
}
