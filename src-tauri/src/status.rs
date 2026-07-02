//! Live status surfaced to the tray UI. Field names are camelCase to match the
//! TypeScript `AgentStatus` type consumed by the React frontend.

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
    pub runtime_state: Option<String>,
    pub loaded_model: Option<String>,
    pub cpu_pct: Option<f32>,
    pub gpu_name: Option<String>,
    pub gpu_util_pct: Option<f32>,
    pub last_error: Option<String>,
}
