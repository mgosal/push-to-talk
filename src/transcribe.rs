//! Transcription via any OpenAI-compatible audio API.
//!
//! Includes the detailed system prompt for speaker-primed transcription,
//! and retry logic with configurable backoff for transient failures.

use std::path::Path;

/// Format-based hallucination check: real speech never produces structured output.
/// Returns Some(reason) if the text looks like a hallucination.
pub fn is_format_hallucination(text: &str) -> Option<&'static str> {
    let trimmed = text.trim();

    // Starts with JSON bracket
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return Some("starts with JSON");
    }
    // Starts with code fence
    if trimmed.starts_with("```") {
        return Some("starts with code fence");
    }
    // Starts with HTML/XML tag
    if trimmed.starts_with('<')
        && trimmed.chars().nth(1).map_or(false, |c| c.is_ascii_alphabetic())
    {
        return Some("starts with HTML/XML tag");
    }
    // Starts with shebang
    if trimmed.starts_with("#!") {
        return Some("starts with shebang");
    }
    // Contains JSON key-value syntax ("key": value)
    if text.contains("\":{") || text.contains("\":[") || text.contains("\":\"")
        || text.contains("\": {") || text.contains("\": [") || text.contains("\": \"")
    {
        return Some("contains JSON key-value syntax");
    }
    None
}

/// WPS-based hallucination check: reject transcripts faster than human speech.
pub fn is_wps_hallucination(text: &str, duration_s: f64, max_wps: f64) -> bool {
    if duration_s <= 0.0 {
        return true;
    }
    let words = text.split_whitespace().count() as f64;
    words / duration_s > max_wps
}

/// Combined hallucination check (format + WPS).
pub fn is_hallucination(text: &str, duration_s: f64, max_wps: f64) -> bool {
    is_format_hallucination(text).is_some() || is_wps_hallucination(text, duration_s, max_wps)
}

/// Transcription result.
pub struct TranscriptionResult {
    pub text: String,
    pub latency_s: f64,
    pub wps: f64,
    pub duration_s: f64,
}

/// Build the system prompt for transcription.
fn build_system_prompt(speaker_profile: &str) -> String {
    format!(
        "You are transcribing a personal voice note recorded by a specific individual.\n\
         Use the speaker profile below to produce an accurate transcription.\n\
         \n\
         {speaker_profile}\n\
         \n\
         ---\n\
         \n\
         TASK: Transcribe the audio faithfully.\n\
         - Preserve filler words, profanity, self-corrections exactly as spoken\n\
         - Preserve mid-sentence pauses and clause restarts — do not smooth them out\n\
         - Prefer domain terms when a word sounds like one from this speaker's vocabulary\n\
         - Follow any spelling or language preferences in the speaker profile\n\
         - Output only the transcript. No preamble, no explanation."
    )
}

/// Determine if an error is retryable (transient).
fn is_retryable_error(error: &str, status_code: Option<u16>) -> bool {
    // HTTP 5xx errors
    if let Some(code) = status_code {
        if code >= 500 {
            return true;
        }
    }
    // Connection / timeout errors
    let retryable_patterns = [
        "connection",
        "timeout",
        "timed out",
        "reset by peer",
        "broken pipe",
        "temporarily unavailable",
    ];
    let lower = error.to_lowercase();
    retryable_patterns.iter().any(|p| lower.contains(p))
}

/// Call an OpenAI-compatible audio transcription endpoint with retry logic.
pub fn transcribe(
    endpoint: &str,
    model: &str,
    api_key: &str,
    speaker_profile: &str,
    ffprobe: &str,
    audio_path: &Path,
    max_retries: u32,
    retry_backoff: &[f64],
) -> Result<TranscriptionResult, String> {
    use base64::Engine;

    let start = std::time::Instant::now();

    // Read and base64-encode the audio file
    let audio_bytes = std::fs::read(audio_path)
        .map_err(|e| format!("Failed to read audio: {e}"))?;
    let audio_b64 = base64::engine::general_purpose::STANDARD.encode(&audio_bytes);

    // Determine format from extension
    let ext = audio_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("m4a");

    let system_prompt = build_system_prompt(speaker_profile);

    let body = serde_json::json!({
        "model": model,
        "messages": [
            {
                "role": "system",
                "content": system_prompt
            },
            {
                "role": "user",
                "content": [
                    {
                        "type": "input_audio",
                        "input_audio": {
                            "data": audio_b64,
                            "format": ext
                        }
                    }
                ]
            }
        ],
        "max_completion_tokens": 4096
    });

    let client = reqwest::blocking::Client::new();
    let mut last_error = String::new();

    // Attempt with retries
    for attempt in 0..=max_retries {
        if attempt > 0 {
            let delay_idx = (attempt as usize - 1).min(retry_backoff.len().saturating_sub(1));
            let delay = retry_backoff.get(delay_idx).copied().unwrap_or(5.0);
            eprintln!("[ptt] Retry {attempt}/{max_retries} after {delay:.0}s...");
            std::thread::sleep(std::time::Duration::from_secs_f64(delay));
        }

        let resp = match client
            .post(endpoint)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
        {
            Ok(r) => r,
            Err(e) => {
                last_error = format!("HTTP error: {e}");
                if is_retryable_error(&last_error, None) && attempt < max_retries {
                    continue;
                }
                return Err(last_error);
            }
        };

        let status = resp.status();
        let status_code = status.as_u16();
        let resp_text = resp.text().map_err(|e| format!("Response read error: {e}"))?;

        if !status.is_success() {
            let msg = serde_json::from_str::<serde_json::Value>(&resp_text)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(String::from))
                .unwrap_or_else(|| resp_text.chars().take(200).collect());

            last_error = format!("API {status}: {msg}");

            if is_retryable_error(&last_error, Some(status_code)) && attempt < max_retries {
                continue;
            }
            return Err(last_error);
        }

        // Successful response — parse and return
        let resp_json: serde_json::Value =
            serde_json::from_str(&resp_text).map_err(|e| format!("JSON parse error: {e}"))?;

        let text = resp_json["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string();

        let latency_s = start.elapsed().as_secs_f64();

        // Get audio duration for WPS calc
        let duration_s = crate::audio::get_duration(ffprobe, audio_path).unwrap_or(0.0);
        let word_count = text.split_whitespace().count() as f64;
        let wps = if duration_s > 0.0 { word_count / duration_s } else { 0.0 };

        return Ok(TranscriptionResult {
            text,
            latency_s,
            wps,
            duration_s,
        });
    }

    Err(last_error)
}
