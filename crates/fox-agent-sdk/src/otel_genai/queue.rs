//! Offline disk queue for failed OTLP exports (retry / backfill).
//!
//! On an HTTP failure the whole `ExportTraceServiceRequest` body is appended
//! as one JSON line to `{queue_dir}/{session_id}.jsonl`; the next successful
//! flush replays every queued body verbatim before sending new spans, then
//! clears the queue files. Best-effort and bounded by the caller's config.

use std::fs;
use std::io::Write;
use std::path::Path;

use serde_json::Value;

/// Append one export request body for later retry.
pub fn append_request(dir: &Path, session_id: &str, body: &Value) {
    if let Err(e) = fs::create_dir_all(dir) {
        tracing::warn!(error = %e, dir = %dir.display(), "otel queue: create dir failed");
        return;
    }
    let file = dir.join(format!("{session_id}.jsonl"));
    let Ok(line) = serde_json::to_string(body) else {
        tracing::warn!("otel queue: failed to serialize export body");
        return;
    };
    let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&file) else {
        tracing::warn!(file = %file.display(), "otel queue: open failed");
        return;
    };
    let _ = writeln!(f, "{line}");
}

/// Read every queued export body across all `.jsonl` files in `dir`.
pub fn read_requests(dir: &Path) -> Vec<Value> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        for line in content.lines() {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                out.push(v);
            }
        }
    }
    out
}

/// Delete all queue files (called after a successful replay).
pub fn clear(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_append_read_clear() {
        let dir = std::env::temp_dir().join(format!("otel-queue-test-{}", uuid::Uuid::new_v4()));
        let body = serde_json::json!({ "resourceSpans": [] });
        append_request(&dir, "sess-1", &body);
        append_request(&dir, "sess-1", &body);
        append_request(&dir, "sess-2", &serde_json::json!({ "other": true }));

        let all = read_requests(&dir);
        assert_eq!(all.len(), 3);

        clear(&dir);
        assert!(read_requests(&dir).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
