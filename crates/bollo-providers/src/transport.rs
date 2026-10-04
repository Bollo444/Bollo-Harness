//! Transport boundary. All adapter tests run against a deterministic fake;
//! the real HTTPS implementation is opt-in via the `live-http` feature and is
//! never enabled implicitly.

use std::io::Read;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub timeout: Duration,
}

pub struct HttpResponse {
    pub status: u16,
    pub body: Box<dyn Read + Send>,
}

impl std::fmt::Debug for HttpResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpResponse")
            .field("status", &self.status)
            .field("body", &"<reader>")
            .finish()
    }
}

pub trait Transport: Send + Sync {
    fn post(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError>;
}

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("network error: {0}")]
    Network(String),
    #[error("connect timeout")]
    Timeout,
}

/// Live HTTPS transport (feature `live-http`). TLS verification is always on.
#[cfg(feature = "live-http")]
pub struct UreqTransport {
    agent: ureq::Agent,
}

#[cfg(feature = "live-http")]
impl UreqTransport {
    pub fn new() -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .build();
        Self { agent }
    }
}

#[cfg(feature = "live-http")]
impl Default for UreqTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "live-http")]
impl Transport for UreqTransport {
    fn post(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
        let mut builder = self.agent.post(&request.url);
        for (key, value) in &request.headers {
            builder = builder.set(key, value);
        }
        match builder.send_bytes(&request.body) {
            Ok(response) => Ok(HttpResponse {
                status: response.status(),
                body: Box::new(response.into_reader()),
            }),
            Err(ureq::Error::Status(status, response)) => Ok(HttpResponse {
                status,
                body: Box::new(response.into_reader()),
            }),
            Err(ureq::Error::Transport(err)) => {
                Err(TransportError::Network(err.to_string()))
            }
        }
    }
}

#[cfg(test)]
pub mod testing {
    use super::*;
    use std::sync::Mutex;

    /// Deterministic transport for adapter tests: replays a canned body and
    /// records the last request for assertions.
    pub struct FakeTransport {
        pub status: u16,
        pub body: Vec<u8>,
        pub last_request: Mutex<Option<HttpRequest>>,
    }

    impl FakeTransport {
        pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
            Self {
                status,
                body: body.into(),
                last_request: Mutex::new(None),
            }
        }

        pub fn last_request(&self) -> Option<HttpRequest> {
            self.last_request.lock().unwrap().clone()
        }
    }

    impl Transport for FakeTransport {
        fn post(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
            *self.last_request.lock().unwrap() = Some(request.clone());
            Ok(HttpResponse {
                status: self.status,
                body: Box::new(std::io::Cursor::new(self.body.clone())),
            })
        }
    }
}
