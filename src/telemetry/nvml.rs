use anyhow::Context;
use nvml_wrapper::{enum_wrappers::device::TemperatureSensor, Nvml};

use super::{GpuStats, TelemetrySource};

#[derive(Debug)]
pub struct NvmlSource {
    nvml: Nvml,
    device_index: u32,
}

impl NvmlSource {
    pub fn new(device_index: u32) -> anyhow::Result<Self> {
        let nvml = Nvml::init().context("failed to initialize NVML")?;
        Ok(Self { nvml, device_index })
    }
}

impl TelemetrySource for NvmlSource {
    fn backend_name(&self) -> &'static str {
        "nvml"
    }

    fn read_stats(&mut self, node_id: &str) -> anyhow::Result<GpuStats> {
        let device = self
            .nvml
            .device_by_index(self.device_index)
            .context("failed to acquire NVML device handle")?;

        let temperature = device
            .temperature(TemperatureSensor::Gpu)
            .context("failed reading GPU temperature from NVML")? as f32;

        let utilization = device
            .utilization_rates()
            .context("failed reading utilization from NVML")?;

        let memory = device
            .memory_info()
            .context("failed reading memory info from NVML")?;

        Ok(GpuStats {
            id: node_id.to_string(),
            temperature,
            usage: (utilization.gpu as f32 / 100.0).clamp(0.0, 1.0),
            vram_used: memory.used,
        })
    }
}
