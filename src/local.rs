//! Local (offline) transcription via a warm Parakeet sidecar.
//!
//! Parakeet TDT is a transducer, not an encoder-decoder LLM: it has no
//! autoregressive text decoder, so it cannot invent fluent text from silence
//! or noise — it returns little or nothing instead. That makes it a good fit
//! for dictation, and it runs entirely on-device with no API key.
//!
//! The model takes seconds to load, so we keep a long-lived Python process
//! alive and speak line-delimited JSON to it (see `parakeet_sidecar.py`), and
//! pay the load cost once per app launch instead of once per dictation. The
//! sidecar is started by [`warm_up`] at launch, or lazily on the first
//! dictation, and is respawned if it dies.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use crate::config::LocalConfig;
use crate::transcribe::TranscriptionResult;

/// The sidecar source, embedded so the script always matches the binary.
const SIDECAR_PY: &str = include_str!("parakeet_sidecar.py");

/// Filename the sidecar is written to inside the config directory.
const SIDECAR_FILE: &str = "parakeet_sidecar.py";

static SIDECAR: LazyLock<Mutex<Option<Sidecar>>> = LazyLock::new(|| Mutex::new(None));

struct Sidecar {
    child: Child,
    stdin: ChildStdin,
    /// Protocol lines read off the sidecar's stdout by a reader thread.
    lines: Receiver<String>,
    /// The model this instance was started with — a config change respawns it.
    model: String,
    /// The launcher this instance was started with, for the same reason.
    command: Vec<String>,
}

impl Sidecar {
    fn shutdown(mut self) {
        drop(self.stdin);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Directories holding the tools the sidecar needs, in priority order.
const TOOL_DIRS: [&str; 3] = ["/opt/homebrew/bin", "/usr/local/bin", "~/.local/bin"];

/// Build the `PATH` to hand the sidecar.
///
/// An app launched from Finder inherits launchd's minimal
/// `/usr/bin:/bin:/usr/sbin:/sbin`, which has neither Homebrew nor
/// `~/.local/bin`. The sidecar needs both — `uv` or `python3` to start, and
/// `ffmpeg`, which `parakeet-mlx` shells out to for decoding. Without this the
/// local backend works when launched from a shell and fails from the Dock,
/// which is a miserable thing to debug.
fn child_path() -> String {
    compose_path(&std::env::var("PATH").unwrap_or_default())
}

/// Prepend the tool directories missing from `inherited`, preserving the rest.
///
/// Split from [`child_path`] so it can be tested against both the launchd
/// default and a full shell PATH, rather than whatever the test runner has.
fn compose_path(inherited: &str) -> String {
    let home = dirs::home_dir();

    let mut dirs: Vec<String> = TOOL_DIRS
        .iter()
        .filter_map(|d| match d.strip_prefix("~/") {
            Some(rest) => home.as_ref().map(|h| h.join(rest).display().to_string()),
            None => Some((*d).to_string()),
        })
        // Whatever the environment already provides is left where it is.
        .filter(|d| !inherited.split(':').any(|p| p == d))
        .collect();

    if !inherited.is_empty() {
        dirs.push(inherited.to_string());
    }
    dirs.join(":")
}

/// Write the embedded sidecar script to the config directory and return its path.
fn install_script() -> Result<PathBuf, String> {
    let dir = crate::config::config_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("failed to create config directory: {e}"))?;
    let path = dir.join(SIDECAR_FILE);
    // Rewrite unconditionally: an upgraded binary must not talk to an old script.
    std::fs::write(&path, SIDECAR_PY)
        .map_err(|e| format!("failed to write {}: {e}", path.display()))?;
    Ok(path)
}

fn spawn(cfg: &LocalConfig) -> Result<Sidecar, String> {
    let script = install_script()?;
    spawn_with_script(cfg, &script)
}

/// Start a sidecar from an explicit script path.
///
/// Split out from [`spawn`] so tests can drive the protocol against a stub
/// model without writing into the user's config directory.
fn spawn_with_script(cfg: &LocalConfig, script: &Path) -> Result<Sidecar, String> {
    let (program, args) = cfg
        .command
        .split_first()
        .ok_or_else(|| "transcription.local.command is empty".to_string())?;

    let mut child = Command::new(program)
        .args(args)
        .arg(script)
        .arg(&cfg.model)
        .env("PATH", child_path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to start `{program}`: {e}"))?;

    let stdin = child.stdin.take().ok_or("sidecar stdin unavailable")?;
    let stdout = child.stdout.take().ok_or("sidecar stdout unavailable")?;
    let stderr = child.stderr.take().ok_or("sidecar stderr unavailable")?;

    // Read stdout on a thread so every wait on the sidecar can time out.
    let (tx, lines) = mpsc::channel();
    std::thread::Builder::new()
        .name("parakeet-stdout".into())
        .spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        })
        .map_err(|e| format!("failed to start sidecar reader thread: {e}"))?;

    // Surface Python tracebacks and Hugging Face download progress in the log.
    std::thread::Builder::new()
        .name("parakeet-stderr".into())
        .spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                eprintln!("[parakeet] {line}");
            }
        })
        .ok();

    let mut sidecar = Sidecar {
        child,
        stdin,
        lines,
        model: cfg.model.clone(),
        command: cfg.command.clone(),
    };

    // Wait for the ready line. The first launch downloads the weights, which
    // is why this timeout is generous by default.
    let ready = recv_line(&mut sidecar, cfg.ready_timeout_s)
        .map_err(|e| format!("model did not load: {e}"))?;
    let value: serde_json::Value = serde_json::from_str(&ready)
        .map_err(|e| format!("bad handshake from sidecar: {e} (line: {ready})"))?;
    if value["ready"].as_bool() != Some(true) {
        let reason = value["error"].as_str().unwrap_or("unknown error");
        return Err(reason.to_string());
    }

    eprintln!("[ptt] Local transcription ready — {}", cfg.model);
    Ok(sidecar)
}

/// Read one protocol line, treating sidecar death as an error rather than a hang.
fn recv_line(sidecar: &mut Sidecar, timeout_s: f64) -> Result<String, String> {
    match sidecar.lines.recv_timeout(Duration::from_secs_f64(timeout_s)) {
        Ok(line) => Ok(line),
        Err(RecvTimeoutError::Timeout) => Err(format!("timed out after {timeout_s:.0}s")),
        Err(RecvTimeoutError::Disconnected) => {
            let status = sidecar.child.try_wait().ok().flatten();
            Err(match status {
                Some(s) => format!("sidecar exited ({s}) — see [parakeet] log lines above"),
                None => "sidecar closed its output stream".to_string(),
            })
        }
    }
}

/// Return a running sidecar for `cfg`, starting one if needed.
///
/// A sidecar started with a different model or launcher is replaced, so
/// editing the config and reloading takes effect without a restart.
fn ensure_started<'a>(
    slot: &'a mut Option<Sidecar>,
    cfg: &LocalConfig,
) -> Result<&'a mut Sidecar, String> {
    if let Some(existing) = slot.as_ref() {
        if existing.model != cfg.model || existing.command != cfg.command {
            if let Some(old) = slot.take() {
                old.shutdown();
            }
        }
    }
    if slot.is_none() {
        *slot = Some(spawn(cfg)?);
    }
    Ok(slot.as_mut().expect("sidecar just started"))
}

/// Why a request failed, and whether the sidecar survived it.
#[derive(Debug)]
enum RequestError {
    /// The sidecar answered, but could not transcribe this recording. It is
    /// still healthy and holding the loaded model, so keep it.
    Rejected(String),
    /// The pipe or the protocol broke. The sidecar can no longer be trusted
    /// and must be replaced before the next request.
    Broken(String),
}

/// One request/response round trip. Errors leave the sidecar untouched; the
/// caller decides whether to discard it based on the [`RequestError`] variant.
fn request(
    sidecar: &mut Sidecar,
    audio_path: &Path,
    timeout_s: f64,
) -> Result<String, RequestError> {
    let path = audio_path.to_str().ok_or_else(|| {
        RequestError::Rejected(format!(
            "audio path is not valid UTF-8: {}",
            audio_path.display()
        ))
    })?;
    let line = serde_json::json!({ "audio": path }).to_string();

    // A write failure means the sidecar is gone (typically a broken pipe).
    writeln!(sidecar.stdin, "{line}")
        .and_then(|()| sidecar.stdin.flush())
        .map_err(|e| RequestError::Broken(format!("failed to send request: {e}")))?;

    let response = recv_line(sidecar, timeout_s).map_err(RequestError::Broken)?;
    // An unparseable line means the protocol has desynchronised; a later
    // response could be read as the answer to the wrong request.
    let value: serde_json::Value = serde_json::from_str(&response).map_err(|e| {
        RequestError::Broken(format!("bad response from sidecar: {e} (line: {response})"))
    })?;

    if value["ok"].as_bool() == Some(true) {
        Ok(value["text"].as_str().unwrap_or("").trim().to_string())
    } else {
        Err(RequestError::Rejected(
            value["error"]
                .as_str()
                .unwrap_or("unknown sidecar error")
                .to_string(),
        ))
    }
}

/// Transcribe `audio_path` locally.
///
/// Retries once through a fresh sidecar, but only for transport failures: the
/// common one is a process that died between dictations (crash, OOM, a stray
/// `pkill python`), which surfaces as a broken pipe on the very next request
/// and is fixed by respawning. A failure the model itself reported is returned
/// as-is — replacing a healthy sidecar would reload the model for nothing.
pub fn transcribe(cfg: &LocalConfig, audio_path: &Path) -> Result<TranscriptionResult, String> {
    let start = Instant::now();
    let mut slot = SIDECAR
        .lock()
        .map_err(|_| "local transcription state is poisoned".to_string())?;

    let mut last_error = String::new();
    for attempt in 0..2 {
        // A failed spawn leaves nothing to reuse, and retrying it would just
        // pay the same timeout twice.
        let sidecar = ensure_started(&mut slot, cfg)?;

        match request(sidecar, audio_path, cfg.request_timeout_s) {
            Ok(text) => return Ok(crate::transcribe::build_result(text, start, audio_path)),
            Err(RequestError::Rejected(e)) => return Err(e),
            Err(RequestError::Broken(e)) => {
                last_error = e;
                if let Some(dead) = slot.take() {
                    dead.shutdown();
                }
                if attempt == 0 {
                    eprintln!(
                        "[ptt] Local sidecar is not responding ({last_error}) — restarting it"
                    );
                }
            }
        }
    }

    Err(last_error)
}

/// Stop the sidecar and release the model, if one is running.
///
/// Called when the user switches to the API backend: a loaded Parakeet holds
/// real memory, and leaving it resident after it stops being used is rude.
pub fn shutdown() {
    let Ok(mut slot) = SIDECAR.lock() else { return };
    if let Some(sidecar) = slot.take() {
        sidecar.shutdown();
        eprintln!("[ptt] Local transcription stopped — model released");
    }
}

/// Start the sidecar ahead of the first dictation, on a background thread.
///
/// Without this the first recording after launch waits out the model load
/// (and, on a cold machine, the weight download).
pub fn warm_up(cfg: &LocalConfig) {
    let cfg = cfg.clone();
    std::thread::Builder::new()
        .name("parakeet-warmup".into())
        .spawn(move || {
            let Ok(mut slot) = SIDECAR.lock() else { return };
            if let Err(e) = ensure_started(&mut slot, &cfg) {
                eprintln!("[ptt] Local transcription unavailable: {e}");
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for `parakeet_mlx` so the protocol can be exercised without
    /// downloading 2.5 GB of weights.
    const STUB_MODULE: &str = r#"
class _Result:
    def __init__(self, text): self.text = text

class _Model:
    def transcribe(self, path):
        if "silence" in path: return _Result("   ")
        if "boom" in path: raise RuntimeError("decode failed")
        return _Result("  hello from the stub  ")

def from_pretrained(model_id):
    return _Model()
"#;

    /// A stub whose model load fails, standing in for a missing dependency
    /// or a bad model id.
    const BROKEN_MODULE: &str = r#"
def from_pretrained(model_id):
    raise OSError("no such model: " + model_id)
"#;

    fn python_available() -> bool {
        Command::new("python3")
            .arg("-c")
            .arg("")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Lay out a throwaway directory holding the sidecar script and a stub
    /// module, and return a config whose launcher puts the stub on PYTHONPATH.
    fn stub_env(name: &str, module: &str) -> (PathBuf, LocalConfig) {
        let dir = std::env::temp_dir().join(format!("ptt-local-test-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("parakeet_mlx.py"), module).unwrap();
        std::fs::write(dir.join(SIDECAR_FILE), SIDECAR_PY).unwrap();

        let cfg = LocalConfig {
            command: vec![
                "env".into(),
                format!("PYTHONPATH={}", dir.display()),
                "python3".into(),
            ],
            model: "stub/model".into(),
            ready_timeout_s: 30.0,
            request_timeout_s: 30.0,
            warm_up: false,
        };
        (dir, cfg)
    }

    #[test]
    fn round_trips_requests_through_a_stub_model() {
        if !python_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let (dir, cfg) = stub_env("ok", STUB_MODULE);
        let mut sidecar = spawn_with_script(&cfg, &dir.join(SIDECAR_FILE)).unwrap();

        // Transcripts come back trimmed.
        assert_eq!(
            request(&mut sidecar, Path::new("/tmp/speech.wav"), 30.0).unwrap(),
            "hello from the stub"
        );
        // Silence yields empty text, not invented speech — and not an error.
        assert_eq!(
            request(&mut sidecar, Path::new("/tmp/silence.wav"), 30.0).unwrap(),
            ""
        );
        // A per-request failure is reported as Rejected, so the caller keeps
        // the sidecar and its loaded model...
        let Err(RequestError::Rejected(err)) =
            request(&mut sidecar, Path::new("/tmp/boom.wav"), 30.0)
        else {
            panic!("a model-reported failure must not be treated as a broken sidecar");
        };
        assert!(err.contains("decode failed"), "unexpected error: {err}");
        // ...so the next request still works.
        assert_eq!(
            request(&mut sidecar, Path::new("/tmp/speech.wav"), 30.0).unwrap(),
            "hello from the stub"
        );

        sidecar.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reports_why_the_model_could_not_load() {
        if !python_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let (dir, cfg) = stub_env("broken", BROKEN_MODULE);
        let Err(err) = spawn_with_script(&cfg, &dir.join(SIDECAR_FILE)) else {
            panic!("a model that fails to load must not report ready");
        };
        assert!(
            err.contains("no such model: stub/model"),
            "unexpected error: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn surfaces_a_dead_sidecar_instead_of_hanging() {
        if !python_available() {
            eprintln!("skipping: python3 not available");
            return;
        }
        let (dir, cfg) = stub_env("dead", STUB_MODULE);
        let mut sidecar = spawn_with_script(&cfg, &dir.join(SIDECAR_FILE)).unwrap();

        let _ = sidecar.child.kill();
        let _ = sidecar.child.wait();

        // Whether this trips on the write or on the closed stdout depends on
        // timing; either way it must come back as Broken rather than block
        // forever, so the caller respawns.
        let Err(RequestError::Broken(err)) =
            request(&mut sidecar, Path::new("/tmp/speech.wav"), 30.0)
        else {
            panic!("a dead sidecar must be reported as broken");
        };
        assert!(!err.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn adds_the_tool_directories_a_gui_launch_lacks() {
        // What an app launched from Finder actually gets.
        let launchd = "/usr/bin:/bin:/usr/sbin:/sbin";
        let path = compose_path(launchd);

        assert!(
            path.starts_with("/opt/homebrew/bin:/usr/local/bin:"),
            "Homebrew must be searched first: {path}"
        );
        assert!(path.contains("/.local/bin"), "missing ~/.local/bin: {path}");
        assert!(
            path.ends_with(launchd),
            "the inherited PATH must be preserved: {path}"
        );
    }

    #[test]
    fn does_not_duplicate_directories_a_shell_already_provides() {
        let shell = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin";
        let path = compose_path(shell);

        assert_eq!(
            path.matches("/opt/homebrew/bin").count(),
            1,
            "Homebrew listed twice: {path}"
        );
        assert!(path.ends_with(shell), "inherited PATH was reordered: {path}");
    }

    #[test]
    fn survives_an_empty_inherited_path() {
        // No trailing separator, and still usable.
        let path = compose_path("");
        assert!(!path.is_empty());
        assert!(!path.ends_with(':'), "trailing separator: {path}");
        assert!(path.starts_with("/opt/homebrew/bin"), "{path}");
    }

    #[test]
    fn rejects_an_empty_launcher() {
        let cfg = LocalConfig {
            command: vec![],
            ..LocalConfig::default()
        };
        let Err(err) = spawn_with_script(&cfg, Path::new("/nonexistent.py")) else {
            panic!("an empty launcher must not start a sidecar");
        };
        assert!(err.contains("command is empty"), "unexpected error: {err}");
    }
}
