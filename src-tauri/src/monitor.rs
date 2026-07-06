//! System telemetry sampling: CPU/RAM/disk via `sysinfo`, NVIDIA GPUs via NVML.
//! Everything degrades gracefully — a missing GPU or sensor yields `None`, not
//! an error.

use nvml_wrapper::enum_wrappers::device::TemperatureSensor;
use nvml_wrapper::Nvml;
use serde::Serialize;
use serde_json::{json, Value};
use sysinfo::{Disks, System};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuStat {
    pub index: u32,
    pub name: Option<String>,
    pub vendor: String,
    pub utilization_pct: Option<f32>,
    pub memory_used_bytes: Option<u64>,
    pub memory_total_bytes: Option<u64>,
    pub temperature_c: Option<f32>,
    pub power_watts: Option<f32>,
}

#[derive(Clone, Debug, Default)]
pub struct Telemetry {
    pub cpu_utilization_pct: Option<f32>,
    pub cpu_temperature_c: Option<f32>,
    pub memory_used_bytes: Option<u64>,
    pub memory_total_bytes: Option<u64>,
    pub disk_used_bytes: Option<u64>,
    pub disk_total_bytes: Option<u64>,
    pub uptime_seconds: Option<u64>,
    pub gpus: Vec<GpuStat>,
}

impl Telemetry {
    /// Shape a row for `POST /rest/v1/rig_metrics`.
    pub fn to_insert(&self, rig_id: &str, ts: &str) -> Value {
        json!({
            "rig_id": rig_id,
            "ts": ts,
            "cpu_utilization_pct": self.cpu_utilization_pct,
            "cpu_temperature_c": self.cpu_temperature_c,
            "memory_used_bytes": self.memory_used_bytes,
            "memory_total_bytes": self.memory_total_bytes,
            "disk_used_bytes": self.disk_used_bytes,
            "disk_total_bytes": self.disk_total_bytes,
            "uptime_seconds": self.uptime_seconds,
            "gpus": self.gpus,
        })
    }
}

pub struct Monitor {
    sys: System,
    nvml: Option<Nvml>,
    /// Apple Silicon GPU name, resolved once (the chip model never changes).
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    apple_chip: Option<String>,
}

impl Monitor {
    pub fn new() -> Self {
        // NVML loads libnvidia-ml at runtime; absence is fine (returns Err).
        let nvml = Nvml::init().ok();
        if nvml.is_none() {
            tracing::info!("NVML unavailable; NVIDIA GPU telemetry disabled");
        }
        Self {
            sys: System::new_all(),
            nvml,
            #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
            apple_chip: apple_chip_name(),
        }
    }

    pub async fn sample(&mut self) -> Telemetry {
        // CPU usage needs two refreshes spaced by a short interval.
        self.sys.refresh_cpu_usage();
        tokio::time::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL).await;
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();

        let disks = Disks::new_with_refreshed_list();
        let (disk_total, disk_used) = disks.iter().fold((0u64, 0u64), |(t, u), d| {
            (t + d.total_space(), u + (d.total_space() - d.available_space()))
        });

        let mut t = Telemetry {
            cpu_utilization_pct: Some(self.sys.global_cpu_usage()),
            memory_used_bytes: Some(self.sys.used_memory()),
            memory_total_bytes: Some(self.sys.total_memory()),
            disk_used_bytes: Some(disk_used),
            disk_total_bytes: Some(disk_total),
            uptime_seconds: Some(System::uptime()),
            ..Default::default()
        };

        if let Some(nvml) = &self.nvml {
            t.gpus = collect_nvidia(nvml);
        }

        // Append non-NVIDIA GPUs, re-indexing so display keys stay unique across
        // sources. Each collector is best-effort and cfg-gated to its platform.
        let _base = t.gpus.len() as u32;
        #[cfg(target_os = "linux")]
        {
            let mut extra = collect_linux_sysfs();
            for (i, g) in extra.iter_mut().enumerate() {
                g.index = _base + i as u32;
            }
            t.gpus.extend(extra);
        }
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            let mut extra = self.collect_apple();
            for (i, g) in extra.iter_mut().enumerate() {
                g.index = _base + i as u32;
            }
            t.gpus.extend(extra);
        }

        t
    }

    /// Apple Silicon integrated GPU: reports the chip name and unified-memory
    /// total (== system RAM). Utilization/power are `None` — accurate figures
    /// need root `powermetrics --samplers gpu_power`, deferred for now.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    fn collect_apple(&self) -> Vec<GpuStat> {
        vec![GpuStat {
            index: 0, // re-indexed by the caller
            name: self.apple_chip.clone(),
            vendor: "apple".into(),
            utilization_pct: None,
            memory_used_bytes: None,
            memory_total_bytes: Some(self.sys.total_memory()),
            temperature_c: None,
            power_watts: None,
        }]
    }
}

fn collect_nvidia(nvml: &Nvml) -> Vec<GpuStat> {
    let mut out = Vec::new();
    let count = nvml.device_count().unwrap_or(0);
    for i in 0..count {
        let Ok(dev) = nvml.device_by_index(i) else { continue };
        let mem = dev.memory_info().ok();
        out.push(GpuStat {
            index: i,
            name: dev.name().ok(),
            vendor: "nvidia".into(),
            utilization_pct: dev.utilization_rates().ok().map(|u| u.gpu as f32),
            memory_used_bytes: mem.as_ref().map(|m| m.used),
            memory_total_bytes: mem.as_ref().map(|m| m.total),
            temperature_c: dev.temperature(TemperatureSensor::Gpu).ok().map(|v| v as f32),
            power_watts: dev.power_usage().ok().map(|mw| mw as f32 / 1000.0),
        });
    }
    out
}

/// Resolve the Apple Silicon chip name once (e.g. "Apple M2 Pro") via sysctl.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn apple_chip_name() -> Option<String> {
    let out = std::process::Command::new("sysctl")
        .args(["-n", "machdep.cpu.brand_string"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Detect AMD + Intel GPUs on Linux by walking `/sys/class/drm/card*/device`.
/// AMD (`amdgpu`) exposes tidy vram/busy sysfs files, so it gets full metrics;
/// Intel (`xe`/`i915`) does not, so it's a detection + name tier only. All reads
/// are best-effort — a missing node yields `None`, never an error.
#[cfg(target_os = "linux")]
fn collect_linux_sysfs() -> Vec<GpuStat> {
    use std::fs;
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/class/drm") else {
        return out;
    };
    // Keep only primary card nodes (cardN); skip render nodes and connector
    // subdirs (cardN-DP-1, etc., which contain a hyphen).
    let mut cards: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("card") && !n.contains('-'))
        .collect();
    cards.sort();
    for card in cards {
        let dev = format!("/sys/class/drm/{card}/device");
        let vendor = fs::read_to_string(format!("{dev}/vendor"))
            .ok()
            .map(|s| s.trim().to_lowercase());
        match vendor.as_deref() {
            Some("0x1002") => out.push(collect_amd(&dev)),
            Some("0x8086") => out.push(collect_intel(&dev)),
            _ => {}
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn read_sysfs_u64(path: &str) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[cfg(target_os = "linux")]
fn read_sysfs_f32(path: &str) -> Option<f32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Read a sensor file from the first `device/hwmon/hwmon*/` dir that has it.
#[cfg(target_os = "linux")]
fn hwmon_read(dev: &str, file: &str) -> Option<f32> {
    let entries = std::fs::read_dir(format!("{dev}/hwmon")).ok()?;
    for e in entries.filter_map(|e| e.ok()) {
        if let Some(v) = e.path().join(file).to_str().and_then(read_sysfs_f32) {
            return Some(v);
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn collect_amd(dev: &str) -> GpuStat {
    GpuStat {
        index: 0, // re-indexed by the caller
        // sysfs has no friendly name; the web falls back to the vendor string.
        name: None,
        vendor: "amd".into(),
        utilization_pct: read_sysfs_f32(&format!("{dev}/gpu_busy_percent")),
        memory_used_bytes: read_sysfs_u64(&format!("{dev}/mem_info_vram_used")),
        memory_total_bytes: read_sysfs_u64(&format!("{dev}/mem_info_vram_total")),
        temperature_c: hwmon_read(dev, "temp1_input").map(|v| v / 1000.0),
        power_watts: hwmon_read(dev, "power1_average").map(|v| v / 1_000_000.0),
    }
}

/// Intel detection + name tier. Full metrics (per-engine utilization, VRAM used)
/// need Level Zero/sysman or `intel_gpu_top` + root — deferred.
#[cfg(target_os = "linux")]
fn collect_intel(dev: &str) -> GpuStat {
    GpuStat {
        index: 0, // re-indexed by the caller
        name: std::fs::read_to_string(format!("{dev}/label"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        vendor: "intel".into(),
        utilization_pct: None,
        memory_used_bytes: None,
        // Best-effort: present on some drivers, absent on others.
        memory_total_bytes: read_sysfs_u64(&format!("{dev}/mem_info_vram_total")),
        temperature_c: None,
        power_watts: None,
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// Fresh temp dir standing in for a `/sys/class/drm/cardN/device` node.
    fn fake_dev(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("locallmos-mon-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn amd_parses_full_metrics() {
        let dev = fake_dev("amd");
        fs::write(dev.join("gpu_busy_percent"), "37\n").unwrap();
        fs::write(dev.join("mem_info_vram_used"), "1073741824\n").unwrap();
        fs::write(dev.join("mem_info_vram_total"), "17179869184\n").unwrap();
        let hw = dev.join("hwmon/hwmon3");
        fs::create_dir_all(&hw).unwrap();
        fs::write(hw.join("temp1_input"), "45000\n").unwrap(); // millidegrees
        fs::write(hw.join("power1_average"), "42000000\n").unwrap(); // microwatts

        let g = collect_amd(dev.to_str().unwrap());
        assert_eq!(g.vendor, "amd");
        assert_eq!(g.utilization_pct, Some(37.0));
        assert_eq!(g.memory_used_bytes, Some(1_073_741_824));
        assert_eq!(g.memory_total_bytes, Some(17_179_869_184));
        assert_eq!(g.temperature_c, Some(45.0));
        assert_eq!(g.power_watts, Some(42.0));
        let _ = fs::remove_dir_all(&dev);
    }

    #[test]
    fn amd_missing_files_yield_none_not_panic() {
        let dev = fake_dev("amd-empty");
        let g = collect_amd(dev.to_str().unwrap());
        assert_eq!(g.utilization_pct, None);
        assert_eq!(g.memory_total_bytes, None);
        assert_eq!(g.temperature_c, None);
        assert_eq!(g.power_watts, None);
        let _ = fs::remove_dir_all(&dev);
    }

    #[test]
    fn intel_light_tier_name_only() {
        let dev = fake_dev("intel");
        fs::write(dev.join("label"), "Arc Pro B50\n").unwrap(); // no amdgpu-style files
        let g = collect_intel(dev.to_str().unwrap());
        assert_eq!(g.vendor, "intel");
        assert_eq!(g.name.as_deref(), Some("Arc Pro B50"));
        assert_eq!(g.utilization_pct, None);
        assert_eq!(g.memory_used_bytes, None);
        assert_eq!(g.temperature_c, None);
        let _ = fs::remove_dir_all(&dev);
    }
}
