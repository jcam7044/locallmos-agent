//! Live status surfaced to the tray UI. Field names are camelCase to match the
//! TypeScript `AgentStatus` type consumed by the React frontend.

use crate::monitor::GpuStat;
use serde::Serialize;

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatus {
    pub enrolled: bool,
    pub rig_id: Option<String>,
    pub rig_name: Option<String>,
    /// True when the last sync round-trip to Supabase succeeded.
    pub connected: bool,
    pub runtime_kind: Option<String>,
    /// The active llama.cpp acceleration backend (cuda|rocm|vulkan|cpu|metal).
    pub runtime_backend: Option<String>,
    pub runtime_state: Option<String>,
    pub loaded_model: Option<String>,
    pub cpu_pct: Option<f32>,
    /// All detected GPUs (multi-GPU rigs report more than one).
    pub gpus: Vec<GpuStat>,
    pub last_error: Option<String>,
}
