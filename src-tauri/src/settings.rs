//! Static settings resolved from the environment at startup: which Supabase
//! project to talk to. Rig-specific credentials live in `config.rs`.

#[derive(Clone, Debug)]
pub struct Settings {
    pub supabase_url: String,
    pub anon_key: String,
    /// How often to sample + push telemetry.
    pub telemetry_interval_secs: u64,
    /// How often to poll for pending commands.
    pub command_poll_secs: u64,
    /// How often the reconciler drives toward desired state.
    pub reconcile_secs: u64,
}

impl Settings {
    pub fn from_env() -> Self {
        let supabase_url = std::env::var("LOCALLMOS_SUPABASE_URL")
            .unwrap_or_else(|_| "http://localhost:54321".to_string());
        let anon_key = std::env::var("LOCALLMOS_SUPABASE_ANON_KEY").unwrap_or_default();
        Self {
            supabase_url: supabase_url.trim_end_matches('/').to_string(),
            anon_key,
            telemetry_interval_secs: env_u64("LOCALLMOS_TELEMETRY_SECS", 10),
            command_poll_secs: env_u64("LOCALLMOS_COMMAND_POLL_SECS", 3),
            reconcile_secs: env_u64("LOCALLMOS_RECONCILE_SECS", 20),
        }
    }
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
