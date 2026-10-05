//! Loopback HTTP transport.
//!
//! Routing follows `docs/reference/api.md` exactly: `/v1` prefix, bearer on
//! every route, capability checks per route, idempotent creations, SSE with
//! numeric-seq cursors and replay from the durable journal. Request bodies are
//! bounded before parsing.

use std::collections::VecDeque;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use serde::Serialize;

use bollo_policy::approval::{ApprovalState, ApprovalStore};
use bollo_protocol::events::EventEnvelope;
use bollo_protocol::ids::{ApprovalId, ArtifactId, RunId, SessionId};
use bollo_protocol::vocab::{TerminalState, VerificationStatus};
use bollo_store::{RunDetail, StoreError};

use crate::auth::{self, ApiToken, Capability};
use crate::backend::BackendRun;
use crate::dto::{
    run_state, ApprovalDecisionRequest, ApprovalDto, ApprovalResultDto, ArtifactDto,
    CancelResultDto, CapabilitiesDto, CreateRunRequest, CreateSessionRequest, HealthDto, PolicyDto,
    RunDto, SessionDto,
};
use crate::error::{request_id, ApiError, ErrorBody};
use crate::idempotency::{body_sha256, validate_key};
use crate::state::{sandbox_verified, ApiConfig, ApiState, SharedState, StreamSlot};
use crate::wire::{self, IncomingRequest, SseSource};

/// A started daemon; dropping it stops the accept loop.
pub struct RunningApi {
    addr: SocketAddr,
    accept: Option<JoinHandle<()>>,
    state: SharedState,
}

impl RunningApi {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn base_url(&self) -> String {
        format!("http://{}/v1", self.addr)
    }

    pub fn state(&self) -> &SharedState {
        &self.state
    }

    pub fn stop(&mut self) {
        self.state.mark_shutdown();
        if let Some(handle) = self.accept.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for RunningApi {
    fn drop(&mut self) {
        self.stop();
    }
}

pub struct ApiServer;

impl ApiServer {
    /// Bind loopback, bootstrap state, and serve until stopped.
    pub fn start(config: ApiConfig) -> Result<RunningApi, ApiError> {
        if !config.bind.ip().is_loopback() {
            return Err(ApiError::invalid_request(
                "remote API exposure requires TLS and is not supported in this build; bind a loopback address",
            ));
        }
        let requested = config.bind;
        let state = ApiState::bootstrap(config)?;
        let listener = TcpListener::bind(requested)
            .map_err(|err| ApiError::internal(format!("bind {requested}: {err}")))?;
        listener
            .set_nonblocking(true)
            .map_err(|err| ApiError::internal(format!("listener mode: {err}")))?;
        let addr = listener
            .local_addr()
            .map_err(|err| ApiError::internal(format!("local address: {err}")))?;
        *state.bound.lock().expect("bound address") = Some(addr);

        let accept_state = Arc::clone(&state);
        let accept = std::thread::spawn(move || {
            while !accept_state.is_shutdown() {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let state = Arc::clone(&accept_state);
                        std::thread::spawn(move || serve_connection(stream, state));
                    }
                    Err(err)
                        if err.kind() == std::io::ErrorKind::WouldBlock
                            || err.kind() == std::io::ErrorKind::Interrupted =>
                    {
                        std::thread::sleep(Duration::from_millis(25));
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(RunningApi {
            addr,
            accept: Some(accept),
            state,
        })
    }
}

enum Route {
    Health,
    Capabilities,
    CreateSession,
    GetSession(SessionId),
    CreateRun(SessionId),
    GetRun(RunId),
    CancelRun(RunId),
    Events(SessionId),
    GetApproval(ApprovalId),
    DecideApproval(ApprovalId),
    ListArtifacts(RunId),
    GetArtifact(ArtifactId),
    GetPolicy(SessionId),
}

impl Route {
    fn capability(&self) -> Capability {
        match self {
            Route::Health
            | Route::Capabilities
            | Route::GetSession(_)
            | Route::GetRun(_)
            | Route::Events(_)
            | Route::ListArtifacts(_)
            | Route::GetArtifact(_)
            | Route::GetPolicy(_) => Capability::Read,
            Route::CreateSession | Route::CreateRun(_) | Route::CancelRun(_) => Capability::Run,
            Route::GetApproval(_) | Route::DecideApproval(_) => Capability::Approve,
        }
    }
}

enum Reply {
    Body {
        status: u16,
        content_type: String,
        body: Vec<u8>,
        headers: Vec<(String, String)>,
    },
    Sse(Box<dyn SseSource>),
}

fn json<T: Serialize>(status: u16, value: &T) -> Reply {
    Reply::Body {
        status,
        content_type: "application/json".to_string(),
        body: serde_json::to_vec(value).expect("DTO serializes"),
        headers: Vec::new(),
    }
}

fn raw_json(status: u16, body: String) -> Reply {
    Reply::Body {
        status,
        content_type: "application/json".to_string(),
        body: body.into_bytes(),
        headers: Vec::new(),
    }
}

fn error_reply(error: &ApiError, request_id: &str) -> Reply {
    let mut headers = Vec::new();
    if error.status == 401 {
        headers.push(("WWW-Authenticate".to_string(), "Bearer".to_string()));
    }
    if let Some(retry_after) = error.retry_after {
        headers.push(("Retry-After".to_string(), retry_after.to_string()));
    }
    Reply::Body {
        status: error.status,
        content_type: "application/json".to_string(),
        body: ErrorBody::render(error, request_id).into_bytes(),
        headers,
    }
}

fn split_url(url: &str) -> (&str, &str) {
    match url.split_once('?') {
        Some((path, query)) => (path, query),
        None => (url, ""),
    }
}

fn route_of(method: &str, path: &str) -> Option<Route> {
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let is_get = method == "GET";
    let is_post = method == "POST";
    match segments.as_slice() {
        ["v1", "health"] if is_get => Some(Route::Health),
        ["v1", "capabilities"] if is_get => Some(Route::Capabilities),
        ["v1", "sessions"] if is_post => Some(Route::CreateSession),
        ["v1", "sessions", id] if is_get => SessionId::parse(*id).ok().map(Route::GetSession),
        ["v1", "sessions", id, "runs"] if is_post => {
            SessionId::parse(*id).ok().map(Route::CreateRun)
        }
        ["v1", "sessions", id, "events"] if is_get => SessionId::parse(*id).ok().map(Route::Events),
        ["v1", "sessions", id, "policy"] if is_get => {
            SessionId::parse(*id).ok().map(Route::GetPolicy)
        }
        ["v1", "runs", id] if is_get => RunId::parse(*id).ok().map(Route::GetRun),
        ["v1", "runs", id, "cancel"] if is_post => RunId::parse(*id).ok().map(Route::CancelRun),
        ["v1", "runs", id, "artifacts"] if is_get => {
            RunId::parse(*id).ok().map(Route::ListArtifacts)
        }
        ["v1", "approvals", id] if is_get => ApprovalId::parse(*id).ok().map(Route::GetApproval),
        ["v1", "approvals", id, "decision"] if is_post => {
            ApprovalId::parse(*id).ok().map(Route::DecideApproval)
        }
        ["v1", "artifacts", id] if is_get => ArtifactId::parse(*id).ok().map(Route::GetArtifact),
        _ => None,
    }
}

fn serve_connection(mut stream: TcpStream, state: SharedState) {
    let request_id = request_id();
    let request = match wire::read_request(&mut stream, state.config.limits.max_body_bytes) {
        Ok(request) => request,
        Err(error) => {
            let message = error.message();
            let _ = respond(
                &mut stream,
                error_reply(&ApiError::invalid_request(message), &request_id),
            );
            return;
        }
    };
    let reply = handle_request(&request, &state, &request_id);
    let _ = respond(&mut stream, reply);
}

fn respond(stream: &mut TcpStream, reply: Reply) -> std::io::Result<()> {
    match reply {
        Reply::Body {
            status,
            content_type,
            body,
            headers,
        } => {
            let mut wire_headers = Vec::with_capacity(headers.len() + 1);
            wire_headers.push(("Content-Type".to_string(), content_type));
            wire_headers.extend(headers);
            wire::write_fixed(stream, status, &wire_headers, &body)
        }
        Reply::Sse(mut source) => wire::write_stream(
            stream,
            &[
                ("Content-Type".to_string(), "text/event-stream".to_string()),
                ("Cache-Control".to_string(), "no-store".to_string()),
            ],
            &mut *source,
        ),
    }
}

fn handle_request(request: &IncomingRequest, state: &SharedState, request_id: &str) -> Reply {
    let bound = state
        .bound
        .lock()
        .expect("bound address")
        .unwrap_or(state.config.bind);

    let (path, query) = split_url(&request.target);
    let Some(route) = route_of(&request.method, path) else {
        return error_reply(&ApiError::not_found("unknown route"), request_id);
    };

    // Host/Origin before anything else: a browser cross-origin request never
    // reaches token handling.
    if let Err(error) = auth::validate_host(request.header("Host"), &bound).and_then(|_| {
        auth::validate_origin(request.header("Origin"), &state.config.allowed_origins)
    }) {
        return error_reply(&error, request_id);
    }

    let token = match auth::authenticate(&state.config.tokens, request.header("Authorization")) {
        Ok(token) => token,
        Err(error) => return error_reply(&error, request_id),
    };

    if let Err(error) = auth::require(token, route.capability()) {
        return error_reply(&error, request_id);
    }

    if !matches!(route, Route::Events(_)) {
        if let Err(error) = state.take_rate_slot(&token.id) {
            return error_reply(&error, request_id);
        }
    }

    match dispatch(request, state, &route, query, token) {
        Ok(reply) => reply,
        Err(error) => error_reply(&error, request_id),
    }
}

fn dispatch(
    request: &IncomingRequest,
    state: &SharedState,
    route: &Route,
    query: &str,
    token: &ApiToken,
) -> Result<Reply, ApiError> {
    match route {
        Route::Health => Ok(json(200, &HealthDto::ok())),
        Route::Capabilities => Ok(json(
            200,
            &CapabilitiesDto::current(sandbox_verified(&state.sandbox)),
        )),
        Route::CreateSession => create_session(request, state, token),
        Route::GetSession(id) => get_session(state, id),
        Route::CreateRun(id) => create_run(request, state, token, id),
        Route::GetRun(id) => get_run(state, id),
        Route::CancelRun(id) => cancel_run(state, id),
        Route::Events(id) => stream_events(request, state, id, query),
        Route::GetApproval(id) => get_approval(state, id),
        Route::DecideApproval(id) => decide_approval(request, state, id),
        Route::ListArtifacts(id) => list_artifacts(state, id),
        Route::GetArtifact(id) => get_artifact(state, id),
        Route::GetPolicy(id) => get_policy(state, id),
    }
}

fn store_error(error: StoreError) -> ApiError {
    match error {
        StoreError::NotFound(message) => ApiError::not_found(message),
        other => ApiError::storage(other.to_string()),
    }
}

fn parse_json<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(body)
        .map_err(|err| ApiError::invalid_request(format!("invalid request body: {err}")))
}

fn epoch_now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

fn session_owned(state: &SharedState, session: &SessionId) -> Result<(), ApiError> {
    let record = state.store().session(session).map_err(store_error)?;
    if record.workspace_identity != state.config.workspace_id {
        return Err(ApiError::not_found(format!("session {session}")));
    }
    Ok(())
}

fn create_session(
    request: &IncomingRequest,
    state: &SharedState,
    token: &ApiToken,
) -> Result<Reply, ApiError> {
    let key = request
        .header("Idempotency-Key")
        .ok_or_else(|| ApiError::invalid_request("Idempotency-Key is required"))?
        .to_string();
    validate_key(&key)?;
    let parsed: CreateSessionRequest = parse_json(&request.body)?;
    if let Some(label) = &parsed.label {
        if label.chars().count() > 200 {
            return Err(ApiError::invalid_request(
                "label must be at most 200 characters",
            ));
        }
    }

    let route_key = "/v1/sessions";
    let now = epoch_now();
    let hash = body_sha256(&request.body);
    if let Some(record) = state
        .idempotency()
        .lookup(&token.id, route_key, &key, now)?
    {
        if record.body_sha256 != hash {
            return Err(ApiError::conflict(
                "idempotency_conflict",
                "same Idempotency-Key with a different request body",
            ));
        }
        return Ok(raw_json(record.status, record.response_json));
    }

    let session = state
        .store()
        .create_session(&state.config.workspace_id, parsed.label.as_deref())
        .map_err(store_error)?;
    let record = state.store().session(&session).map_err(store_error)?;
    let body_json =
        serde_json::to_string(&SessionDto::from(&record)).expect("session DTO serializes");
    state
        .idempotency()
        .record(&token.id, route_key, &key, &hash, 201, &body_json, now)?;
    Ok(raw_json(201, body_json))
}

fn get_session(state: &SharedState, session: &SessionId) -> Result<Reply, ApiError> {
    session_owned(state, session)?;
    let record = state.store().session(session).map_err(store_error)?;
    Ok(json(200, &SessionDto::from(&record)))
}

fn create_run(
    request: &IncomingRequest,
    state: &SharedState,
    token: &ApiToken,
    session: &SessionId,
) -> Result<Reply, ApiError> {
    let key = request
        .header("Idempotency-Key")
        .ok_or_else(|| ApiError::invalid_request("Idempotency-Key is required"))?
        .to_string();
    validate_key(&key)?;
    let parsed: CreateRunRequest = parse_json(&request.body)?;
    if parsed.prompt.trim().is_empty() || parsed.prompt.chars().count() > 65536 {
        return Err(ApiError::invalid_request(
            "prompt must be 1–65536 characters",
        ));
    }

    session_owned(state, session)?;
    let route_key = format!("/v1/sessions/{session}/runs");
    let now = epoch_now();
    let hash = body_sha256(&request.body);
    if let Some(record) = state
        .idempotency()
        .lookup(&token.id, &route_key, &key, now)?
    {
        if record.body_sha256 != hash {
            return Err(ApiError::conflict(
                "idempotency_conflict",
                "same Idempotency-Key with a different request body",
            ));
        }
        return Ok(raw_json(record.status, record.response_json));
    }

    if state.active_run(session).is_some() {
        return Err(ApiError::conflict(
            "session_busy",
            "the session already has an active run",
        ));
    }
    let unfinished = state
        .store()
        .interrupted_runs(session)
        .map_err(store_error)?;
    if !unfinished.is_empty() {
        return Err(ApiError::conflict(
            "session_busy",
            "the session has an unfinished run; resume it before starting another",
        ));
    }

    let run = state
        .store()
        .start_run(
            session,
            &state.config.provider,
            &state.config.model,
            state.config.policy.revision,
        )
        .map_err(store_error)?;
    state.set_active(session, &run);

    let backend_run = BackendRun {
        session: session.clone(),
        run: run.clone(),
        prompt: parsed.prompt,
    };
    if let Err(error) = state.config.backend.start(backend_run, &Arc::clone(state)) {
        let mut store = state.store();
        let _ = store.finish_run(
            &run,
            TerminalState::Failed,
            None,
            VerificationStatus::Inconclusive,
        );
        state.clear_active(session);
        return Err(error);
    }

    let detail = state.store().run(&run).map_err(store_error)?;
    let body_json = serde_json::to_string(&RunDto::from(&detail)).expect("run DTO serializes");
    state
        .idempotency()
        .record(&token.id, &route_key, &key, &hash, 202, &body_json, now)?;
    Ok(raw_json(202, body_json))
}

fn run_owned(state: &SharedState, run: &RunId) -> Result<RunDetail, ApiError> {
    let detail = state.store().run(run).map_err(store_error)?;
    session_owned(state, &detail.session_id)?;
    Ok(detail)
}

fn get_run(state: &SharedState, run: &RunId) -> Result<Reply, ApiError> {
    let detail = run_owned(state, run)?;
    Ok(json(200, &RunDto::from(&detail)))
}

fn cancel_run(state: &SharedState, run: &RunId) -> Result<Reply, ApiError> {
    let detail = run_owned(state, run)?;
    let current = run_state(&detail.state);
    if current.is_terminal() {
        return Ok(json(
            202,
            &CancelResultDto {
                run_id: run.to_string(),
                state: current,
            },
        ));
    }
    let resulting = state.config.backend.cancel(run, &Arc::clone(state))?;
    if let Some(terminal) = resulting.as_terminal() {
        let mut store = state.store();
        let _ = store.finish_run(run, terminal, None, VerificationStatus::Inconclusive);
    }
    state.clear_active(&detail.session_id);
    Ok(json(
        202,
        &CancelResultDto {
            run_id: run.to_string(),
            state: resulting,
        },
    ))
}

fn stream_events(
    request: &IncomingRequest,
    state: &SharedState,
    session: &SessionId,
    query: &str,
) -> Result<Reply, ApiError> {
    session_owned(state, session)?;
    let last_seq = state.store().last_seq(session).map_err(store_error)?;

    let params: std::collections::HashMap<&str, &str> = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .filter_map(|pair| pair.split_once('='))
        .collect();
    let after_param = params.get("after").copied();
    let last_event_id = request.header("Last-Event-ID");
    if let (Some(after), Some(last)) = (after_param, last_event_id) {
        if after != last {
            return Err(ApiError::invalid_cursor(
                "after and Last-Event-ID disagree; supply one cursor",
            ));
        }
    }
    let cursor = after_param.or(last_event_id);
    let after = match cursor {
        Some(value) => value.parse::<u64>().map_err(|_| {
            ApiError::invalid_cursor("cursor must be a non-negative integer session seq")
        })?,
        None => 0,
    };
    if after > last_seq {
        return Err(ApiError::invalid_cursor(
            "cursor is ahead of the session event log",
        ));
    }
    let replay = state.store().replay(session, after).map_err(store_error)?;

    let slot = state.enter_stream()?;
    let receiver = state.bus().subscribe();
    Ok(Reply::Sse(Box::new(SseStream {
        receiver,
        replay: replay.into(),
        last_sent: after,
        state: Arc::clone(state),
        _slot: slot,
    })))
}

fn get_approval(state: &SharedState, approval: &ApprovalId) -> Result<Reply, ApiError> {
    let receipt = state.approvals().get(approval).cloned();
    let Some(receipt) = receipt else {
        return Err(ApiError::not_found(format!("approval {approval}")));
    };
    Ok(json(
        200,
        &ApprovalDto::from_receipt(
            &receipt,
            &state.config.workspace_scope,
            state.config.policy.sandbox,
            time::OffsetDateTime::now_utc(),
        ),
    ))
}

fn decide_approval(
    request: &IncomingRequest,
    state: &SharedState,
    approval: &ApprovalId,
) -> Result<Reply, ApiError> {
    let parsed: ApprovalDecisionRequest = parse_json(&request.body)?;
    if let Some(note) = &parsed.note {
        if note.chars().count() > 2000 {
            return Err(ApiError::invalid_request(
                "note must be at most 2000 characters",
            ));
        }
    }

    let receipt = state.approvals().get(approval).cloned();
    let Some(receipt) = receipt else {
        return Err(ApiError::not_found(format!("approval {approval}")));
    };
    if receipt.consumed_at.is_some() {
        return Err(ApiError::conflict(
            "approval_consumed",
            "this approval was already consumed",
        ));
    }
    if receipt.state != ApprovalState::Pending {
        return Err(ApiError::conflict(
            "approval_consumed",
            "this approval was already resolved",
        ));
    }
    if receipt.intent_hash != parsed.intent_hash {
        return Err(ApiError::conflict(
            "approval_stale",
            "intent hash no longer matches the approved action",
        ));
    }
    if receipt.policy_revision != parsed.policy_revision {
        return Err(ApiError::conflict(
            "approval_stale",
            "policy revision changed since this approval was requested",
        ));
    }

    let decision = match parsed.decision {
        crate::dto::Decision::Approve => ApprovalState::Approved,
        crate::dto::Decision::Deny => ApprovalState::Denied,
    };
    let outcome = state.approvals().resolve(
        approval,
        decision,
        parsed.note.clone(),
        time::OffsetDateTime::now_utc(),
    );
    match outcome {
        bollo_policy::approval::ResolveOutcome::Approved
        | bollo_policy::approval::ResolveOutcome::Denied => Ok(json(
            200,
            &ApprovalResultDto {
                approval_id: approval.to_string(),
                decision: parsed.decision,
            },
        )),
        bollo_policy::approval::ResolveOutcome::Expired => Err(ApiError::gone(
            "approval_expired",
            "this approval expired before the decision arrived",
        )),
        bollo_policy::approval::ResolveOutcome::Conflict => Err(ApiError::conflict(
            "approval_consumed",
            "another decision already resolved this approval",
        )),
        bollo_policy::approval::ResolveOutcome::NotFound => {
            Err(ApiError::not_found(format!("approval {approval}")))
        }
    }
}

fn list_artifacts(state: &SharedState, run: &RunId) -> Result<Reply, ApiError> {
    run_owned(state, run)?;
    let artifacts = state.store().artifacts_for_run(run).map_err(store_error)?;
    let dtos: Vec<ArtifactDto> = artifacts
        .iter()
        .map(|artifact| ArtifactDto::from_ref(run.as_str(), artifact))
        .collect();
    Ok(json(200, &dtos))
}

fn get_artifact(state: &SharedState, artifact: &ArtifactId) -> Result<Reply, ApiError> {
    let record = state.store().artifact(artifact).map_err(store_error)?;
    if !record.media_type.starts_with("text/") {
        return Err(ApiError::not_found(format!("artifact {artifact}")));
    }
    let directory = state.config.state_directory.join("artifacts");
    let bytes = state
        .store()
        .read_artifact(&directory, artifact)
        .map_err(store_error)?;
    Ok(Reply::Body {
        status: 200,
        content_type: format!("{}; charset=utf-8", record.media_type),
        body: bytes,
        headers: Vec::new(),
    })
}

fn get_policy(state: &SharedState, session: &SessionId) -> Result<Reply, ApiError> {
    session_owned(state, session)?;
    Ok(json(200, &PolicyDto::from(&state.config.policy)))
}

struct SseStream {
    receiver: std::sync::mpsc::Receiver<EventEnvelope>,
    replay: VecDeque<EventEnvelope>,
    last_sent: u64,
    state: SharedState,
    _slot: StreamSlot,
}

fn frame(event: &EventEnvelope, last_sent: &mut u64) -> Vec<u8> {
    *last_sent = event.seq;
    let data = serde_json::to_string(event).unwrap_or_else(|_| "{}".to_string());
    format!("id: {}\ndata: {}\n\n", event.seq, data).into_bytes()
}

impl SseSource for SseStream {
    fn next_frame(&mut self) -> Option<Vec<u8>> {
        loop {
            if let Some(event) = self.replay.pop_front() {
                return Some(frame(&event, &mut self.last_sent));
            }
            if self.state.is_shutdown() {
                return None;
            }
            match self.receiver.recv_timeout(Duration::from_secs(15)) {
                Ok(event) => {
                    if event.seq > self.last_sent {
                        return Some(frame(&event, &mut self.last_sent));
                    }
                }
                Err(RecvTimeoutError::Timeout) => return Some(b": keep-alive\n\n".to_vec()),
                Err(RecvTimeoutError::Disconnected) => return None,
            }
        }
    }
}
