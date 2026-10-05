//! Minimal HTTP/1.1 wire handling for the loopback daemon.
//!
//! One request per connection; `Content-Length` request bodies; responses are
//! either fixed-length or an EOF-delimited SSE stream (`Connection: close`)
//! whose frames are flushed individually.
//!
//! The daemon deliberately does not sit on a buffered server layer: a 1 KiB
//! write buffer would hold small SSE frames until the next larger write, which
//! is exactly the latency SSE exists to avoid. Keeping the wire layer small
//! and local makes every flush explicit.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

pub(crate) const MAX_HEAD_BYTES: usize = 64 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub(crate) struct IncomingRequest {
    pub method: String,
    pub target: String,
    /// Lowercased names, trimmed values.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl IncomingRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Debug)]
pub(crate) enum WireError {
    TooLarge,
    Malformed(String),
    Io(String),
}

impl WireError {
    pub fn message(&self) -> String {
        match self {
            WireError::TooLarge => "request body exceeds the configured cap".to_string(),
            WireError::Malformed(message) => format!("malformed HTTP request: {message}"),
            WireError::Io(message) => format!("connection error: {message}"),
        }
    }
}

fn read_line(reader: &mut impl BufRead, buffer: &mut Vec<u8>) -> std::io::Result<usize> {
    buffer.clear();
    reader.read_until(b'\n', buffer)
}

/// Read one request head plus its bounded body.
pub(crate) fn read_request(
    stream: &mut TcpStream,
    body_limit: usize,
) -> Result<IncomingRequest, WireError> {
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .map_err(|err| WireError::Io(err.to_string()))?;
    // A clone shares the socket; reads buffer on the clone while responses can
    // still be written on the original handle (Expect: 100-continue).
    let read_stream = stream
        .try_clone()
        .map_err(|err| WireError::Io(err.to_string()))?;
    let mut reader = BufReader::new(read_stream);

    let mut line = Vec::new();
    if read_line(&mut reader, &mut line).map_err(|err| WireError::Io(err.to_string()))? == 0 {
        return Err(WireError::Malformed("empty request".into()));
    }
    let request_line = String::from_utf8_lossy(&line).trim().to_string();
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| WireError::Malformed("missing method".into()))?
        .to_string();
    let target = parts
        .next()
        .ok_or_else(|| WireError::Malformed("missing request target".into()))?
        .to_string();
    let version = parts.next().unwrap_or("HTTP/1.1");
    if !version.starts_with("HTTP/1.") {
        return Err(WireError::Malformed(format!(
            "unsupported version {version:?}"
        )));
    }

    let mut headers = Vec::new();
    let mut head_bytes = request_line.len();
    loop {
        let count =
            read_line(&mut reader, &mut line).map_err(|err| WireError::Io(err.to_string()))?;
        if count == 0 {
            break;
        }
        head_bytes += count;
        if head_bytes > MAX_HEAD_BYTES {
            return Err(WireError::Malformed("request head too large".into()));
        }
        let text = String::from_utf8_lossy(&line);
        let trimmed = text.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        let Some((name, value)) = trimmed.split_once(':') else {
            return Err(WireError::Malformed("bad header line".into()));
        };
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }

    if headers.iter().any(|(name, value)| {
        name == "transfer-encoding" && value.to_ascii_lowercase().contains("chunked")
    }) {
        return Err(WireError::Malformed(
            "chunked request bodies are not supported".into(),
        ));
    }
    let content_length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    if content_length > body_limit {
        return Err(WireError::TooLarge);
    }
    if headers.iter().any(|(name, value)| {
        name == "expect" && value.to_ascii_lowercase().contains("100-continue")
    }) {
        stream
            .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
            .and_then(|()| stream.flush())
            .map_err(|err| WireError::Io(err.to_string()))?;
    }
    let mut body = vec![0u8; content_length];
    reader
        .read_exact(&mut body)
        .map_err(|err| WireError::Io(err.to_string()))?;

    Ok(IncomingRequest {
        method,
        target,
        headers,
        body,
    })
}

/// A source of pre-encoded SSE frames. `None` ends the stream.
pub(crate) trait SseSource: Send {
    fn next_frame(&mut self) -> Option<Vec<u8>>;
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        410 => "Gone",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "Status",
    }
}

fn write_head(
    writer: &mut impl Write,
    status: u16,
    headers: &[(String, String)],
) -> std::io::Result<()> {
    write!(writer, "HTTP/1.1 {} {}\r\n", status, reason(status))?;
    writer.write_all(b"Connection: close\r\n")?;
    for (name, value) in headers {
        write!(writer, "{name}: {value}\r\n")?;
    }
    Ok(())
}

pub(crate) fn write_fixed(
    writer: &mut impl Write,
    status: u16,
    headers: &[(String, String)],
    body: &[u8],
) -> std::io::Result<()> {
    write_head(writer, status, headers)?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(body)?;
    writer.flush()
}

/// Write an EOF-delimited event stream, flushing after every frame.
pub(crate) fn write_stream(
    writer: &mut impl Write,
    headers: &[(String, String)],
    source: &mut dyn SseSource,
) -> std::io::Result<()> {
    write_head(writer, 200, headers)?;
    writer.write_all(b"\r\n")?;
    // Opening comment: clients see the stream as established immediately.
    writer.write_all(b": connected\n\n")?;
    writer.flush()?;
    while let Some(frame) = source.next_frame() {
        if frame.is_empty() {
            continue;
        }
        writer.write_all(&frame)?;
        writer.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons_cover_documented_statuses() {
        assert_eq!(reason(200), "OK");
        assert_eq!(reason(409), "Conflict");
        assert_eq!(reason(429), "Too Many Requests");
    }
}
