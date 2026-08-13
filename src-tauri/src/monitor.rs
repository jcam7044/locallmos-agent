//! System telemetry sampling: CPU/RAM/disk via `sysinfo`, NVIDIA GPUs via NVML.
//! Everything degrades gracefully — a missing GPU or sensor yields `None`, not
//! an error.

use nvml_wrapper::enum_wrappers::device::TemperatureSensor;
use nvml_wrapper::Nvml;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use sysinfo::{Disk, Disks, MacAddr, Networks, System};

#[cfg(target_os = "linux")]
use std::collections::HashSet;

#[cfg(not(target_os = "linux"))]
use sysinfo::DiskKind;

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
    /// Raw OS device identifier (`nvme0n1`, `/dev/disk0`, `C:\\`, etc.).
    pub name: String,
    pub display_name: String,
    pub mount_points: Vec<String>,
    pub kind: String,
    pub transport: Option<String>,
    pub removable: bool,
    /// Filesystem usage aggregated across mounted partitions. Unmounted drives
    /// intentionally report `None` rather than a misleading zero.
    pub used_bytes: Option<u64>,
    pub total_bytes: u64,
    pub read_bytes_per_second: Option<f64>,
    pub write_bytes_per_second: Option<f64>,
    pub total_read_bytes: Option<u64>,
    pub total_written_bytes: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkStat {
    pub id: String,
    /// OS interface identifier (for example `enp7s0`).
    pub name: String,
    /// Human-facing connection label such as "Ethernet Connection".
    pub display_name: String,
    /// Best-effort controller model supplied by the operating system.
    pub hardware_name: Option<String>,
    pub interface_type: String,
    pub mac_address: Option<String>,
    pub ip_addresses: Vec<String>,
    pub received_bytes_per_second: Option<f64>,
    pub transmitted_bytes_per_second: Option<f64>,
    pub total_received_bytes: u64,
    pub total_transmitted_bytes: u64,
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
    pub networks: Vec<NetworkStat>,
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
    pub networks: Vec<NetworkStat>,
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
            networks: value.networks.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct DiskCounters {
    read_bytes: u64,
    write_bytes: u64,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Debug, Default)]
struct MountedDiskUsage {
    used_bytes: u64,
    mount_points: Vec<String>,
    /// Prevent bind mounts/subvolume aliases from counting one filesystem more
    /// than once when sysinfo returns duplicate mount records.
    sources: HashSet<String>,
}

#[derive(Clone, Debug)]
struct NetworkMetadata {
    display_name: String,
    hardware_name: Option<String>,
    interface_type: String,
}

pub struct Monitor {
    sys: System,
    nvml: Option<Nvml>,
    cpu_ready: bool,
    last_sample: Option<(Instant, Telemetry)>,
    disk_counters: HashMap<String, (Instant, DiskCounters)>,
    networks: Networks,
    last_network_refresh: Option<Instant>,
    network_metadata: HashMap<String, Option<NetworkMetadata>>,
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
            networks: Networks::new_with_refreshed_list(),
            last_network_refresh: Some(Instant::now()),
            network_metadata: HashMap::new(),
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
        // Preserve the existing cloud telemetry semantics: aggregate mounted,
        // fixed filesystems. The local Resources window independently models
        // physical hardware so unmounted and removable drives can appear.
        let telemetry_disks: Vec<&Disk> = disks.iter().filter(|d| is_fixed_volume(d)).collect();
        let (disk_total, disk_used) = telemetry_disks.iter().fold((0u64, 0u64), |(t, u), d| {
            (t + d.total_space(), u + (d.total_space() - d.available_space()))
        });
        let disk_stats = self.collect_disks(&disks, now);
        let network_stats = self.collect_networks(now);

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
            networks: network_stats,
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

    #[cfg(not(target_os = "linux"))]
    fn collect_disks(&mut self, disks: &Disks, now: Instant) -> Vec<DiskStat> {
        let mut active_counter_keys = Vec::new();
        let mut out = disks
            .iter()
            .filter(|disk| disk.total_space() > 0)
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
                    display_name: if name.is_empty() { mount_point.clone() } else { name.clone() },
                    name,
                    mount_points: vec![mount_point],
                    kind: disk_kind(disk.kind()).to_string(),
                    transport: None,
                    removable: disk.is_removable(),
                    used_bytes: Some(disk.total_space().saturating_sub(disk.available_space())),
                    total_bytes: disk.total_space(),
                    read_bytes_per_second: rates.and_then(|r| r.0),
                    write_bytes_per_second: rates.and_then(|r| r.1),
                    total_read_bytes: None,
                    total_written_bytes: None,
                }
            })
            .collect::<Vec<_>>();
        self.disk_counters
            .retain(|key, _| active_counter_keys.iter().any(|active| active == key));
        out.sort_by(|a, b| a.display_name.cmp(&b.display_name).then(a.name.cmp(&b.name)));
        out
    }

    #[cfg(target_os = "linux")]
    fn collect_disks(&mut self, disks: &Disks, now: Instant) -> Vec<DiskStat> {
        let mounted = linux_mounted_usage(disks);
        let Ok(entries) = std::fs::read_dir("/sys/block") else {
            return Vec::new();
        };
        let mut active_counter_keys = Vec::new();
        let mut out = entries
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| linux_is_physical_block_device(name))
            .filter_map(|name| {
                let base = std::path::PathBuf::from("/sys/block").join(&name);
                let total_bytes = read_trim_u64(&base.join("size"))?.saturating_mul(512);
                if total_bytes == 0 {
                    return None;
                }
                let counters = read_disk_counters(&name);
                let rates = counters.and_then(|current| {
                    active_counter_keys.push(name.clone());
                    let previous = self.disk_counters.insert(name.clone(), (now, current));
                    let (sampled, old) = previous?;
                    let elapsed = now.duration_since(sampled).as_secs_f64();
                    Some((
                        delta_rate(old.read_bytes, current.read_bytes, elapsed),
                        delta_rate(old.write_bytes, current.write_bytes, elapsed),
                    ))
                });
                let model = read_trim_string(&base.join("device/model"));
                let vendor = read_trim_string(&base.join("device/vendor"));
                let serial = read_trim_string(&base.join("device/serial"));
                let transport = linux_disk_transport(&name, &base);
                let removable = read_trim_u64(&base.join("removable")) == Some(1)
                    || transport.as_deref() == Some("usb");
                let rotational = read_trim_u64(&base.join("queue/rotational")) == Some(1);
                let kind = if name.starts_with("nvme") {
                    "nvme"
                } else if rotational {
                    "hdd"
                } else {
                    "ssd"
                };
                let display_name = disk_display_name(model.as_deref(), vendor.as_deref(), total_bytes);
                let usage = mounted.get(&name);
                Some(DiskStat {
                    id: serial
                        .filter(|serial| !serial.is_empty())
                        .map(|serial| format!("linux-disk:{serial}"))
                        .unwrap_or_else(|| format!("linux-disk:{name}")),
                    name: name.clone(),
                    display_name,
                    mount_points: usage.map(|usage| usage.mount_points.clone()).unwrap_or_default(),
                    kind: kind.into(),
                    transport,
                    removable,
                    used_bytes: usage.map(|usage| usage.used_bytes.min(total_bytes)),
                    total_bytes,
                    read_bytes_per_second: rates.and_then(|rates| rates.0),
                    write_bytes_per_second: rates.and_then(|rates| rates.1),
                    total_read_bytes: counters.map(|counter| counter.read_bytes),
                    total_written_bytes: counters.map(|counter| counter.write_bytes),
                })
            })
            .collect::<Vec<_>>();
        self.disk_counters
            .retain(|key, _| active_counter_keys.iter().any(|active| active == key));
        out.sort_by(|a, b| a.removable.cmp(&b.removable).then(a.name.cmp(&b.name)));
        out
    }

    fn collect_networks(&mut self, now: Instant) -> Vec<NetworkStat> {
        // refresh_list both discovers adapters and advances sysinfo's cumulative
        // counter baseline on every supported desktop platform.
        self.networks.refresh_list();
        let elapsed = self
            .last_network_refresh
            .replace(now)
            .map(|sampled| now.duration_since(sampled).as_secs_f64());
        let names = self.networks.keys().cloned().collect::<Vec<_>>();
        for name in names {
            self.network_metadata
                .entry(name.clone())
                .or_insert_with(|| detect_network_metadata(&name));
        }
        self.network_metadata
            .retain(|name, _| self.networks.contains_key(name));

        let mut out = self
            .networks
            .iter()
            .filter_map(|(name, network)| {
                let metadata = self.network_metadata.get(name)?.as_ref()?;
                let mac = network.mac_address();
                Some(NetworkStat {
                    id: network_id(name, mac),
                    name: name.clone(),
                    display_name: metadata.display_name.clone(),
                    hardware_name: metadata.hardware_name.clone(),
                    interface_type: metadata.interface_type.clone(),
                    mac_address: (!mac.is_unspecified()).then(|| mac.to_string()),
                    ip_addresses: network
                        .ip_networks()
                        .iter()
                        .map(|network| network.addr.to_string())
                        .collect(),
                    received_bytes_per_second: elapsed.and_then(|seconds| {
                        bytes_per_second(network.received(), seconds)
                    }),
                    transmitted_bytes_per_second: elapsed.and_then(|seconds| {
                        bytes_per_second(network.transmitted(), seconds)
                    }),
                    total_received_bytes: network.total_received(),
                    total_transmitted_bytes: network.total_transmitted(),
                })
            })
            .collect::<Vec<_>>();
        out.sort_by(|a, b| {
            a.interface_type
                .cmp(&b.interface_type)
                .then(a.display_name.cmp(&b.display_name))
                .then(a.name.cmp(&b.name))
        });
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

#[cfg(not(target_os = "linux"))]
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

#[cfg(target_os = "linux")]
fn linux_mounted_usage(disks: &Disks) -> HashMap<String, MountedDiskUsage> {
    let mut out: HashMap<String, MountedDiskUsage> = HashMap::new();
    for disk in disks.iter() {
        let source = disk.name().to_string_lossy().into_owned();
        if !source.starts_with("/dev/") {
            continue;
        }
        let Some(parent) = linux_parent_block_name(&source) else {
            continue;
        };
        let usage = out.entry(parent).or_default();
        if usage.sources.insert(source) {
            usage.used_bytes = usage.used_bytes.saturating_add(
                disk.total_space().saturating_sub(disk.available_space()),
            );
        }
        let mount = disk.mount_point().to_string_lossy().into_owned();
        if !usage.mount_points.contains(&mount) {
            usage.mount_points.push(mount);
        }
    }
    for usage in out.values_mut() {
        usage.mount_points.sort();
    }
    out
}

#[cfg(target_os = "linux")]
fn linux_parent_block_name(source: &str) -> Option<String> {
    let canonical = std::fs::canonicalize(source).unwrap_or_else(|_| source.into());
    let partition = canonical.file_name()?.to_str()?;
    let base = std::path::PathBuf::from("/sys/class/block").join(partition);
    if base.join("partition").exists() {
        std::fs::canonicalize(&base)
            .ok()?
            .parent()?
            .file_name()?
            .to_str()
            .map(str::to_owned)
    } else {
        Some(partition.to_owned())
    }
}

#[cfg(target_os = "linux")]
fn linux_is_physical_block_device(name: &str) -> bool {
    linux_is_physical_block_device_at(std::path::Path::new("/sys/block"), name)
}

#[cfg(target_os = "linux")]
fn linux_is_physical_block_device_at(root: &std::path::Path, name: &str) -> bool {
    if name.starts_with("loop") || name.starts_with("ram") || name.starts_with("zram") {
        return false;
    }
    let base = root.join(name);
    base.exists()
        && !base.join("partition").exists()
        && (base.join("device").exists() || name.starts_with("nvme") || name.starts_with("mmcblk"))
}

#[cfg(target_os = "linux")]
fn read_trim_string(path: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(target_os = "linux")]
fn read_trim_u64(path: &std::path::Path) -> Option<u64> {
    read_trim_string(path)?.parse().ok()
}

#[cfg(target_os = "linux")]
fn linux_disk_transport(name: &str, base: &std::path::Path) -> Option<String> {
    let canonical = std::fs::canonicalize(base).ok()?;
    let path = canonical.to_string_lossy();
    if path.contains("/usb") {
        Some("usb".into())
    } else if name.starts_with("nvme") || path.contains("/nvme/") {
        Some("nvme".into())
    } else if name.starts_with("mmcblk") {
        Some("mmc".into())
    } else if name.starts_with("sd") {
        Some("sata".into())
    } else {
        None
    }
}

fn disk_display_name(model: Option<&str>, vendor: Option<&str>, total_bytes: u64) -> String {
    let model = model.unwrap_or("").trim();
    let vendor = vendor.unwrap_or("").trim();
    let vendor = if matches!(vendor.to_ascii_lowercase().as_str(), "ata" | "nvme" | "scsi") {
        ""
    } else {
        vendor
    };
    let hardware = if model.is_empty() {
        vendor.to_string()
    } else if vendor.is_empty() || model.to_ascii_lowercase().starts_with(&vendor.to_ascii_lowercase()) {
        model.to_string()
    } else {
        format!("{vendor} {model}")
    };
    if hardware.is_empty() {
        format!("{} Drive", human_capacity(total_bytes))
    } else {
        hardware
    }
}

fn human_capacity(bytes: u64) -> String {
    const GB: f64 = 1_000_000_000.0;
    const TB: f64 = 1_000_000_000_000.0;
    if bytes as f64 >= TB {
        format!("{:.1} TB", bytes as f64 / TB)
    } else {
        format!("{:.0} GB", bytes as f64 / GB)
    }
}

fn delta_rate(previous: u64, current: u64, elapsed_seconds: f64) -> Option<f64> {
    if elapsed_seconds <= 0.0 || current < previous {
        return None;
    }
    Some((current - previous) as f64 / elapsed_seconds)
}

fn bytes_per_second(bytes: u64, elapsed_seconds: f64) -> Option<f64> {
    (elapsed_seconds > 0.0).then(|| bytes as f64 / elapsed_seconds)
}

fn network_id(name: &str, mac: MacAddr) -> String {
    if mac.is_unspecified() {
        format!("interface:{name}")
    } else {
        format!("mac:{mac}")
    }
}

#[cfg(target_os = "linux")]
fn detect_network_metadata(name: &str) -> Option<NetworkMetadata> {
    let base = std::path::PathBuf::from("/sys/class/net").join(name);
    // Kernel-created bridges, Docker devices, loopback, tunnels, and veth
    // endpoints live under devices/virtual and do not expose a backing device.
    if !linux_has_backing_device(&base) {
        return None;
    }
    let properties = std::process::Command::new("udevadm")
        .args(["info", "--query=property"])
        .arg(format!("--path={}", base.display()))
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
    let wireless = udev_property(&properties, "DEVTYPE").as_deref() == Some("wlan")
        || base.join("wireless").exists()
        || name.starts_with("wl");
    let hardware_name = udev_property(&properties, "ID_MODEL_FROM_DATABASE")
        .or_else(|| udev_property(&properties, "ID_MODEL").map(|value| value.replace('_', " ")));
    Some(connection_metadata(wireless, hardware_name))
}

#[cfg(target_os = "linux")]
fn linux_has_backing_device(base: &std::path::Path) -> bool {
    base.join("device").exists()
}

#[cfg(target_os = "linux")]
fn udev_property(properties: &str, key: &str) -> Option<String> {
    properties.lines().find_map(|line| {
        line.strip_prefix(key)
            .and_then(|value| value.strip_prefix('='))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

#[cfg(target_os = "macos")]
fn detect_network_metadata(name: &str) -> Option<NetworkMetadata> {
    let output = std::process::Command::new("networksetup")
        .arg("-listallhardwareports")
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    let text = String::from_utf8_lossy(&output.stdout);
    for block in text.split("\n\n") {
        let Some(port) = block
            .lines()
            .find_map(|line| line.strip_prefix("Hardware Port: "))
        else {
            continue;
        };
        let Some(device) = block
            .lines()
            .find_map(|line| line.strip_prefix("Device: "))
        else {
            continue;
        };
        if device == name {
            let wireless = port.to_ascii_lowercase().contains("wi-fi");
            return Some(NetworkMetadata {
                display_name: format!("{port} Connection"),
                hardware_name: None,
                interface_type: if wireless { "wifi" } else { "ethernet" }.into(),
            });
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn detect_network_metadata(name: &str) -> Option<NetworkMetadata> {
    // sysinfo's Windows collector already rejects disconnected and software
    // interfaces using the native MIB physical-interface table.
    let normalized = name.to_ascii_lowercase();
    let wireless = normalized.contains("wi-fi")
        || normalized.contains("wifi")
        || normalized.contains("wireless");
    let suffix = if normalized.ends_with("connection") {
        name.to_string()
    } else {
        format!("{name} Connection")
    };
    Some(NetworkMetadata {
        display_name: suffix,
        hardware_name: None,
        interface_type: if wireless { "wifi" } else { "ethernet" }.into(),
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn detect_network_metadata(_name: &str) -> Option<NetworkMetadata> {
    None
}

fn connection_metadata(wireless: bool, hardware_name: Option<String>) -> NetworkMetadata {
    NetworkMetadata {
        display_name: if wireless {
            "Wi-Fi Connection"
        } else {
            "Ethernet Connection"
        }
        .into(),
        hardware_name,
        interface_type: if wireless { "wifi" } else { "ethernet" }.into(),
    }
}

/// Linux exposes cumulative block counters without requiring elevated access.
/// Other platforms retain capacity telemetry and explicitly report I/O as
/// unavailable until an equally reliable native mapping exists.
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
            networks: vec![NetworkStat {
                id: "mac:00:11:22:33:44:55".into(),
                name: "Ethernet".into(),
                display_name: "Ethernet Connection".into(),
                hardware_name: Some("Test Controller".into()),
                interface_type: "ethernet".into(),
                mac_address: Some("00:11:22:33:44:55".into()),
                ip_addresses: vec!["192.0.2.10".into()],
                received_bytes_per_second: Some(100.0),
                transmitted_bytes_per_second: Some(50.0),
                total_received_bytes: 1_000,
                total_transmitted_bytes: 500,
            }],
            ..Default::default()
        };
        let value = serde_json::to_value(SystemMetricsSnapshot::from(&telemetry)).unwrap();
        assert_eq!(value["sampledAtMs"], 42);
        assert_eq!(value["gpus"][0]["id"], "nvidia:gpu-1");
        assert_eq!(value["networks"][0]["receivedBytesPerSecond"], 100.0);
        assert!(value.get("uptimeSeconds").is_none());
    }

    #[test]
    fn network_rate_and_ids_handle_first_party_and_fallback_adapters() {
        assert_eq!(bytes_per_second(4_000, 2.0), Some(2_000.0));
        assert_eq!(bytes_per_second(4_000, 0.0), None);
        assert_eq!(
            network_id("eth0", MacAddr([0, 1, 2, 3, 4, 5])),
            "mac:00:01:02:03:04:05"
        );
        assert_eq!(network_id("tun0", MacAddr::UNSPECIFIED), "interface:tun0");
        let ethernet = connection_metadata(false, Some("RTL8126 5GbE Controller".into()));
        assert_eq!(ethernet.display_name, "Ethernet Connection");
        assert_eq!(ethernet.interface_type, "ethernet");
        assert_eq!(
            ethernet.hardware_name.as_deref(),
            Some("RTL8126 5GbE Controller")
        );
        let wifi = connection_metadata(true, None);
        assert_eq!(wifi.display_name, "Wi-Fi Connection");
        assert_eq!(wifi.interface_type, "wifi");
    }

    #[test]
    fn linux_physical_filter_requires_a_backing_device_and_parses_friendly_model() {
        let root = fake_dev("network-physical-filter");
        assert!(!linux_has_backing_device(&root));
        fs::create_dir(root.join("device")).unwrap();
        assert!(linux_has_backing_device(&root));
        let properties = "DEVTYPE=wlan\nID_MODEL_FROM_DATABASE=RTL8922AE Wireless Adapter\n";
        assert_eq!(udev_property(properties, "DEVTYPE").as_deref(), Some("wlan"));
        assert_eq!(
            udev_property(properties, "ID_MODEL_FROM_DATABASE").as_deref(),
            Some("RTL8922AE Wireless Adapter")
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn physical_disk_filter_keeps_nvme_sata_and_usb_but_not_partitions_or_loops() {
        let root = fake_dev("block-filter");
        for name in ["nvme0n1", "sda", "sdb"] {
            fs::create_dir_all(root.join(name).join("device")).unwrap();
        }
        fs::create_dir_all(root.join("sda1/partition")).unwrap();
        fs::create_dir_all(root.join("loop0/device")).unwrap();
        assert!(linux_is_physical_block_device_at(&root, "nvme0n1"));
        assert!(linux_is_physical_block_device_at(&root, "sda"));
        assert!(linux_is_physical_block_device_at(&root, "sdb"));
        assert!(!linux_is_physical_block_device_at(&root, "sda1"));
        assert!(!linux_is_physical_block_device_at(&root, "loop0"));
        assert_eq!(
            disk_display_name(Some("PC SN5000S"), Some("WD"), 512_000_000_000),
            "WD PC SN5000S"
        );
        assert_eq!(disk_display_name(None, None, 32_000_000_000), "32 GB Drive");
        assert_eq!(
            disk_display_name(Some("TS32GSSD370"), Some("ATA"), 32_000_000_000),
            "TS32GSSD370"
        );
        let _ = fs::remove_dir_all(&root);
    }
}
