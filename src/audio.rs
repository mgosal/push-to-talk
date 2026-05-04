//! Audio recording (ffmpeg) and system sound playback.

use std::path::Path;
use std::process::{Child, Command, Stdio};

// System sound paths
const TONE_START: &str = "/System/Library/Sounds/Tink.aiff";
const TONE_LOCK: &str = "/System/Library/Sounds/Morse.aiff";
const TONE_DISCARD: &str = "/System/Library/Sounds/Basso.aiff";
const TONE_PROCESSING: &str = "/System/Library/Sounds/Pop.aiff";
const TONE_DONE: &str = "/System/Library/Sounds/Glass.aiff";

pub enum Tone {
    Start,
    Lock,
    Discard,
    Processing,
    Done,
}

/// Play a system sound asynchronously.
pub fn play_tone(tone: Tone) {
    let path = match tone {
        Tone::Start => TONE_START,
        Tone::Lock => TONE_LOCK,
        Tone::Discard => TONE_DISCARD,
        Tone::Processing => TONE_PROCESSING,
        Tone::Done => TONE_DONE,
    };
    let _ = Command::new("afplay")
        .arg(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// Start recording audio to a WAV file. Returns the ffmpeg child process.
pub fn start_recording(ffmpeg: &str, output_path: &Path) -> Option<Child> {
    match Command::new(ffmpeg)
        .args([
            "-f", "avfoundation",
            "-i", ":default",
            "-c:a", "pcm_s16le",
            "-ar", "16000",
            "-ac", "1",
            "-y",
        ])
        .arg(output_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => Some(child),
        Err(e) => {
            eprintln!("[ptt] Failed to start ffmpeg: {e}");
            None
        }
    }
}

/// Trigger macOS microphone permission through the same ffmpeg path used for
/// real recordings, then capture a tiny sample to verify access.
pub fn request_microphone_access(ffmpeg: &str) -> Result<(), String> {
    let output = Command::new(ffmpeg)
        .args([
            "-nostdin",
            "-f", "avfoundation",
            "-i", ":default",
            "-t", "0.2",
            "-f", "null",
            "-",
        ])
        .stdout(Stdio::null())
        .output()
        .map_err(|e| format!("failed to start ffmpeg: {e}"))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("microphone test failed");
    Err(detail.to_string())
}

/// Stop recording by sending SIGTERM to ffmpeg.
pub fn stop_recording(child: &mut Child) {
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let _ = child.wait();
}

/// Get audio duration in seconds using ffprobe.
pub fn get_duration(ffprobe: &str, path: &Path) -> Option<f64> {
    let output = Command::new(ffprobe)
        .args([
            "-v", "error",
            "-show_entries", "format=duration",
            "-of", "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .output()
        .ok()?;

    let s = String::from_utf8_lossy(&output.stdout);
    s.trim().parse().ok()
}
