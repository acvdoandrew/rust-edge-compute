use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;

use super::{GpuStats, TelemetrySource};

const DRM_CLASS_PATH: &str = "/sys/class/drm";

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
    resolve_card_device_path_in(Path::new(DRM_CLASS_PATH), gpu_index)
}

fn resolve_card_device_path_in(drm_class_root: &Path, gpu_index: u32) -> anyhow::Result<PathBuf> {
    let card_device_path = drm_class_root
        .join(format!("card{gpu_index}"))
        .join("device");

    if !card_device_path.exists() {
        anyhow::bail!(
            "DRM card index {} does not exist at {}",
            gpu_index,
            card_device_path.display()
        );
    }

    Ok(card_device_path)
}

fn ensure_amdgpu_driver(card_device_path: &Path) -> anyhow::Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(prefix: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after UNIX_EPOCH")
                .as_nanos();

            let path =
                std::env::temp_dir().join(format!("{}-{}-{}", prefix, std::process::id(), unique));
            fs::create_dir_all(&path).expect("failed to create temporary test directory");

            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn resolve_card_device_path_in_finds_expected_device_path() {
        let tmp = TempDir::new("amd-sysfs-test");
        let card_device = tmp.path().join("card3").join("device");
        fs::create_dir_all(&card_device).expect("failed to create fake card path");

        let resolved =
            resolve_card_device_path_in(tmp.path(), 3).expect("expected fake card path to resolve");

        assert_eq!(resolved, card_device);
    }

    #[test]
    fn ensure_amdgpu_driver_rejects_non_amdgpu_devices() {
        let tmp = TempDir::new("amd-sysfs-test");
        fs::write(tmp.path().join("uevent"), "DRIVER=nouveau\n")
            .expect("failed to write fake uevent");

        let result = ensure_amdgpu_driver(tmp.path());

        assert!(result.is_err());
    }

    #[test]
    fn read_stats_uses_sysfs_metrics() {
        let tmp = TempDir::new("amd-sysfs-test");
        fs::write(tmp.path().join("uevent"), "DRIVER=amdgpu\n").expect("failed to write uevent");
        fs::write(tmp.path().join("gpu_busy_percent"), "75\n")
            .expect("failed to write usage value");
        fs::write(tmp.path().join("mem_info_vram_used"), "123456789\n")
            .expect("failed to write vram value");

        let temp_dir = tmp.path().join("hwmon").join("hwmon9");
        fs::create_dir_all(&temp_dir).expect("failed to create hwmon path");
        fs::write(temp_dir.join("temp1_input"), "64000\n").expect("failed to write temp value");

        let mut source = AmdSysfsSource {
            card_device_path: tmp.path().to_path_buf(),
        };
        let stats = source
            .read_stats("Node-AMD")
            .expect("expected sysfs telemetry read to succeed");

        assert_eq!(stats.id, "Node-AMD");
        assert_eq!(stats.vram_used, 123_456_789);
        assert!((stats.usage - 0.75).abs() < f32::EPSILON);
        assert!((stats.temperature - 64.0).abs() < f32::EPSILON);
    }
}
