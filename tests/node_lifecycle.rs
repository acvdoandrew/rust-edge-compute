use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

use rust_edge_compute::client::node::node_service_server::{NodeService, NodeServiceServer};
use rust_edge_compute::client::node::{
    DisconnectRequest, DisconnectResponse, HeartbeatRequest, HeartbeatResponse,
};
use rust_edge_compute::telemetry::GpuStats;
use tokio::sync::{oneshot, watch};
use tokio::time::{sleep, timeout};
use tonic::{transport::Server, Request, Response, Status};

#[derive(Default)]
struct LifecycleState {
    heartbeat_count: AtomicU64,
    disconnect_count: AtomicU64,
    last_heartbeat: Mutex<Option<HeartbeatRequest>>,
    last_disconnect_node: Mutex<Option<String>>,
}

#[derive(Clone)]
struct LifecycleService {
    state: Arc<LifecycleState>,
}

#[tonic::async_trait]
impl NodeService for LifecycleService {
    async fn heartbeat(
        &self,
        request: Request<HeartbeatRequest>,
    ) -> Result<Response<HeartbeatResponse>, Status> {
        let req = request.into_inner();
        self.state.heartbeat_count.fetch_add(1, Ordering::SeqCst);

        let mut lock = self.state.last_heartbeat.lock().unwrap();
        *lock = Some(req);

        Ok(Response::new(HeartbeatResponse { acknowledged: true }))
    }

    async fn disconnect(
        &self,
        request: Request<DisconnectRequest>,
    ) -> Result<Response<DisconnectResponse>, Status> {
        let req = request.into_inner();
        self.state.disconnect_count.fetch_add(1, Ordering::SeqCst);

        let mut lock = self.state.last_disconnect_node.lock().unwrap();
        *lock = Some(req.node_id);

        Ok(Response::new(DisconnectResponse { acknowledged: true }))
    }
}

async fn wait_until<F>(timeout_duration: Duration, mut condition: F) -> bool
where
    F: FnMut() -> bool,
{
    timeout(timeout_duration, async {
        loop {
            if condition() {
                break;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .is_ok()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn node_client_lifecycle_emits_heartbeat_then_disconnect() {
    let state = Arc::new(LifecycleState::default());
    let service = LifecycleService {
        state: state.clone(),
    };

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let addr = listener.local_addr().expect("read bound addr");
    drop(listener);

    let (server_shutdown_tx, server_shutdown_rx) = oneshot::channel::<()>();
    let server_task = tokio::spawn(async move {
        Server::builder()
            .add_service(NodeServiceServer::new(service))
            .serve_with_shutdown(addr, async move {
                let _ = server_shutdown_rx.await;
            })
            .await
    });

    let shared_state = Arc::new(Mutex::new(Some(GpuStats {
        id: "Node-IT".to_string(),
        temperature: 61.0,
        usage: 0.5,
        vram_used: 3_000_000_000,
    })));

    let (client_shutdown_tx, client_shutdown_rx) = watch::channel(false);
    let client_task = tokio::spawn(rust_edge_compute::client::start_client(
        shared_state,
        "Node-IT".to_string(),
        format!("http://{}", addr),
        client_shutdown_rx,
    ));

    assert!(
        wait_until(Duration::from_secs(8), || {
            state.heartbeat_count.load(Ordering::SeqCst) > 0
        })
        .await,
        "timed out waiting for heartbeat"
    );

    client_shutdown_tx
        .send(true)
        .expect("trigger client shutdown");

    timeout(Duration::from_secs(8), client_task)
        .await
        .expect("client task timed out")
        .expect("client task join failed");

    assert!(
        wait_until(Duration::from_secs(6), || {
            state.disconnect_count.load(Ordering::SeqCst) > 0
        })
        .await,
        "timed out waiting for disconnect"
    );

    let heartbeat = state
        .last_heartbeat
        .lock()
        .unwrap()
        .clone()
        .expect("expected at least one heartbeat");
    assert_eq!(heartbeat.node_id, "Node-IT");
    assert!(heartbeat.gpu_usage > 0.0);
    assert!(heartbeat.vram_used_bytes > 0);
    assert!(!heartbeat.client_version.is_empty());

    let disconnected_node = state
        .last_disconnect_node
        .lock()
        .unwrap()
        .clone()
        .expect("expected disconnect request");
    assert_eq!(disconnected_node, "Node-IT");

    let _ = server_shutdown_tx.send(());
    timeout(Duration::from_secs(8), server_task)
        .await
        .expect("server task timed out")
        .expect("server task join failed")
        .expect("server returned error");
}
