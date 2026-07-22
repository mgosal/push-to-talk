//! Shared session log — append-only JSONL file for the interaction surface.
//!
//! Both Push to Talk and Better Clipboard write to the same log at
//! `~/.local/share/5et/session.jsonl`. Each line is a JSON object with
//! a timestamp, event type, and payload.
//!
//! This is the "tap" protocol: passive, ordered capture of interaction
//! events that form a session context.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::SystemTime;

/// Return the session log path, creating the directory if needed.
fn log_path() -> PathBuf {
    let dir = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("5et");
    let _ = fs::create_dir_all(&dir);
    dir.join("session.jsonl")
}

/// ISO 8601 timestamp from SystemTime, without pulling in chrono.
fn now_iso() -> String {
    let d = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs();
    // Format as seconds since epoch — consumers parse this.
    // Full ISO 8601 would need chrono; epoch seconds are unambiguous.
    secs.to_string()
}

/// Append a voice transcription event to the session log.
pub fn log_voice(text: &str, duration_s: f64, latency_s: f64, wps: f64) {
    let t = now_iso();
    let escaped = text.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n");
    let line = format!(
        r#"{{"t":{},"type":"voice","text":"{}","duration_s":{:.1},"latency_s":{:.1},"wps":{:.1}}}"#,
        t, escaped, duration_s, latency_s, wps
    );
    append(&line);
}

/// Append a raw line to the session log.
fn append(line: &str) {
    let path = log_path();
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{}", line);
    }
}
