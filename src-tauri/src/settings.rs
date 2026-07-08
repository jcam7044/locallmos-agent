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
    /// How often to check for a newer agent release (opt-in auto-update).
    pub update_check_secs: u64,
}

const DEFAULT_SUPABASE_URL: &str = "https://fvpjkpfshbvszbcknkqq.supabase.co";
const DEFAULT_SUPABASE_ANON_KEY: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJzdXBhYmFzZSIsInJlZiI6ImZ2cGprcGZzaGJ2c3piY2tua3FxIiwicm9sZSI6ImFub24iLCJpYXQiOjE3ODI5NzI3MjYsImV4cCI6MjA5ODU0ODcyNn0.b0FDzCAweH6VIwcumLKjNP959unJCUN_egZpb7KdCwg";

impl Settings {
    pub fn from_env() -> Self {
        let supabase_url = std::env::var("LOCALLMOS_SUPABASE_URL")
            .unwrap_or_else(|_| DEFAULT_SUPABASE_URL.to_string());
        let anon_key = std::env::var("LOCALLMOS_SUPABASE_ANON_KEY")
            .unwrap_or_else(|_| DEFAULT_SUPABASE_ANON_KEY.to_string());
        Self {
            supabase_url: supabase_url.trim_end_matches('/').to_string(),
            anon_key,
            telemetry_interval_secs: env_u64("LOCALLMOS_TELEMETRY_SECS", 10),
            command_poll_secs: env_u64("LOCALLMOS_COMMAND_POLL_SECS", 3),
            reconcile_secs: env_u64("LOCALLMOS_RECONCILE_SECS", 20),
            update_check_secs: env_u64("LOCALLMOS_UPDATE_CHECK_SECS", 3600),
        }
    }
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
