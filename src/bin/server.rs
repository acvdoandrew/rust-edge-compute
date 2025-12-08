use dashmap::DashMap;
use std::sync::Arc;
use tonic::{transport::Server, Request, Response, Status};

pub mod node {
    tonic::include_proto!("node");
}

use node::node_service_server::{NodeService, NodeServiceServer};
use node::{HeartbeatRequest, HeartbeatResponse};

#[derive(Debug)]
pub struct MyNodeService {
    // Reference to the state
    state: Arc<DashMap<String, f32>>,
}

#[tonic::async_trait]
impl NodeService for MyNodeService {
    async fn heartbeat(
        &self,
        request: Request<HeartbeatRequest>,
    ) -> Result<Response<HeartbeatResponse>, Status> {
        let req = request.into_inner();

        println!(
            "⚡ Heartbeat from {}: Temp {:.1}°C",
            req.node_id, req.gpu_temp
        );

        self.state.insert(req.node_id, req.gpu_temp);

        Ok(Response::new(HeartbeatResponse { acknowledged: true }))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "[::1]:50051".parse()?;
    println!("🚀 Orchestrator listening on {}", addr);

    let state = Arc::new(DashMap::new());

    let service = MyNodeService {
        state: state.clone(),
    };

    Server::builder()
        .add_service(NodeServiceServer::new(service))
        .serve(addr)
        .await?;

    Ok(())
}
