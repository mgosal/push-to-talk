//! Configuration management.
//!
//! Config lives at `~/.config/push-to-talk/config.toml`.
//! All fields have sensible defaults — a bare API key is the only requirement.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Top-level configuration.
#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub api: ApiConfig,
    pub audio: AudioConfig,
    pub transcription: TranscriptionConfig,
    pub storage: StorageConfig,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct ApiConfig {
    /// OpenAI-compatible chat completions endpoint.
    pub endpoint: String,
    /// Model identifier.
    pub model: String,
    /// API key (inline). Takes precedence over key_file.
    pub key: Option<String>,
    /// Path to a file containing the API key (one line).
    /// Relative paths are resolved from the config directory.
    pub key_file: Option<String>,
    /// Maximum retries on 5xx / connection errors.
    pub max_retries: u32,
    /// Backoff delays in seconds between retries.
    pub retry_backoff: Vec<f64>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct AudioConfig {
    /// Minimum recording duration in seconds. Shorter recordings are discarded.
    pub min_duration_s: f64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct TranscriptionConfig {
    /// Path to a speaker profile file for transcription priming.
    pub speaker_profile: Option<String>,
    /// Maximum words-per-second threshold. Above this → hallucination.
    pub max_wps: f64,
    /// Directory for saved transcript markdown files.
    pub transcripts_dir: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct StorageConfig {
    /// Path to the SQLite database. Default: <config_dir>/history.db
    pub db: Option<String>,
    /// Directory for temporary audio recordings. Default: system temp.
    pub audio_dir: Option<String>,
    /// Path to the PID file. Default: /tmp/ptt.pid
    pub pid_file: String,
    /// Path to the Unix IPC socket. Default: /tmp/ptt.sock
    pub socket_path: String,
}

// ── Defaults ──────────────────────────────────────────────────────────

impl Default for Config {
    fn default() -> Self {
        Self {
            api: ApiConfig::default(),
            audio: AudioConfig::default(),
            transcription: TranscriptionConfig::default(),
            storage: StorageConfig::default(),
        }
    }
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            endpoint: Provider::OpenRouter.endpoint().into(),
            model: Provider::OpenRouter.model().into(),
            key: None,
            key_file: Some(Provider::OpenRouter.key_file().into()),
            max_retries: 3,
            retry_backoff: vec![1.0, 3.0, 5.0],
        }
    }
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            min_duration_s: 0.4,
        }
    }
}

impl Default for TranscriptionConfig {
    fn default() -> Self {
        Self {
            speaker_profile: None,
            max_wps: 5.0,
            transcripts_dir: None,
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            db: None,
            audio_dir: None,
            pid_file: "/tmp/ptt.pid".into(),
            socket_path: "/tmp/ptt.sock".into(),
        }
    }
}

// ── Loading ───────────────────────────────────────────────────────────

/// Return the config directory: `~/.config/push-to-talk/`
pub fn config_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("push-to-talk")
}

/// Return the conventional generated speaker profile path.
pub fn default_speaker_profile_path() -> PathBuf {
    config_dir().join("speaker-profile.md")
}

/// Load config from disk, falling back to defaults.
pub fn load() -> Config {
    let dir = config_dir();
    let config_path = dir.join("config.toml");

    let config: Config = if config_path.exists() {
        match std::fs::read_to_string(&config_path) {
            Ok(contents) => match toml::from_str(&contents) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("[ptt] Warning: failed to parse {}: {e}", config_path.display());
                    Config::default()
                }
            },
            Err(e) => {
                eprintln!("[ptt] Warning: failed to read {}: {e}", config_path.display());
                Config::default()
            }
        }
    } else {
        eprintln!("[ptt] No config at {} — using defaults", config_path.display());
        Config::default()
    };

    config
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    OpenAI,
    OpenRouter,
}

impl Provider {
    pub fn from_label(label: &str) -> Self {
        if label.contains("OpenRouter") {
            Self::OpenRouter
        } else {
            Self::OpenAI
        }
    }

    pub fn endpoint(self) -> &'static str {
        match self {
            Self::OpenAI => "https://api.openai.com/v1/chat/completions",
            Self::OpenRouter => "https://openrouter.ai/api/v1/chat/completions",
        }
    }

    pub fn model(self) -> &'static str {
        match self {
            Self::OpenAI => "gpt-4o-transcribe",
            Self::OpenRouter => "openai/gpt-4o-transcribe",
        }
    }

    pub fn key_file(self) -> &'static str {
        "api-key"
    }
}

/// Save an API key and switch the provider settings to a supported endpoint.
pub fn save_api_key(provider: Provider, api_key: &str) -> Result<(), String> {
    let key = api_key.trim();
    if key.is_empty() {
        return Err("API key is empty".into());
    }

    let dir = config_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("failed to create config directory: {e}"))?;

    let key_path = dir.join(provider.key_file());
    std::fs::write(&key_path, format!("{key}\n"))
        .map_err(|e| format!("failed to write API key: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600));
    }

    let mut cfg = load();
    cfg.api.endpoint = provider.endpoint().into();
    cfg.api.model = provider.model().into();
    cfg.api.key = None;
    cfg.api.key_file = Some(provider.key_file().into());

    let config_text = toml::to_string_pretty(&cfg)
        .map_err(|e| format!("failed to serialise config: {e}"))?;
    std::fs::write(dir.join("config.toml"), config_text)
        .map_err(|e| format!("failed to write config: {e}"))?;

    Ok(())
}

/// Map retired or provider-specific aliases to the model used for audio
/// transcription requests. This keeps existing config files working after
/// model deprecations.
pub fn transcription_model(model: &str) -> String {
    match model {
        "gpt-4o-audio-preview" => "gpt-4o-transcribe".into(),
        "openai/gpt-4o-audio-preview" => "openai/gpt-4o-transcribe".into(),
        _ => model.into(),
    }
}

/// Return the endpoint expected by speech-to-text models.
pub fn transcription_endpoint(endpoint: &str) -> String {
    if endpoint.contains("/audio/transcriptions") {
        endpoint.into()
    } else {
        endpoint.replace("/chat/completions", "/audio/transcriptions")
    }
}

/// Whether this model should use the speech-to-text transcription endpoint
/// rather than the chat completions endpoint.
pub fn uses_transcription_endpoint(model: &str) -> bool {
    transcription_model(model).contains("transcribe")
}

/// Pick a text-capable model for profile generation and calibration analysis.
/// STT-only models are not suitable for plain text chat prompts.
pub fn text_model_for(model: &str) -> String {
    match transcription_model(model).as_str() {
        "gpt-4o-transcribe" | "gpt-4o-mini-transcribe" => "gpt-4o-mini".into(),
        "openai/gpt-4o-transcribe" | "openai/gpt-4o-mini-transcribe" => {
            "openai/gpt-4o-mini".into()
        }
        normalized => normalized.replace("audio-preview", "mini"),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        text_model_for, transcription_endpoint, transcription_model, uses_transcription_endpoint,
    };

    #[test]
    fn maps_retired_audio_preview_models() {
        assert_eq!(
            transcription_model("gpt-4o-audio-preview"),
            "gpt-4o-transcribe"
        );
        assert_eq!(
            transcription_model("openai/gpt-4o-audio-preview"),
            "openai/gpt-4o-transcribe"
        );
    }

    #[test]
    fn maps_transcription_models_to_text_models_for_calibration() {
        assert_eq!(text_model_for("gpt-4o-transcribe"), "gpt-4o-mini");
        assert_eq!(
            text_model_for("openai/gpt-4o-transcribe"),
            "openai/gpt-4o-mini"
        );
        assert_eq!(text_model_for("custom/model"), "custom/model");
    }

    #[test]
    fn derives_audio_transcription_endpoints_from_chat_endpoints() {
        assert_eq!(
            transcription_endpoint("https://api.openai.com/v1/chat/completions"),
            "https://api.openai.com/v1/audio/transcriptions"
        );
        assert_eq!(
            transcription_endpoint("https://openrouter.ai/api/v1/chat/completions"),
            "https://openrouter.ai/api/v1/audio/transcriptions"
        );
    }

    #[test]
    fn detects_transcription_endpoint_models() {
        assert!(uses_transcription_endpoint("gpt-4o-transcribe"));
        assert!(uses_transcription_endpoint("openai/gpt-4o-audio-preview"));
        assert!(!uses_transcription_endpoint("gpt-audio"));
    }
}

// ── Resolved accessors ───────────────────────────────────────────────

impl Config {
    /// Resolve the API key from inline value or key_file.
    pub fn api_key(&self) -> Option<String> {
        // Inline key takes precedence
        if let Some(ref k) = self.api.key {
            if !k.is_empty() {
                return Some(k.clone());
            }
        }
        // Try key_file
        if let Some(ref path_str) = self.api.key_file {
            let path = resolve_path(path_str);
            if let Ok(contents) = std::fs::read_to_string(&path) {
                let key = contents.trim().to_string();
                if !key.is_empty() {
                    return Some(key);
                }
            }
        }
        // Try OPENROUTER_API_KEY or OPENAI_API_KEY env vars
        if let Ok(k) = std::env::var("OPENROUTER_API_KEY") {
            return Some(k);
        }
        if let Ok(k) = std::env::var("OPENAI_API_KEY") {
            return Some(k);
        }
        None
    }

    /// Resolve speaker profile content.
    ///
    /// An explicit config path wins. Otherwise, use the generated profile that
    /// the onboarding flow writes to the config directory.
    pub fn speaker_profile(&self) -> String {
        match &self.transcription.speaker_profile {
            Some(path_str) => {
                let path = resolve_path(path_str);
                std::fs::read_to_string(&path).unwrap_or_default()
            }
            None => std::fs::read_to_string(default_speaker_profile_path()).unwrap_or_default(),
        }
    }

    /// Resolve the database path.
    pub fn db_path(&self) -> PathBuf {
        match &self.storage.db {
            Some(p) => resolve_path(p),
            None => config_dir().join("history.db"),
        }
    }

    /// Resolve the audio recording directory.
    pub fn audio_dir(&self) -> PathBuf {
        match &self.storage.audio_dir {
            Some(p) => resolve_path(p),
            None => {
                let dir = std::env::temp_dir().join("ptt");
                let _ = std::fs::create_dir_all(&dir);
                dir
            }
        }
    }

    /// Resolve the transcripts directory, if configured.
    pub fn transcripts_dir(&self) -> Option<PathBuf> {
        self.transcription.transcripts_dir.as_ref().map(|p| {
            let path = resolve_path(p);
            let _ = std::fs::create_dir_all(&path);
            path
        })
    }

    /// Resolve the PID file path.
    pub fn pid_path(&self) -> PathBuf {
        resolve_path(&self.storage.pid_file)
    }

    /// Resolve the IPC socket path.
    pub fn socket_path(&self) -> PathBuf {
        resolve_path(&self.storage.socket_path)
    }

}

/// Resolve a path string: expand `~`, resolve relative to config dir.
fn resolve_path(p: &str) -> PathBuf {
    if p.starts_with('~') {
        if let Some(home) = dirs::home_dir() {
            return home.join(&p[2..]); // skip "~/"
        }
    }
    let path = Path::new(p);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        config_dir().join(p)
    }
}
