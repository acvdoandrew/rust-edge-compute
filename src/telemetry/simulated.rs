use rand::Rng;

use super::{GpuStats, TelemetrySource};

#[derive(Debug, Default)]
pub struct SimulatedSource;

impl SimulatedSource {
    pub fn new() -> Self {
        Self
    }
}

impl TelemetrySource for SimulatedSource {
    fn backend_name(&self) -> &'static str {
        "simulated"
    }

    fn read_stats(&mut self, node_id: &str) -> anyhow::Result<GpuStats> {
        let mut rng = rand::thread_rng();
        Ok(GpuStats {
            id: node_id.to_string(),
            temperature: rng.gen_range(40.0..90.0),
            usage: rng.gen_range(0.0..1.0),
            vram_used: rng.gen_range(1_000_000_000..24_000_000_000),
        })
    }
}
