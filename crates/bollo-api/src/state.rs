//! Shared daemon state.
//!
//! All durable pieces are the same components the CLI uses (`SqliteStore`,
//! policy approval receipts); the API adds transport-scoped state only:
//! idempotency receipts, the SSE bus, rate windows and active-run bookkeeping.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

use bollo_policy::approval::MemoryApprovalStore;
use bollo_protocol::events::{EventEnvelope, EventType};
use bollo_protocol::ids::{RunId, SessionId};
use bollo_protocol::vocab::{Effect, Profile, SandboxCapabilities, SandboxMode};
use bollo_store::SqliteStore;

use crate::auth::ApiToken;
use crate::backend::RunBackend;
use crate::error::ApiError;
use crate::events::EventBus;
use crate::idempotency::IdempotencyStore;

pub type SharedState = Arc<ApiState>;

/// One effective policy rule, safe to display (no credentials, no file text).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuleView {
    pub id: String,
    pub effect: Effect,
    pub tool: String,
    pub path_glob: Option<String>,
    pub argv_prefix: Option<Vec<String>>,
}

/// Where a rule came from; the API never lets a project layer widen policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProvenanceView {
    pub rule_id: String,
    pub source: String,
    pub layer: String,
}

/// Immutable snapshot of the daemon's effective policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PolicySnapshot {
    pub revision: u64,
    pub profile: Profile,
    pub sandbox: SandboxMode,
    pub isolation_verified: bool,
    pub rules: Vec<RuleView>,
    pub provenance: Vec<ProvenanceView>,
}

/// Operational limits; defaults are the documented proposed targets.
#[derive(Debug, Clone)]
pub struct ApiLimits {
    /// Control requests per minute per token (SSE streams excluded).
    pub requests_per_minute: u32,
    /// Concurrent event streams per token.
    pub max_concurrent_streams: usize,
    /// Cap on HTTP request bodies.
    pub max_body_bytes: usize,
}

impl Default for ApiLimits {
    fn default() -> Self {
        Self {
            requests_per_minute: 60,
            max_concurrent_streams: 5,
            max_body_bytes: 1024 * 1024,
        }
    }
}

/// Everything the daemon needs to start. The CLI composition builds this;
/// tests build it directly.
pub struct ApiConfig {
    /// Loopback bind address; non-loopback is refused at startup.
    pub bind: SocketAddr,
    /// Workspace identity sessions are bound to (hash or id, never a secret).
    pub workspace_id: String,
    /// Human-readable workspace scope shown in approval views.
    pub workspace_scope: String,
    /// Private state directory for the store, receipts and artifacts.
    pub state_directory: PathBuf,
    pub provider: String,
    pub model: String,
    pub policy: PolicySnapshot,
    pub tokens: Vec<ApiToken>,
    /// Exact-match browser Origins; empty means no browser cross-origin use.
    pub allowed_origins: Vec<String>,
    pub limits: ApiLimits,
    pub backend: Arc<dyn RunBackend>,
}

impl std::fmt::Debug for ApiConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApiConfig")
            .field("bind", &self.bind)
            .field("workspace_id", &self.workspace_id)
            .field("state_directory", &self.state_directory)
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("tokens", &self.tokens)
            .field("allowed_origins", &self.allowed_origins)
            .finish_non_exhaustive()
    }
}

/// The running daemon's shared state.
pub struct ApiState {
    pub config: ApiConfig,
    store: Mutex<SqliteStore>,
    approvals: Mutex<MemoryApprovalStore>,
    idempotency: Mutex<IdempotencyStore>,
    bus: EventBus,
    rate: Mutex<HashMap<String, VecDeque<Instant>>>,
    streams: Mutex<usize>,
    active_runs: Mutex<BTreeMap<String, String>>,
    shutdown: AtomicBool,
    /// Probed at startup; capabilities report facts, not assumptions.
    pub sandbox: SandboxCapabilities,
    /// Actual bound address once the socket exists (ephemeral ports in tests).
    pub bound: Mutex<Option<SocketAddr>>,
}

impl ApiState {
    /// Open the store and receipt database under `state_directory`.
    pub fn bootstrap(config: ApiConfig) -> Result<SharedState, ApiError> {
        std::fs::create_dir_all(&config.state_directory)
            .map_err(|err| ApiError::storage(format!("create state directory: {err}")))?;
        let store = SqliteStore::open(&config.state_directory.join("bollo.sqlite"))
            .map_err(|err| ApiError::storage(format!("open store: {err}")))?;
        let receipts = IdempotencyStore::open(&config.state_directory.join("api-receipts.sqlite"))?;
        Self::with_parts(config, store, receipts)
    }

    /// Assemble state from already-opened parts (used by tests).
    pub fn with_parts(
        config: ApiConfig,
        store: SqliteStore,
        idempotency: IdempotencyStore,
    ) -> Result<SharedState, ApiError> {
        let mut ids = BTreeSet::new();
        for token in &config.tokens {
            if token.id.trim().is_empty() || token.secret.trim().is_empty() {
                return Err(ApiError::invalid_request(
                    "API tokens need a non-empty id and secret",
                ));
            }
            if !ids.insert(token.id.clone()) {
                return Err(ApiError::invalid_request(format!(
                    "duplicate API token id {}",
                    token.id
                )));
            }
        }
        let sandbox = bollo_workspace::sandbox::probe();
        Ok(Arc::new(Self {
            config,
            store: Mutex::new(store),
            approvals: Mutex::new(MemoryApprovalStore::new()),
            idempotency: Mutex::new(idempotency),
            bus: EventBus::new(),
            rate: Mutex::new(HashMap::new()),
            streams: Mutex::new(0),
            active_runs: Mutex::new(BTreeMap::new()),
            shutdown: AtomicBool::new(false),
            sandbox,
            bound: Mutex::new(None),
        }))
    }

    pub fn store(&self) -> MutexGuard<'_, SqliteStore> {
        self.store.lock().expect("store lock")
    }

    pub fn approvals(&self) -> MutexGuard<'_, MemoryApprovalStore> {
        self.approvals.lock().expect("approval lock")
    }

    pub fn idempotency(&self) -> MutexGuard<'_, IdempotencyStore> {
        self.idempotency.lock().expect("receipt lock")
    }

    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    /// Persist an event and fan it out to live subscribers.
    pub fn append_event(
        &self,
        session: &SessionId,
        run: Option<&RunId>,
        event_type: EventType,
        data: &Value,
    ) -> Result<EventEnvelope, ApiError> {
        let envelope = self
            .store()
            .append_event(session, run, event_type, data)
            .map_err(|err| ApiError::storage(format!("append event: {err}")))?;
        self.bus.publish(&envelope);
        Ok(envelope)
    }

    /// Fixed-window limiter over the last 60 seconds, per token principal.
    pub fn take_rate_slot(&self, principal: &str) -> Result<(), ApiError> {
        let limit = u64::from(self.config.limits.requests_per_minute);
        if limit == 0 {
            return Ok(());
        }
        let now = Instant::now();
        let window = Duration::from_secs(60);
        let mut windows = self.rate.lock().expect("rate lock");
        let entries = windows.entry(principal.to_string()).or_default();
        while let Some(front) = entries.front() {
            if now.duration_since(*front) >= window {
                entries.pop_front();
            } else {
                break;
            }
        }
        if entries.len() as u64 >= limit {
            let oldest = entries.front().copied().unwrap_or(now);
            let waited = now.duration_since(oldest);
            let retry_after = window
                .checked_sub(waited)
                .map(|remaining| remaining.as_secs().max(1))
                .unwrap_or(1);
            return Err(ApiError::rate_limited(retry_after));
        }
        entries.push_back(now);
        Ok(())
    }

    /// Reserve one of the bounded SSE stream slots; released on drop.
    pub fn enter_stream(self: &SharedState) -> Result<StreamSlot, ApiError> {
        let mut count = self.streams.lock().expect("stream lock");
        if *count >= self.config.limits.max_concurrent_streams {
            return Err(ApiError::rate_limited(5));
        }
        *count += 1;
        Ok(StreamSlot {
            state: Arc::clone(self),
        })
    }

    pub fn active_run(&self, session: &SessionId) -> Option<RunId> {
        self.active_runs
            .lock()
            .expect("active run lock")
            .get(session.as_str())
            .and_then(|run| RunId::parse(run.clone()).ok())
    }

    pub fn set_active(&self, session: &SessionId, run: &RunId) {
        self.active_runs
            .lock()
            .expect("active run lock")
            .insert(session.as_str().to_string(), run.as_str().to_string());
    }

    pub fn clear_active(&self, session: &SessionId) {
        self.active_runs
            .lock()
            .expect("active run lock")
            .remove(session.as_str());
    }

    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }

    pub fn mark_shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
}

/// RAII slot for the concurrent-stream ceiling.
pub struct StreamSlot {
    state: SharedState,
}

impl Drop for StreamSlot {
    fn drop(&mut self) {
        let mut count = self.state.streams.lock().expect("stream lock");
        *count = count.saturating_sub(1);
    }
}

/// True when the sandbox probe proves both containment properties.
pub fn sandbox_verified(sandbox: &SandboxCapabilities) -> bool {
    sandbox.filesystem_containment && sandbox.network_denied
}

/// Convenience for tests and the CLI: private state path.
pub fn default_state_directory(base: &Path) -> PathBuf {
    base.join("api")
}
