//! macOS notifications via osascript.
//!
//! Uses `osascript -e 'display notification ...'` — the simplest approach
//! that works without requesting notification permissions or entitlements.
//!
//! Notifications can be disabled globally via [`set_enabled`], controlled
//! by the `[ui] notifications` config option.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

const APP_TITLE: &str = "Push to Talk";

static ENABLED: AtomicBool = AtomicBool::new(true);

/// Enable or disable all notifications. Called once at startup from config.
pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

/// Display a macOS notification asynchronously.
pub fn notify(title: &str, message: &str) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }

    let script = format!(
        "display notification \"{}\" with title \"{}\" subtitle \"{}\"",
        escape_applescript(message),
        escape_applescript(APP_TITLE),
        escape_applescript(title),
    );

    let _ = Command::new("osascript")
        .args(["-e", &script])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// Notify on successful transcription.
pub fn notify_success(preview: &str, latency: f64) {
    notify("Pasted", &format!("({latency:.1}s): {preview}"));
}

/// Notify on transcription error.
pub fn notify_error(error: &str) {
    notify("Error", &format!("Transcription failed: {error}"));
}

/// Notify on hallucination rejection.
pub fn notify_hallucination(reason: &str) {
    notify("Rejected", &format!("Hallucination ({reason})"));
}

/// Escape special characters for AppleScript string literals.
fn escape_applescript(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}
