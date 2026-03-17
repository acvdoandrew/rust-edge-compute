use rand::RngExt;

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
        let mut rng = rand::rng();
        Ok(GpuStats {
            id: node_id.to_string(),
            temperature: rng.random_range(40.0..90.0),
            usage: rng.random_range(0.0..1.0),
            vram_used: rng.random_range(1_000_000_000..24_000_000_000),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_stats_stay_within_expected_bounds() {
        let mut source = SimulatedSource::new();

        for _ in 0..256 {
            let stats = source
                .read_stats("Node-Bounds")
                .expect("simulated source should always generate stats");

            assert!(stats.temperature >= 40.0);
            assert!(stats.temperature < 90.0);
            assert!(stats.usage >= 0.0);
            assert!(stats.usage < 1.0);
            assert!(stats.vram_used >= 1_000_000_000);
            assert!(stats.vram_used < 24_000_000_000);
        }
    }
}
