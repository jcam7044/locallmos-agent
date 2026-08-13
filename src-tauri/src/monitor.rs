//! System telemetry sampling: CPU/RAM/disk via `sysinfo`, NVIDIA GPUs via NVML.
//! Everything degrades gracefully — a missing GPU or sensor yields `None`, not
//! an error.

use nvml_wrapper::enum_wrappers::device::TemperatureSensor;
use nvml_wrapper::Nvml;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use sysinfo::{Disk, DiskKind, Disks, System};

const SAMPLE_CACHE_TTL: Duration = Duration::from_millis(750);

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuStat {
    /// Stable within a machine, unlike the presentation index which can change
    /// when a device is attached or a collector is unavailable.
    pub id: String,
    pub index: u32,
    pub name: Option<String>,
    pub vendor: String,
    pub utilization_pct: Option<f32>,
    pub memory_used_bytes: Option<u64>,
    pub memory_total_bytes: Option<u64>,
    pub temperature_c: Option<f32>,
    pub power_watts: Option<f32>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskStat {
    pub id: String,
    pub name: String,
    pub mount_point: String,
    pub kind: String,
    pub used_bytes: u64,
    pub total_bytes: u64,
    pub read_bytes_per_second: Option<f64>,
    pub write_bytes_per_second: Option<f64>,
}

/// Local IPC contract for the Resources window. This intentionally contains no
/// runtime/model state and is safe to request frequently while offline.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemMetricsSnapshot {
    pub sampled_at_ms: u64,
    pub cpu_utilization_pct: Option<f32>,
    pub memory_used_bytes: Option<u64>,
    pub memory_total_bytes: Option<u64>,
    pub gpus: Vec<GpuStat>,
    pub disks: Vec<DiskStat>,
}

#[derive(Clone, Debug, Default)]
pub struct Telemetry {
    pub sampled_at_ms: u64,
    pub cpu_utilization_pct: Option<f32>,
    pub cpu_temperature_c: Option<f32>,
    pub memory_used_bytes: Option<u64>,
    pub memory_total_bytes: Option<u64>,
    pub disk_used_bytes: Option<u64>,
    pub disk_total_bytes: Option<u64>,
    pub uptime_seconds: Option<u64>,
    pub gpus: Vec<GpuStat>,
    pub disks: Vec<DiskStat>,
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

impl From<&Telemetry> for SystemMetricsSnapshot {
    fn from(value: &Telemetry) -> Self {
        Self {
            sampled_at_ms: value.sampled_at_ms,
            cpu_utilization_pct: value.cpu_utilization_pct,
            memory_used_bytes: value.memory_used_bytes,
            memory_total_bytes: value.memory_total_bytes,
            gpus: value.gpus.clone(),
            disks: value.disks.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct DiskCounters {
    read_bytes: u64,
    write_bytes: u64,
}

pub struct Monitor {
    sys: System,
    nvml: Option<Nvml>,
    cpu_ready: bool,
    last_sample: Option<(Instant, Telemetry)>,
    disk_counters: HashMap<String, (Instant, DiskCounters)>,
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
            cpu_ready: false,
            last_sample: None,
            disk_counters: HashMap::new(),
            #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
            apple_chip: apple_chip_name(),
        }
    }

    pub async fn sample(&mut self) -> Telemetry {
        let now = Instant::now();
        if let Some((sampled, telemetry)) = &self.last_sample {
            if now.duration_since(*sampled) < SAMPLE_CACHE_TTL {
                return telemetry.clone();
            }
        }

        // Only the first sample needs two spaced refreshes. Subsequent callers
        // arrive at graph/dashboard cadence and can use the previous baseline.
        if !self.cpu_ready {
            self.sys.refresh_cpu_usage();
            tokio::time::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL).await;
            self.cpu_ready = true;
        }
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();

        let disks = Disks::new_with_refreshed_list();
        let visible_disks: Vec<&Disk> = disks.iter().filter(|d| is_fixed_volume(d)).collect();
        let (disk_total, disk_used) = visible_disks.iter().fold((0u64, 0u64), |(t, u), d| {
            (t + d.total_space(), u + (d.total_space() - d.available_space()))
        });
        let disk_stats = self.collect_disks(&visible_disks, now);

        let mut t = Telemetry {
            sampled_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            cpu_utilization_pct: Some(self.sys.global_cpu_usage()),
            memory_used_bytes: Some(self.sys.used_memory()),
            memory_total_bytes: Some(self.sys.total_memory()),
            disk_used_bytes: Some(disk_used),
            disk_total_bytes: Some(disk_total),
            uptime_seconds: Some(System::uptime()),
            disks: disk_stats,
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

        self.last_sample = Some((now, t.clone()));
        t
    }

    fn collect_disks(&mut self, disks: &[&Disk], now: Instant) -> Vec<DiskStat> {
        let mut active_counter_keys = Vec::new();
        let mut out = disks
            .iter()
            .map(|disk| {
                let name = disk.name().to_string_lossy().into_owned();
                let mount_point = disk.mount_point().to_string_lossy().into_owned();
                let id = format!("{name}@{mount_point}");
                let counter_key = disk_counter_key(&name);
                let counters = counter_key.as_deref().and_then(read_disk_counters);
                let rates = counters.and_then(|current| {
                    let key = counter_key.as_ref()?;
                    active_counter_keys.push(key.clone());
                    let previous = self.disk_counters.insert(key.clone(), (now, current));
                    let (sampled, old) = previous?;
                    let elapsed = now.duration_since(sampled).as_secs_f64();
                    Some((
                        delta_rate(old.read_bytes, current.read_bytes, elapsed),
                        delta_rate(old.write_bytes, current.write_bytes, elapsed),
                    ))
                });
                DiskStat {
                    id,
                    name,
                    mount_point,
                    kind: disk_kind(disk.kind()).to_string(),
                    used_bytes: disk.total_space().saturating_sub(disk.available_space()),
                    total_bytes: disk.total_space(),
                    read_bytes_per_second: rates.and_then(|r| r.0),
                    write_bytes_per_second: rates.and_then(|r| r.1),
                }
            })
            .collect::<Vec<_>>();
        self.disk_counters
            .retain(|key, _| active_counter_keys.iter().any(|active| active == key));
        out.sort_by(|a, b| a.mount_point.cmp(&b.mount_point).then(a.name.cmp(&b.name)));
        out
    }

    /// Apple Silicon integrated GPU: reports the chip name and unified-memory
    /// total (== system RAM). Utilization/power are `None` — accurate figures
    /// need root `powermetrics --samplers gpu_power`, deferred for now.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    fn collect_apple(&self) -> Vec<GpuStat> {
        vec![GpuStat {
            id: "apple:integrated".into(),
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

fn disk_kind(kind: DiskKind) -> &'static str {
    match kind {
        DiskKind::HDD => "hdd",
        DiskKind::SSD => "ssd",
        DiskKind::Unknown(_) => "unknown",
    }
}

fn is_fixed_volume(disk: &Disk) -> bool {
    if disk.is_removable() || disk.total_space() == 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        // sysinfo already excludes tmpfs and network mounts. Requiring a real
        // block-device path also filters container overlay/pseudo mounts.
        return disk.name().to_string_lossy().starts_with("/dev/");
    }
    #[cfg(not(target_os = "linux"))]
    true
}

fn delta_rate(previous: u64, current: u64, elapsed_seconds: f64) -> Option<f64> {
    if elapsed_seconds <= 0.0 || current < previous {
        return None;
    }
    Some((current - previous) as f64 / elapsed_seconds)
}

/// Linux exposes cumulative block counters without requiring elevated access.
/// Other platforms retain capacity telemetry and explicitly report I/O as
/// unavailable until an equally reliable native mapping exists.
#[cfg(target_os = "linux")]
fn disk_counter_key(name: &str) -> Option<String> {
    let canonical = std::fs::canonicalize(name).unwrap_or_else(|_| name.into());
    canonical.file_name()?.to_str().map(str::to_owned)
}

#[cfg(not(target_os = "linux"))]
fn disk_counter_key(_name: &str) -> Option<String> {
    None
}

#[cfg(target_os = "linux")]
fn read_disk_counters(device: &str) -> Option<DiskCounters> {
    // /sys block stats use 512-byte sectors for fields 3 and 7. Partition
    // nodes expose the same layout, which lets mounted volumes remain distinct.
    let raw = std::fs::read_to_string(format!("/sys/class/block/{device}/stat")).ok()?;
    let values = raw
        .split_whitespace()
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    Some(DiskCounters {
        read_bytes: values.get(2)?.saturating_mul(512),
        write_bytes: values.get(6)?.saturating_mul(512),
    })
}

#[cfg(not(target_os = "linux"))]
fn read_disk_counters(_device: &str) -> Option<DiskCounters> {
    None
}

fn collect_nvidia(nvml: &Nvml) -> Vec<GpuStat> {
    let mut out = Vec::new();
    let count = nvml.device_count().unwrap_or(0);
    for i in 0..count {
        let Ok(dev) = nvml.device_by_index(i) else { continue };
        let mem = dev.memory_info().ok();
        out.push(GpuStat {
            id: dev.uuid().unwrap_or_else(|_| format!("nvidia:{i}")),
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
        id: gpu_sysfs_id("amd", dev),
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
        id: gpu_sysfs_id("intel", dev),
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

#[cfg(target_os = "linux")]
fn gpu_sysfs_id(vendor: &str, dev: &str) -> String {
    let suffix = std::fs::canonicalize(dev)
        .ok()
        .and_then(|path| path.file_name().map(|name| name.to_string_lossy().into_owned()))
        .unwrap_or_else(|| dev.to_string());
    format!("{vendor}:{suffix}")
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

    #[test]
    fn disk_rate_handles_normal_reset_and_zero_elapsed_counters() {
        assert_eq!(delta_rate(1_000, 4_000, 2.0), Some(1_500.0));
        assert_eq!(delta_rate(4_000, 1_000, 2.0), None);
        assert_eq!(delta_rate(1_000, 4_000, 0.0), None);
    }

    #[test]
    fn system_snapshot_serializes_the_local_ipc_contract() {
        let telemetry = Telemetry {
            sampled_at_ms: 42,
            cpu_utilization_pct: Some(12.5),
            memory_used_bytes: Some(10),
            memory_total_bytes: Some(20),
            gpus: vec![GpuStat {
                id: "nvidia:gpu-1".into(),
                index: 0,
                name: Some("GPU".into()),
                vendor: "nvidia".into(),
                utilization_pct: Some(25.0),
                memory_used_bytes: Some(5),
                memory_total_bytes: Some(10),
                temperature_c: None,
                power_watts: None,
            }],
            disks: vec![],
            ..Default::default()
        };
        let value = serde_json::to_value(SystemMetricsSnapshot::from(&telemetry)).unwrap();
        assert_eq!(value["sampledAtMs"], 42);
        assert_eq!(value["gpus"][0]["id"], "nvidia:gpu-1");
        assert!(value.get("uptimeSeconds").is_none());
    }
}
