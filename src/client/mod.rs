use crate::telemetry::GpuStats;
use std::sync::{Arc, Mutex};

use std::time::{Duration, Instant};
use tokio::sync::watch;
use tokio::time::sleep;

mod backoff;
use backoff::ExponentialBackoff;

pub mod node {
    tonic::include_proto!("node");
}

use node::job_service_client::JobServiceClient;
use node::node_service_client::NodeServiceClient;
use node::{DisconnectRequest, HeartbeatRequest, LeaseJobRequest, ReportJobResultRequest};

async fn execute_simulated_job(kind: &str, payload: &str) -> (bool, String, String) {
    sleep(Duration::from_millis(250)).await;

    if !kind.eq_ignore_ascii_case("simulated") {
        return (
            false,
            String::new(),
            format!("unsupported job kind: {kind}"),
        );
    }

    if payload.contains("fail") {
        return (
            false,
            String::new(),
            "simulated job failed due to payload directive".to_string(),
        );
    }

    (
        true,
        format!("simulated job completed (payload={payload})"),
        String::new(),
    )
}

pub async fn start_client(
    state: Arc<Mutex<Option<GpuStats>>>,
    node_id: String,
    server_addr: String,
    shutdown_rx: watch::Receiver<bool>,
) {
    let mut shutdown_rx = shutdown_rx;
    let mut reconnect_backoff =
        ExponentialBackoff::new(Duration::from_secs(1), Duration::from_secs(30));
    let mut should_backoff = false;
    let started_at = Instant::now();
    let client_version = env!("CARGO_PKG_VERSION").to_string();

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
            Some(Ok(mut node_client)) => {
                reconnect_backoff.reset();
                let mut job_client = JobServiceClient::connect(server_addr.clone()).await.ok();

                loop {
                    if *shutdown_rx.borrow() {
                        let _ = node_client
                            .disconnect(tonic::Request::new(DisconnectRequest {
                                node_id: node_id.clone(),
                            }))
                            .await;
                        return;
                    }

                    let (temp, usage, vram_used) = {
                        let lock = state.lock().unwrap();
                        match &*lock {
                            Some(s) => (s.temperature, s.usage, s.vram_used),
                            None => (0.0, 0.0, 0),
                        }
                    };

                    let request = tonic::Request::new(HeartbeatRequest {
                        node_id: node_id.clone(),
                        gpu_temp: temp,
                        gpu_usage: usage,
                        vram_used_bytes: vram_used,
                        uptime_seconds: started_at.elapsed().as_secs(),
                        client_version: client_version.clone(),
                    });

                    if node_client.heartbeat(request).await.is_err() {
                        should_backoff = true;
                        break;
                    }

                    if let Some(client) = job_client.as_mut() {
                        let lease_request = tonic::Request::new(LeaseJobRequest {
                            worker_id: node_id.clone(),
                        });
                        match client.lease_job(lease_request).await {
                            Ok(response) => {
                                let lease = response.into_inner();
                                if lease.has_job {
                                    eprintln!(
                                        "leased job {} (kind={})",
                                        lease.job_id,
                                        lease.kind
                                    );

                                    let (success, output, error) =
                                        execute_simulated_job(&lease.kind, &lease.payload).await;

                                    let report = tonic::Request::new(ReportJobResultRequest {
                                        worker_id: node_id.clone(),
                                        job_id: lease.job_id.clone(),
                                        success,
                                        output,
                                        error,
                                    });

                                    if client.report_job_result(report).await.is_err() {
                                        job_client = None;
                                    }
                                }
                            }
                            Err(_) => {
                                job_client = None;
                            }
                        }
                    }

                    tokio::select! {
                        _ = sleep(Duration::from_secs(2)) => {}
                        changed = shutdown_rx.changed() => {
                            if changed.is_err() || *shutdown_rx.borrow() {
                                let _ = node_client
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
