//! Bearer-token authentication, capability checks, and Host/Origin validation.
//!
//! Tokens are opaque secrets compared in constant time via SHA-256 digests, so
//! neither content nor length leaks through comparison timing. Secrets are
//! never formatted: `Debug` redacts them.

use std::collections::BTreeSet;
use std::net::SocketAddr;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::ApiError;

/// Capabilities a token can carry. `Approve` also implies viewing approvals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Capability {
    Read,
    Run,
    Approve,
}

/// One locally issued token bound to the daemon workspace.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiToken {
    pub id: String,
    pub secret: String,
    pub capabilities: BTreeSet<Capability>,
}

impl std::fmt::Debug for ApiToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApiToken")
            .field("id", &self.id)
            .field("secret", &"<redacted>")
            .field("capabilities", &self.capabilities)
            .finish()
    }
}

impl ApiToken {
    pub fn new(
        id: impl Into<String>,
        secret: impl Into<String>,
        capabilities: impl IntoIterator<Item = Capability>,
    ) -> Self {
        Self {
            id: id.into(),
            secret: secret.into(),
            capabilities: capabilities.into_iter().collect(),
        }
    }

    /// Generate a fresh id/secret pair for `bollo api token` (P2 CLI wiring).
    pub fn generate(
        id: impl Into<String>,
        capabilities: impl IntoIterator<Item = Capability>,
    ) -> Self {
        let secret = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        Self::new(id, secret, capabilities)
    }

    pub fn has(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
    }
}

/// Constant-time comparison through fixed-size digests.
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let left = Sha256::digest(a.as_bytes());
    let right = Sha256::digest(b.as_bytes());
    let mut difference = 0u8;
    for (x, y) in left.iter().zip(right.iter()) {
        difference |= x ^ y;
    }
    difference == 0
}

/// Resolve the `Authorization: Bearer …` header to exactly one known token.
/// Every token is compared so a wrong secret does not reveal token order.
pub fn authenticate<'a>(
    tokens: &'a [ApiToken],
    header: Option<&str>,
) -> Result<&'a ApiToken, ApiError> {
    let Some(header) = header else {
        return Err(ApiError::unauthenticated("missing Authorization header"));
    };
    let Some(secret) = header.strip_prefix("Bearer ") else {
        return Err(ApiError::unauthenticated(
            "Authorization must use the Bearer scheme",
        ));
    };
    if secret.is_empty() {
        return Err(ApiError::unauthenticated("empty bearer secret"));
    }
    let mut found = None;
    for token in tokens {
        if constant_time_eq(secret, &token.secret) {
            found = Some(token);
        }
    }
    found.ok_or_else(|| ApiError::unauthenticated("unknown bearer token"))
}

pub fn require(token: &ApiToken, capability: Capability) -> Result<(), ApiError> {
    if token.has(capability) {
        Ok(())
    } else {
        Err(ApiError::forbidden(format!(
            "token {} lacks the {capability:?} capability",
            token.id
        )))
    }
}

fn split_host_port(value: &str) -> (&str, Option<u16>) {
    if let Some(rest) = value.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            let host = &rest[..end];
            let port = rest[end + 1..]
                .strip_prefix(':')
                .and_then(|port| port.parse::<u16>().ok());
            return (host, port);
        }
    }
    match value.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => (host, port.parse::<u16>().ok()),
        _ => (value, None),
    }
}

/// Loopback-only Host validation: the name must be a loopback host and, when a
/// port is present, the port this daemon actually bound.
pub fn validate_host(host_header: Option<&str>, bound: &SocketAddr) -> Result<(), ApiError> {
    let Some(value) = host_header else {
        return Err(ApiError::invalid_request("missing Host header"));
    };
    let (host, port) = split_host_port(value.trim());
    let host = host.to_ascii_lowercase();
    let loopback_name = host == "localhost"
        || host == "127.0.0.1"
        || host == "::1"
        || host == "0.0.0.0"
        || host == bound.ip().to_string();
    if !loopback_name {
        return Err(ApiError::origin_denied(format!(
            "Host {value:?} is not the loopback daemon address"
        )));
    }
    if let Some(port) = port {
        if port != bound.port() {
            return Err(ApiError::origin_denied(format!(
                "Host {value:?} does not match the daemon port"
            )));
        }
    }
    Ok(())
}

/// A browser Origin must be explicitly allowlisted; a missing Origin (non-
/// browser client) is fine. No wildcard and no cookie authorization exist.
pub fn validate_origin(origin: Option<&str>, allowed_origins: &[String]) -> Result<(), ApiError> {
    let Some(origin) = origin else {
        return Ok(());
    };
    if allowed_origins.iter().any(|allowed| allowed == origin) {
        Ok(())
    } else {
        Err(ApiError::origin_denied(format!(
            "Origin {origin:?} is not allowlisted for this daemon"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn addr() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7777)
    }

    #[test]
    fn tokens_authenticate_and_redact() {
        let token = ApiToken::new("tok_1", "secret-value", [Capability::Read]);
        let tokens = [token.clone()];
        let found = authenticate(&tokens, Some("Bearer secret-value")).unwrap();
        assert_eq!(found.id, "tok_1");
        assert!(format!("{token:?}").contains("<redacted>"));
        assert!(format!("{token:?}").find("secret-value").is_none());
    }

    #[test]
    fn wrong_scheme_and_unknown_secret_are_unauthenticated() {
        let token = ApiToken::new("tok_1", "secret-value", [Capability::Read]);
        assert!(authenticate(&[token.clone()], None).is_err());
        assert!(authenticate(&[token.clone()], Some("Basic abc")).is_err());
        assert!(authenticate(&[token], Some("Bearer other")).is_err());
    }

    #[test]
    fn capabilities_are_enforced() {
        let token = ApiToken::new("tok_1", "s", [Capability::Read]);
        assert!(require(&token, Capability::Read).is_ok());
        let denied = require(&token, Capability::Run).unwrap_err();
        assert_eq!(denied.status, 403);
        assert_eq!(denied.code, "forbidden");
    }

    #[test]
    fn host_validation_accepts_loopback_and_rejects_foreign() {
        assert!(validate_host(Some("127.0.0.1:7777"), &addr()).is_ok());
        assert!(validate_host(Some("localhost:7777"), &addr()).is_ok());
        assert!(validate_host(Some("[::1]:7777"), &addr()).is_ok());
        assert!(validate_host(Some("127.0.0.1"), &addr()).is_ok());
        assert!(validate_host(Some("evil.example:7777"), &addr()).is_err());
        assert!(validate_host(Some("127.0.0.1:9999"), &addr()).is_err());
        assert!(validate_host(None, &addr()).is_err());
    }

    #[test]
    fn origin_validation_is_exact_and_defaults_to_deny() {
        let allowed = vec!["http://127.0.0.1:5173".to_string()];
        assert!(validate_origin(None, &allowed).is_ok());
        assert!(validate_origin(Some("http://127.0.0.1:5173"), &allowed).is_ok());
        assert!(validate_origin(Some("http://evil.example"), &allowed).is_err());
        assert!(validate_origin(Some("http://127.0.0.1:5173"), &[]).is_err());
    }

    #[test]
    fn constant_time_comparison_is_exact() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "abcd"));
    }
}
