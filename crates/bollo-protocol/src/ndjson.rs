//! NDJSON codec for the headless interface. One compact JSON object plus a
//! newline per event; no ANSI escapes ever.

use std::io::{self, BufRead, Write};

use crate::errors::ProtocolError;
use crate::events::EventEnvelope;

/// Serialize one envelope as a single NDJSON line.
///
/// Rejects payloads containing an ESC byte: stdout is a machine interface and a
/// terminal must never receive escape sequences from it (AT-012).
pub fn write_event<W: Write>(writer: &mut W, event: &EventEnvelope) -> io::Result<()> {
    let line = serde_json::to_string(event)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    if line.contains('\u{1b}') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "refusing to write ANSI escape sequences to NDJSON stdout",
        ));
    }
    writer.write_all(line.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}

/// Read one envelope. Returns `Ok(None)` at EOF; blank lines are skipped.
pub fn read_event<R: BufRead>(reader: &mut R) -> Result<Option<EventEnvelope>, ProtocolError> {
    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .map_err(|err| ProtocolError::InvalidLine(err.to_string()))?;
        if read == 0 {
            return Ok(None);
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            continue;
        }
        let event: EventEnvelope = serde_json::from_str(trimmed)
            .map_err(|err| ProtocolError::InvalidLine(err.to_string()))?;
        event.validate()?;
        return Ok(Some(event));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{AssistantDeltaData, EventEnvelope, EventType};
    use crate::ids::{RunId, SessionId};

    fn sample() -> EventEnvelope {
        let session = SessionId::generate();
        let run = RunId::generate();
        EventEnvelope::new(
            &session,
            Some(&run),
            1,
            EventType::AssistantDelta,
            &AssistantDeltaData { text: "hello".into() },
        )
        .unwrap()
    }

    #[test]
    fn roundtrips_through_ndjson() {
        let event = sample();
        let mut buf = Vec::new();
        write_event(&mut buf, &event).unwrap();
        assert!(buf.ends_with(b"\n"));
        let mut cursor = io::Cursor::new(buf);
        let back = read_event(&mut cursor).unwrap().unwrap();
        assert_eq!(back, event);
        assert!(read_event(&mut cursor).unwrap().is_none());
    }

    #[test]
    fn escape_sequences_are_json_escaped_on_stdout() {
        let session = SessionId::generate();
        let run = RunId::generate();
        let event = EventEnvelope::new(
            &session,
            Some(&run),
            1,
            EventType::AssistantDelta,
            &AssistantDeltaData {
                text: "\u{1b}[31mred".into(),
            },
        )
        .unwrap();
        let mut buf = Vec::new();
        write_event(&mut buf, &event).unwrap();
        // serde escapes control characters, so no raw ESC byte reaches stdout;
        // the guard in write_event is defense-in-depth against regressions.
        assert!(!buf.contains(&0x1b));
        let back = read_event(&mut io::Cursor::new(buf)).unwrap().unwrap();
        assert_eq!(back.data["text"], "\u{1b}[31mred");
    }

    #[test]
    fn rejects_malformed_line() {
        let mut cursor = io::Cursor::new(b"{\"not\":\"an envelope\"}\n".to_vec());
        assert!(read_event(&mut cursor).is_err());
    }
}
