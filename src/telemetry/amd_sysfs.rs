use super::{GpuStats, TelemetrySource};

#[derive(Debug)]
pub struct AmdSysfsSource {
    gpu_index: u32,
}

impl AmdSysfsSource {
    pub fn new(gpu_index: u32) -> anyhow::Result<Self> {
        Ok(Self { gpu_index })
    }
}

impl TelemetrySource for AmdSysfsSource {
    fn backend_name(&self) -> &'static str {
        "amd-sysfs"
    }

    fn read_stats(&mut self, _node_id: &str) -> anyhow::Result<GpuStats> {
        anyhow::bail!(
            "amd-sysfs telemetry is not implemented yet for gpu-index {}",
            self.gpu_index
        )
    }
}
