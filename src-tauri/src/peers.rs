//! Group peer pool: discovers the serving rigs this rig may offload sub-agent
//! inference to (via the `list_inference_peers` RPC) and load-balances
//! dispatches across the ready ones. "Ready" = recently seen *and* currently
//! serving a model. The cache is refreshed lazily (short TTL) so a fan-out of
//! concurrent `run_agent` calls shares one network round-trip.

use crate::AppState;
use serde::Serialize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// A peer is a candidate only if its last heartbeat is within this window.
const FRESH_WINDOW_SECS: i64 = 90;
/// Don't re-hit the cloud more often than this while dispatching.
const REFRESH_TTL: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerInfo {
    pub rig_id: String,
    pub name: Option<String>,
    pub group_id: String,
    /// The model the peer currently serves (None → not ready).
    pub model: Option<String>,
    /// Fresh heartbeat and a loaded model — eligible to receive dispatches.
    pub ready: bool,
}

/// The concrete target chosen for one sub-agent dispatch.
pub struct PeerTarget {
    pub rig_id: String,
    pub group_id: String,
    pub model: String,
    pub name: Option<String>,
}

#[derive(Default)]
pub struct PeerPool {
    inner: Mutex<PoolState>,
    cursor: AtomicUsize,
}

#[derive(Default)]
struct PoolState {
    peers: Vec<PeerInfo>,
    last_refresh: Option<Instant>,
}

impl PeerPool {
    pub fn new() -> Self {
        Self::default()
    }

    /// Refresh the cache from the cloud if it is stale. Silent on error — keeps
    /// the previous snapshot; the dispatcher just falls back to local inference.
    async fn refresh_if_stale(&self, state: &Arc<AppState>) {
        {
            let s = self.inner.lock().await;
            if s.last_refresh.map(|t| t.elapsed() < REFRESH_TTL).unwrap_or(false) {
                return;
            }
        }
        let token = match crate::worker::ensure_token(state).await {
            Ok(t) => t,
            Err(_) => return,
        };
        let peers = match state.supabase.list_inference_peers(&token).await {
            Ok(p) => p,
            Err(e) => {
                tracing::debug!("peer refresh failed: {e}");
                return;
            }
        };
        let now = chrono::Utc::now();
        let mapped: Vec<PeerInfo> = peers
            .into_iter()
            .map(|p| {
                let fresh = p
                    .last_seen
                    .as_deref()
                    .and_then(|ls| chrono::DateTime::parse_from_rfc3339(ls).ok())
                    .map(|t| (now - t.with_timezone(&chrono::Utc)).num_seconds() <= FRESH_WINDOW_SECS)
                    .unwrap_or(false);
                let ready = fresh && p.loaded_model.is_some();
                PeerInfo {
                    rig_id: p.rig_id,
                    name: p.name,
                    group_id: p.group_id,
                    model: p.loaded_model,
                    ready,
                }
            })
            .collect();
        let mut s = self.inner.lock().await;
        s.peers = mapped;
        s.last_refresh = Some(Instant::now());
    }

    /// Refresh (if stale) and return the current snapshot — for the status UI.
    pub async fn snapshot(&self, state: &Arc<AppState>) -> Vec<PeerInfo> {
        self.refresh_if_stale(state).await;
        self.inner.lock().await.peers.clone()
    }

    /// Pick a serving peer for the next sub-agent dispatch (round-robin over
    /// ready peers), or None when the feature is off or nothing is ready.
    pub async fn pick(&self, state: &Arc<AppState>) -> Option<PeerTarget> {
        if !state.config.lock().await.use_group_subagents {
            return None;
        }
        self.refresh_if_stale(state).await;
        let s = self.inner.lock().await;
        let ready: Vec<&PeerInfo> = s.peers.iter().filter(|p| p.ready).collect();
        if ready.is_empty() {
            return None;
        }
        let i = self.cursor.fetch_add(1, Ordering::Relaxed) % ready.len();
        let p = ready[i];
        Some(PeerTarget {
            rig_id: p.rig_id.clone(),
            group_id: p.group_id.clone(),
            model: p.model.clone().unwrap_or_default(),
            name: p.name.clone(),
        })
    }
}
