use std::fs;
use std::path::PathBuf;

use anyhow::Context;

use super::{GpuStats, TelemetrySource};

#[derive(Debug)]
pub struct AmdSysfsSource {
    gpu_index: u32,
    card_device_path: PathBuf,
}

impl AmdSysfsSource {
    pub fn new(gpu_index: u32) -> anyhow::Result<Self> {
        let card_device_path = resolve_card_device_path(gpu_index)?;
        ensure_amdgpu_driver(&card_device_path)?;

        Ok(Self {
            gpu_index,
            card_device_path,
        })
    }
}

fn resolve_card_device_path(gpu_index: u32) -> anyhow::Result<PathBuf> {
    let card_device_path = PathBuf::from(format!("/sys/class/drm/card{gpu_index}/device"));

    if !card_device_path.exists() {
        anyhow::bail!(
            "DRM card index {} does not exist at {}",
            gpu_index,
            card_device_path.display()
        );
    }

    Ok(card_device_path)
}

fn ensure_amdgpu_driver(card_device_path: &PathBuf) -> anyhow::Result<()> {
    let uevent_path = card_device_path.join("uevent");
    let uevent = fs::read_to_string(&uevent_path)
        .with_context(|| format!("failed to read {}", uevent_path.display()))?;

    if uevent.lines().any(|line| line == "DRIVER=amdgpu") {
        return Ok(());
    }

    anyhow::bail!(
        "DRM device {} is not managed by amdgpu",
        card_device_path.display()
    )
}

impl TelemetrySource for AmdSysfsSource {
    fn backend_name(&self) -> &'static str {
        "amd-sysfs"
    }

    fn read_stats(&mut self, _node_id: &str) -> anyhow::Result<GpuStats> {
        anyhow::bail!(
            "amd-sysfs telemetry is not implemented yet for gpu-index {} at {}",
            self.gpu_index,
            self.card_device_path.display()
        )
    }
}
