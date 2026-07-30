//! EventRecorder: JSONL export and replay of [`EventEnvelope`] streams.
//!
//! Every agent event can be captured, written to a `.jsonl` file, and
//! replayed in order for debugging, post-hoc analysis, or e2e testing.

use fox_agent_core::{AgentEvent, EnvelopePayload, EventEnvelope};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};

/// Records agent events to a JSONL file and/or an in-memory buffer.
///
/// # Usage
///
/// ```ignore
/// let recorder = EventRecorder::new(session_id, turn_id);
/// let output_path = PathBuf::from("events.jsonl");
///
/// // wire into agent event channel
/// let (tx, mut rx) = mpsc::channel(64);
/// tokio::spawn(recorder.clone().run(rx, Some(output_path)));
/// ```
pub struct EventRecorder {
    buffer: Arc<RwLock<Vec<EventEnvelope>>>,
    seq: Arc<RwLock<u64>>,
    session_id: String,
    turn_id: u64,
}

impl EventRecorder {
    /// Create a new recorder for a session + turn.
    pub fn new(session_id: impl Into<String>, turn_id: u64) -> Self {
        Self {
            buffer: Arc::new(RwLock::new(Vec::new())),
            seq: Arc::new(RwLock::new(0)),
            session_id: session_id.into(),
            turn_id,
        }
    }

    /// Create a recorded envelope and return it.
    pub fn record(&self, source: &str, event: EnvelopePayload) -> EventEnvelope {
        let seq = self.seq.blocking_write();
        let envelope = EventEnvelope::new(&self.session_id, self.turn_id, *seq, source, event);
        // Non-blocking push to buffer
        let mut buf = self.buffer.blocking_write();
        buf.push(envelope.clone());
        drop(buf);
        drop(seq);
        // Advance sequence
        {
            let mut s = self.seq.blocking_write();
            *s += 1;
        }
        envelope
    }

    /// Run the recorder loop: read from channel, optionally write to file.
    pub async fn run(
        self: Arc<Self>,
        mut rx: mpsc::Receiver<AgentEvent>,
        output_path: Option<PathBuf>,
    ) {
        let mut file = match output_path {
            Some(p) => {
                let dir = p.parent().unwrap();
                tokio::fs::create_dir_all(dir).await.ok();
                std::fs::File::create(&p).ok()
            }
            None => None,
        };

        while let Some(ev) = rx.recv().await {
            let payload = EnvelopePayload::from(&ev);
            let envelope = self.record("agent", payload);
            if let Some(ref mut f) = file {
                let line = envelope.to_json_line().unwrap_or_default();
                let _ = writeln!(f, "{line}");
            }
        }
    }

    /// Return a snapshot of all recorded envelopes.
    pub async fn buffer(&self) -> Vec<EventEnvelope> {
        self.buffer.read().await.clone()
    }

    /// Export buffer to a JSONL file, with secret scrubbing applied.
    pub async fn export_to_file(&self, path: &PathBuf) -> std::io::Result<()> {
        let buf = self.buffer.read().await;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = std::fs::File::create(path)?;
        for envelope in buf.iter() {
            let line = envelope
                .to_json_line()
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
            // Scrub secrets from exported data
            let safe_line = crate::scrub::mask_event_payload(&line);
            writeln!(f, "{safe_line}")?;
        }
        Ok(())
    }

    /// Load envelopes from a JSONL file for replay.
    pub fn load_from_file(path: &PathBuf) -> std::io::Result<Vec<EventEnvelope>> {
        let f = std::fs::File::open(path)?;
        let reader = BufReader::new(f);
        let mut envelopes = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let envelope: EventEnvelope = serde_json::from_str(&line)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
            envelopes.push(envelope);
        }
        Ok(envelopes)
    }
}
