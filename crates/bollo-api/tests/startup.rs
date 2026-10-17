//! Startup-window regression tests (follow-up to the observed one-off
//! `connection: reset` on the very first request after `bollo serve` bound).
//!
//! The contract: from the moment `ApiServer::start` returns, the port is
//! *ready* — every connection must be answered with a parseable HTTP response
//! carrying a documented status. It must never observe a connection reset,
//! a truncated read, or a hang, including when several connections race the
//! first moments of the accept loop.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bollo_api::backend::UnavailableBackend;
use bollo_api::auth::{ApiToken, Capability};
use bollo_api::state::{ApiConfig, ApiLimits, PolicySnapshot, ProvenanceView, RuleView};
use bollo_api::ApiServer;

fn policy() -> PolicySnapshot {
    PolicySnapshot {
        revision: 1,
        profile: bollo_protocol::vocab::Profile::Balanced,
        sandbox: bollo_protocol::vocab::SandboxMode::Off,
        isolation_verified: false,
        rules: vec![RuleView {
            id: "startup-test".into(),
            effect: bollo_protocol::vocab::Effect::Deny,
            tool: "*".into(),
            path_glob: Some("**/.env".into()),
            argv_prefix: None,
        }],
        provenance: vec![ProvenanceView {
            rule_id: "startup-test".into(),
            source: "startup-test".into(),
            layer: "test".into(),
        }],
    }
}

fn config(i: u32, dir: PathBuf, port: u16) -> ApiConfig {
    ApiConfig {
        bind: SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::LOCALHOST), port),
        workspace_id: format!("startup-{i}"),
        workspace_scope: "startup test workspace".into(),
        state_directory: dir,
        store_path: None,
        provider: "replay".into(),
        model: "startup-test-model".into(),
        policy: policy(),
        tokens: vec![ApiToken::new(
            "startup",
            format!("startup-secret-{i}"),
            vec![
                Capability::Read,
                Capability::Run,
                Capability::Approve,
            ],
        )],
        allowed_origins: Vec::new(),
        limits: ApiLimits::default(),
        backend: Arc::new(UnavailableBackend),
    }
}

/// One full unauthenticated HTTP round trip. Returns `(status, got_http_head)`
/// where a connection reset/truncation surfaces as an error.
fn probe(addr: SocketAddr) -> std::io::Result<(u16, bool)> {
    let mut stream = TcpStream::connect(addr)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    write!(
        stream,
        "GET /v1/health HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        addr
    )?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    let text = String::from_utf8_lossy(&raw);
    let head = text.lines().next().unwrap_or("").to_string();
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| std::io::Error::other("no status line received"))?;
    Ok((status, head.starts_with("HTTP/1.")))
}

#[test]
fn every_connection_in_the_startup_window_is_answered() {
    // Enough ratios of run to catch a flaky window without dominating CI time.
    for iteration in 0..15u32 {
        let dir =
            tempfile::Builder::new().prefix("startup-race").tempdir().expect("temp dir");
        let mut api = match ApiServer::start(config(iteration, dir.path().to_path_buf(), 0)) {
            Ok(api) => api,
            Err(err) => panic!("iteration {iteration}: daemon did not start: {err}"),
        };

        // Phase A — literally the first connection after start() returns.
        // Sequential and immediate: no settle delay is allowed to exist.
        let (status, valid_head) =
            probe(api.addr()).unwrap_or_else(|err| panic!("iteration {iteration}: the first connection after start() failed: {err}"));
        let ok = status == 401 || status == 200;
        assert!(valid_head && ok, "iteration {iteration}: first connection answered HTTP {status} (head valid={valid_head})");

        // Phase B — a concurrent burst racing the accept loop's first polls.
        let addr = api.addr();
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let addr = addr;
                std::thread::spawn(move || probe(addr))
            })
            .collect();
        for (index, handle) in handles.into_iter().enumerate() {
            let outcome = handle
                .join()
                .unwrap_or_else(|panic| panic!("iteration {iteration}: burst thread {index} panicked: {panic:?}"));
            let (status, valid_head) = outcome
                .unwrap_or_else(|err| panic!("iteration {iteration}: burst connection {index} failed: {err}"));
            let ok = status == 401 || status == 200;
            assert!(valid_head && ok, "iteration {iteration}: burst connection {index} answered HTTP {status} (head valid={valid_head})");
        }

        api.stop();
    }
}
