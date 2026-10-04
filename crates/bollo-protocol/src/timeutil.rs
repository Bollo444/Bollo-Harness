//! RFC3339 UTC timestamp helpers. Timestamps are observation times, never the
//! ordering authority (`seq` is).

use time::{format_description::well_known::Rfc3339, Duration, OffsetDateTime};

use crate::errors::ProtocolError;

pub fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .expect("RFC3339 formatting of the current UTC time cannot fail")
}

pub fn parse_rfc3339(value: &str) -> Result<OffsetDateTime, ProtocolError> {
    OffsetDateTime::parse(value, &Rfc3339)
        .map_err(|err| ProtocolError::InvalidEvent(format!("timestamp {value:?}: {err}")))
}

pub fn add_seconds(from: OffsetDateTime, seconds: i64) -> OffsetDateTime {
    from + Duration::seconds(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_parses_back() {
        let now = now_rfc3339();
        let parsed = parse_rfc3339(&now).unwrap();
        assert_eq!(parsed.offset(), time::UtcOffset::UTC);
    }

    #[test]
    fn rejects_non_rfc3339() {
        assert!(parse_rfc3339("yesterday").is_err());
        assert!(parse_rfc3339("2026-10-03").is_err());
    }

    #[test]
    fn adds_seconds() {
        let base = parse_rfc3339("2026-10-03T00:00:00Z").unwrap();
        let later = add_seconds(base, 300);
        assert_eq!(later.unix_timestamp() - base.unix_timestamp(), 300);
    }
}
