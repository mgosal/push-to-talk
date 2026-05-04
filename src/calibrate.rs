//! Speaker calibration and profile generation.
//!
//! Three features:
//! 1. Onboarding — generate a speaker profile from user-provided vocabulary/context
//! 2. Calibration — generate test sentences, record, transcribe, analyse errors
//! 3. Correction learning — mine the DB for correction patterns → update profile

use std::path::{Path, PathBuf};

use crate::config;

// ── Profile Generation (Onboarding) ──────────────────────────────────

const PROFILE_GENERATION_PROMPT: &str = r#"You are building a speaker profile for a voice transcription system.
The user will provide vocabulary, writing preferences, project context, or
free-form notes. Generate a speaker profile in markdown that will help a
transcription model produce accurate output.

Do not infer or include sensitive personal attributes such as age, gender,
ethnicity, nationality, precise location, health, religion, or politics. Include
accent or dialect notes only when the user explicitly provides them and they are
useful for transcription.

The profile MUST include these sections:

## Transcription Context
- Language, optional accent/dialect notes, register, and any explicit context
  that helps transcription accuracy

## Recording Environment
- Device, setting, noise level, speaker count, language

## Domain Context
- Tools and platforms mentioned, domain vocabulary,
  proper nouns and acronyms that may appear, recurring themes

## Pronunciation Tendencies
- Leave this section with a placeholder:
  "*Run voice calibration to populate this section.*"

## Speaking Style
- Filler words, self-corrections, profanity, pacing, formality

## Spelling Preferences
- British vs American English, specific word preferences

Output ONLY the markdown profile. No preamble, no explanation.
Start with `# Speaker Profile`."#;

/// Generate a speaker profile from user-provided context by calling the
/// configured LLM endpoint.
pub fn generate_profile(
    endpoint: &str,
    model: &str,
    api_key: &str,
    background_text: &str,
) -> Result<String, String> {
    let body = serde_json::json!({
        "model": model.replace("audio-preview", "mini"),
        "messages": [
            {"role": "system", "content": PROFILE_GENERATION_PROMPT},
            {"role": "user", "content": background_text}
        ],
        "max_tokens": 3000,
        "temperature": 0.3,
    });

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    let resp = client
        .post(endpoint)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .map_err(|e| format!("Request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        return Err(format!("API error {status}: {text}"));
    }

    let json: serde_json::Value = resp.json().map_err(|e| format!("JSON parse error: {e}"))?;
    json["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "No content in API response".to_string())
}

/// Save the generated profile to the config directory.
pub fn save_profile(profile_text: &str) -> Result<PathBuf, String> {
    let dir = config::config_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("speaker-profile.md");
    std::fs::write(&path, profile_text)
        .map_err(|e| format!("Failed to write profile: {e}"))?;
    Ok(path)
}

/// Check if a speaker profile exists.
pub fn profile_exists() -> bool {
    let dir = config::config_dir();
    dir.join("speaker-profile.md").exists()
}

/// Read the current speaker profile, if any.
pub fn read_profile() -> Option<String> {
    let path = config::config_dir().join("speaker-profile.md");
    std::fs::read_to_string(path).ok()
}

// ── Calibration Sentence Generation ──────────────────────────────────

const CALIBRATION_PROMPT: &str = r#"You are generating calibration sentences for a voice transcription system.
Based on the speaker profile below, create exactly 20 sentences that the user
will read aloud. These sentences are designed to exercise specific pronunciation
patterns and domain vocabulary so the system can learn the speaker's voice.

REQUIREMENTS:
- Group A (sentences 1-4): Use the speaker's domain terminology in natural sentences
- Group B (sentences 5-8): Technical tool names and acronyms from their work
- Group C (sentences 9-12): Unusual or domain-specific vocabulary they use
- Group D (sentences 13-15): Sentences with self-corrections and clause restarts
- Group E (sentences 16-18): Numbers, plurals, and designations
- Group F (sentences 19-20): Informal register with colloquialisms

Each sentence should be 10-25 words. Write them as the speaker would naturally say them.

Output format — exactly like this, no other text:

**01.** The first sentence here.
**02.** The second sentence here.
...
**20.** The twentieth sentence here."#;

/// Calibration sentence with its group and number.
#[derive(Debug, Clone)]
pub struct CalibrationSentence {
    pub number: u32,
    pub group: &'static str,
    pub text: String,
}

/// Generate personalised calibration sentences from the speaker profile.
pub fn generate_calibration_sentences(
    endpoint: &str,
    model: &str,
    api_key: &str,
    profile: &str,
) -> Result<Vec<CalibrationSentence>, String> {
    let body = serde_json::json!({
        "model": model.replace("audio-preview", "mini"),
        "messages": [
            {"role": "system", "content": CALIBRATION_PROMPT},
            {"role": "user", "content": profile}
        ],
        "max_tokens": 2000,
        "temperature": 0.5,
    });

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    let resp = client
        .post(endpoint)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .map_err(|e| format!("Request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        return Err(format!("API error {status}: {text}"));
    }

    let json: serde_json::Value = resp.json().map_err(|e| format!("JSON parse error: {e}"))?;
    let content = json["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| "No content in API response".to_string())?;

    parse_calibration_sentences(content)
}

fn parse_calibration_sentences(text: &str) -> Result<Vec<CalibrationSentence>, String> {
    let re = regex::Regex::new(r"\*\*(\d+)\.\*\*\s+(.+)")
        .map_err(|e| format!("Regex error: {e}"))?;

    let mut sentences = Vec::new();
    for cap in re.captures_iter(text) {
        let num: u32 = cap[1].parse().unwrap_or(0);
        let sent = cap[2].trim().to_string();
        let group = match num {
            1..=4 => "A: domain terminology",
            5..=8 => "B: technical tool names",
            9..=12 => "C: unusual vocabulary",
            13..=15 => "D: self-corrections",
            16..=18 => "E: numbers/designations",
            19..=20 => "F: informal register",
            _ => "unknown",
        };
        sentences.push(CalibrationSentence { number: num, group, text: sent });
    }

    if sentences.is_empty() {
        return Err("No calibration sentences parsed from response".to_string());
    }
    Ok(sentences)
}

/// Save calibration sentences to a file for reference.
pub fn save_calibration_script(sentences: &[CalibrationSentence]) -> Result<PathBuf, String> {
    let dir = config::config_dir().join("calibration");
    let _ = std::fs::create_dir_all(&dir);

    let path = dir.join("sentences.md");
    let mut content = String::from("# Calibration Sentences\n\nRead each sentence naturally.\n\n");
    let mut current_group = "";
    for s in sentences {
        if s.group != current_group {
            current_group = s.group;
            content.push_str(&format!("\n### Group {current_group}\n\n"));
        }
        content.push_str(&format!("**{:02}.** {}\n\n", s.number, s.text));
    }
    std::fs::write(&path, &content)
        .map_err(|e| format!("Failed to write calibration script: {e}"))?;
    Ok(path)
}

// ── Calibration Results ──────────────────────────────────────────────

/// Result of comparing a single calibration recording against ground truth.
#[derive(Debug, Clone)]
pub struct CalibrationResult {
    pub sentence_num: u32,
    pub group: String,
    pub reference: String,
    pub hypothesis: String,
    /// Word-level differences: (expected, got)
    pub substitutions: Vec<(String, String)>,
}

/// Compare a transcription against the expected sentence.
/// Returns word-level substitution pairs.
pub fn compare_transcription(reference: &str, hypothesis: &str) -> Vec<(String, String)> {
    let ref_norm = normalise(reference);
    let hyp_norm = normalise(hypothesis);
    let ref_words: Vec<&str> = ref_norm.split_whitespace().collect();
    let hyp_words: Vec<&str> = hyp_norm.split_whitespace().collect();

    // Simple word-level diff (not full edit distance, but sufficient for pattern detection)
    let mut subs = Vec::new();
    let min_len = ref_words.len().min(hyp_words.len());
    for i in 0..min_len {
        if ref_words[i] != hyp_words[i] {
            subs.push((ref_words[i].to_string(), hyp_words[i].to_string()));
        }
    }
    subs
}

fn normalise(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

// ── Correction Learning ──────────────────────────────────────────────

/// A correction pair from the database.
#[derive(Debug)]
pub struct CorrectionPair {
    pub original: String,
    pub corrected: String,
}

/// Query the database for all correction pairs.
pub fn get_correction_pairs(db_path: &Path) -> Vec<CorrectionPair> {
    let conn = match rusqlite::Connection::open(db_path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };

    let mut stmt = match conn.prepare(
        "SELECT transcript, corrected FROM dictations
         WHERE corrected IS NOT NULL AND corrected != ''
         AND transcript IS NOT NULL AND transcript != corrected
         ORDER BY id DESC LIMIT 100"
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };

    stmt.query_map([], |row| {
        Ok(CorrectionPair {
            original: row.get(0)?,
            corrected: row.get(1)?,
        })
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

/// Analyse correction pairs and generate profile update suggestions
/// by calling the LLM to identify patterns.
pub fn analyse_corrections(
    endpoint: &str,
    model: &str,
    api_key: &str,
    pairs: &[CorrectionPair],
    current_profile: &str,
) -> Result<String, String> {
    if pairs.is_empty() {
        return Err("No corrections to analyse".to_string());
    }

    let mut examples = String::new();
    for (i, pair) in pairs.iter().enumerate().take(50) {
        examples.push_str(&format!(
            "#{}: ORIGINAL: {}\n    CORRECTED: {}\n\n",
            i + 1, pair.original, pair.corrected
        ));
    }

    let prompt = [
        "You are analysing user corrections to voice transcriptions to identify",
        "systematic error patterns. Below are pairs of (original transcription, user-corrected version).",
        "",
        "Identify patterns in what the transcription model gets wrong for this speaker:",
        "- Words consistently misheard (e.g., \"code\" → \"cold\")",
        "- Technical terms mangled",
        "- Accent-related substitutions",
        "- Consistent spelling preferences",
        "",
        "Then produce a \"## Pronunciation Tendencies\" section and a \"## Correction Patterns\"",
        "section that can be appended to the speaker profile below.",
        "",
        "Only include patterns with 2+ occurrences or where the error is clearly accent/domain-related.",
        "Do NOT repeat information already in the profile.",
        "",
        "CURRENT PROFILE:",
    ].join("\n") + "\n" + current_profile + "\n\nCORRECTION PAIRS:\n" + &examples
        + "\nOutput ONLY the new markdown sections to append. No preamble.";

    let body = serde_json::json!({
        "model": model.replace("audio-preview", "mini"),
        "messages": [
            {"role": "user", "content": prompt}
        ],
        "max_tokens": 2000,
        "temperature": 0.2,
    });

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    let resp = client
        .post(endpoint)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .map_err(|e| format!("Request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        return Err(format!("API error {status}: {text}"));
    }

    let json: serde_json::Value = resp.json().map_err(|e| format!("JSON parse error: {e}"))?;
    json["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "No content in API response".to_string())
}

/// Append new sections to the existing speaker profile.
pub fn append_to_profile(new_sections: &str) -> Result<(), String> {
    let path = config::config_dir().join("speaker-profile.md");
    let mut content = std::fs::read_to_string(&path)
        .unwrap_or_default();

    content.push_str("\n\n---\n\n");
    content.push_str(new_sections);

    std::fs::write(&path, content)
        .map_err(|e| format!("Failed to update profile: {e}"))?;
    Ok(())
}

// ── Calibration State (for multi-step recording flow) ────────────────

use std::sync::Mutex;

/// Shared calibration state for the recording flow.
pub struct CalibrationState {
    pub sentences: Vec<CalibrationSentence>,
    pub current_index: usize,
    pub results: Vec<CalibrationResult>,
    pub active: bool,
}

impl CalibrationState {
    pub fn new() -> Self {
        Self {
            sentences: Vec::new(),
            current_index: 0,
            results: Vec::new(),
            active: false,
        }
    }

    pub fn current_sentence(&self) -> Option<&CalibrationSentence> {
        self.sentences.get(self.current_index)
    }

    pub fn advance(&mut self) -> bool {
        self.current_index += 1;
        self.current_index < self.sentences.len()
    }

    pub fn is_complete(&self) -> bool {
        self.current_index >= self.sentences.len()
    }

    pub fn progress_text(&self) -> String {
        format!(
            "Sentence {}/{}: {}",
            self.current_index + 1,
            self.sentences.len(),
            self.sentences.get(self.current_index)
                .map(|s| s.text.as_str())
                .unwrap_or("(done)")
        )
    }
}

pub static CALIBRATION: std::sync::LazyLock<Mutex<CalibrationState>> =
    std::sync::LazyLock::new(|| Mutex::new(CalibrationState::new()));

/// Generate a summary report of calibration results and update the profile.
pub fn finalise_calibration(
    endpoint: &str,
    model: &str,
    api_key: &str,
) -> Result<String, String> {
    let state = CALIBRATION.lock().map_err(|e| format!("Lock error: {e}"))?;
    if state.results.is_empty() {
        return Err("No calibration results".to_string());
    }

    // Build a summary of all errors
    let mut error_summary = String::new();
    for r in &state.results {
        if !r.substitutions.is_empty() {
            error_summary.push_str(&format!(
                "Sentence {:02} [{}]: REF=\"{}\" HYP=\"{}\"\n  Errors: {}\n\n",
                r.sentence_num,
                r.group,
                r.reference,
                r.hypothesis,
                r.substitutions.iter()
                    .map(|(a, b)| format!("'{a}' → '{b}'"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }

    if error_summary.is_empty() {
        return Ok("No errors detected — transcription is already accurate for your voice.".to_string());
    }

    // Read current profile
    let profile = read_profile().unwrap_or_default();

    let prompt = [
        "You are analysing voice calibration results. The speaker recorded",
        "scripted sentences and the transcription model made the following errors.",
        "",
        "Identify systematic pronunciation patterns and generate a",
        "\"## Pronunciation Tendencies\" section for the speaker profile.",
        "",
        "Categorise errors into:",
        "- Final consonant dropping",
        "- Technical term misrecognition",
        "- Word boundary fusion",
        "- Accent-specific substitutions",
        "- Other patterns",
        "",
        "Only include confirmed patterns (errors that clearly show a systematic issue).",
        "",
        "CURRENT PROFILE:",
    ].join("\n") + "\n" + &profile + "\n\nCALIBRATION ERRORS:\n" + &error_summary
        + "\nOutput ONLY the markdown section to add to the profile. No preamble.";

    let body = serde_json::json!({
        "model": model.replace("audio-preview", "mini"),
        "messages": [
            {"role": "user", "content": prompt}
        ],
        "max_tokens": 2000,
        "temperature": 0.2,
    });

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    let resp = client
        .post(endpoint)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .map_err(|e| format!("Request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        return Err(format!("API error {status}: {text}"));
    }

    let json: serde_json::Value = resp.json().map_err(|e| format!("JSON parse error: {e}"))?;
    let new_section = json["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| "No content in API response".to_string())?;

    // Append to profile
    append_to_profile(new_section)?;

    let n_errors: usize = state.results.iter().map(|r| r.substitutions.len()).sum();
    let n_sentences = state.results.len();
    Ok(format!(
        "Calibration complete: {n_sentences} sentences, {n_errors} errors detected.\nProfile updated with pronunciation patterns."
    ))
}
