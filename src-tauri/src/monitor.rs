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
}

impl Monitor {
    pub fn new() -> Self {
        // NVML loads libnvidia-ml at runtime; absence is fine (returns Err).
        let nvml = Nvml::init().ok();
        if nvml.is_none() {
            tracing::info!("NVML unavailable; GPU telemetry disabled");
        }
        Self { sys: System::new_all(), nvml }
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
        t
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
