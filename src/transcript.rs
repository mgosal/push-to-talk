//! Transcript file saving — markdown files with YAML frontmatter.
//!
//! Each successful transcription (and hallucination) is saved as a markdown
//! file alongside the database record. Matches the Python dictate.py format.

use std::path::{Path, PathBuf};

/// Save a transcript to a markdown file with YAML frontmatter.
///
/// Returns the path of the saved file, or None if saving failed.
pub fn save(
    transcripts_dir: &Path,
    audio_path: &Path,
    text: &str,
    model: &str,
    latency_s: f64,
    wps: f64,
    mode: &str,
    is_hallucination: bool,
) -> Option<PathBuf> {
    let _ = std::fs::create_dir_all(transcripts_dir);

    let stem = audio_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("dictation");

    // Timestamp for frontmatter
    let now = timestamp_now();

    let filename = format!("{stem}.md");
    let out_path = transcripts_dir.join(&filename);

    let mut frontmatter = format!(
        "---\n\
         source: {}\n\
         model: {model}\n\
         transcribed: {now}\n\
         latency: {latency_s:.1}s\n\
         mode: {mode}\n",
        audio_path.file_name().and_then(|s| s.to_str()).unwrap_or("unknown"),
    );

    if is_hallucination {
        frontmatter.push_str(&format!("hallucination: true\nwps: {wps:.1}\n"));
    }

    frontmatter.push_str("---\n\n");

    let contents = format!("{frontmatter}{text}\n");

    match std::fs::write(&out_path, contents) {
        Ok(()) => {
            eprintln!("[ptt] Transcript saved: {}", out_path.display());
            Some(out_path)
        }
        Err(e) => {
            eprintln!("[ptt] Failed to save transcript: {e}");
            None
        }
    }
}

fn timestamp_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let (y, mo, d, h, mi, s) = crate::db::time_from_epoch(secs);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}
