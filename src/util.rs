use sha2::{Digest, Sha256};

use crate::ir::{Event, TraceMeta};

pub fn sha256_hex(data: &str) -> String {
    let mut h = Sha256::new();
    h.update(data.as_bytes());
    format!("{:x}", h.finalize())
}

pub fn sha256_file(path: &std::path::Path) -> Result<String, String> {
    let data = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut h = Sha256::new();
    h.update(&data);
    Ok(format!("{:x}", h.finalize()))
}

/// Parse an RFC3339 timestamp into epoch millis.
pub fn parse_ts(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.timestamp_millis())
        .ok()
}

/// Fill in trace meta derived from the extracted events and canonicalize hashes.
pub fn finish_meta(meta: &mut TraceMeta, events: &[Event]) {
    meta.event_count = events.len();
    let canonical = serde_json::to_string(events).unwrap_or_default();
    meta.events_sha256 = sha256_hex(&canonical);
}

/// Format epoch millis as an RFC3339-ish string for listings.
pub fn format_ms(ms: i64) -> String {
    let dt = chrono::DateTime::from_timestamp_millis(ms);
    dt.map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| ms.to_string())
}

/// Format epoch millis in the local timezone for human-facing listings.
pub fn format_local_ms(ms: i64) -> String {
    use chrono::TimeZone;

    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|d| d.format("%Y-%m-%d %H:%M %:z").to_string())
        .unwrap_or_else(|| ms.to_string())
}
