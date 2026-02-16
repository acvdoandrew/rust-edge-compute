use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;

use super::{GpuStats, TelemetrySource};

#[derive(Debug)]
pub struct AmdSysfsSource {
    card_device_path: PathBuf,
}

impl AmdSysfsSource {
    pub fn new(gpu_index: u32) -> anyhow::Result<Self> {
        let card_device_path = resolve_card_device_path(gpu_index)?;
        ensure_amdgpu_driver(&card_device_path)?;

        Ok(Self { card_device_path })
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

    fn read_stats(&mut self, node_id: &str) -> anyhow::Result<GpuStats> {
        let usage_percent = read_u64_file(&self.card_device_path.join("gpu_busy_percent"))? as f32;
        let usage = (usage_percent / 100.0).clamp(0.0, 1.0);

        let vram_used = read_u64_file(&self.card_device_path.join("mem_info_vram_used"))?;

        let temperature = read_temperature_celsius(&self.card_device_path)?
            .unwrap_or(0.0)
            .max(0.0);

        Ok(GpuStats {
            id: node_id.to_string(),
            temperature,
            usage,
            vram_used,
        })
    }
}

fn read_u64_file(path: &Path) -> anyhow::Result<u64> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let value = raw
        .trim()
        .parse::<u64>()
        .with_context(|| format!("failed to parse numeric value from {}", path.display()))?;
    Ok(value)
}

fn read_temperature_celsius(card_device_path: &Path) -> anyhow::Result<Option<f32>> {
    let hwmon_root = card_device_path.join("hwmon");
    if !hwmon_root.exists() {
        return Ok(None);
    }

    for entry in fs::read_dir(&hwmon_root)
        .with_context(|| format!("failed to read {}", hwmon_root.display()))?
    {
        let entry =
            entry.with_context(|| format!("failed to read entry in {}", hwmon_root.display()))?;
        let temp_path = entry.path().join("temp1_input");

        if !temp_path.exists() {
            continue;
        }

        let milli_celsius = read_u64_file(&temp_path)? as f32;
        return Ok(Some(milli_celsius / 1000.0));
    }

    Ok(None)
}
