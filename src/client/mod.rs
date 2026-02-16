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
use node::{
    DisconnectRequest, ExtendJobLeaseRequest, HeartbeatRequest, LeaseJobRequest, LeaseJobResponse,
    ReportJobResultRequest,
};

const DEFAULT_SIMULATED_JOB_DURATION: Duration = Duration::from_millis(250);
const EXECUTION_STEP: Duration = Duration::from_millis(200);

fn parse_sleep_ms(payload: &str) -> Option<u64> {
    let marker = "sleep_ms=";
    let start = payload.find(marker)? + marker.len();
    let digits: String = payload[start..]
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();

    if digits.is_empty() {
        return None;
    }

    digits.parse::<u64>().ok()
}

async fn execute_simulated_job(
    job_client: &mut JobServiceClient<tonic::transport::Channel>,
    worker_id: &str,
    lease: &LeaseJobResponse,
) -> Result<(bool, String, String), tonic::Status> {
    if !lease.kind.eq_ignore_ascii_case("simulated") {
        return Ok((
            false,
            String::new(),
            format!("unsupported job kind: {}", lease.kind),
        ));
    }

    let run_for = parse_sleep_ms(&lease.payload)
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_SIMULATED_JOB_DURATION);

    let started_at = Instant::now();
    let mut next_renewal_at = started_at;

    while started_at.elapsed() < run_for {
        let now = Instant::now();
        if now >= next_renewal_at {
            let renewal = job_client
                .extend_job_lease(tonic::Request::new(ExtendJobLeaseRequest {
                    worker_id: worker_id.to_string(),
                    job_id: lease.job_id.clone(),
                }))
                .await?
                .into_inner();

            if renewal.cancel_requested {
                return Ok((
                    false,
                    String::new(),
                    "job cancelled by orchestrator request".to_string(),
                ));
            }

            let renew_in_secs = (renewal.lease_timeout_seconds / 2).max(1);
            next_renewal_at = now + Duration::from_secs(renew_in_secs);
        }

        let remaining = run_for.saturating_sub(started_at.elapsed());
        sleep(remaining.min(EXECUTION_STEP)).await;
    }

    if lease.payload.contains("fail") {
        return Ok((
            false,
            String::new(),
            "simulated job failed due to payload directive".to_string(),
        ));
    }

    Ok((
        true,
        format!("simulated job completed (payload={})", lease.payload),
        String::new(),
    ))
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
                                    eprintln!("leased job {} (kind={})", lease.job_id, lease.kind);

                                    let execution_result =
                                        execute_simulated_job(client, &node_id, &lease).await;

                                    let (success, output, error) = match execution_result {
                                        Ok(result) => result,
                                        Err(_) => {
                                            job_client = None;
                                            continue;
                                        }
                                    };

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
