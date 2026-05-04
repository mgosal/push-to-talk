//! IPC via Unix domain socket + CLI argument handling.
//!
//! The running app listens on a Unix socket (default `/tmp/ptt.sock`).
//! CLI clients can send `toggle` and `status` commands.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Commands received over IPC.
#[derive(Debug, Clone, PartialEq)]
pub enum IpcCommand {
    Toggle,
    Status,
}

/// IPC messages queued for the main thread.
pub struct IpcState {
    pub commands: Vec<IpcCommand>,
}

impl IpcState {
    pub fn new() -> Self {
        Self { commands: Vec::new() }
    }

    pub fn drain_commands(&mut self) -> Vec<IpcCommand> {
        std::mem::take(&mut self.commands)
    }
}

/// Parsed CLI action (determined before app launch).
pub enum CliAction {
    /// Run as the menubar app (default).
    RunApp,
    /// Send --toggle to a running instance.
    Toggle,
    /// Query --status from a running instance.
    Status,
}

/// Parse CLI arguments. Minimal hand-rolled parser — no clap needed for 2 flags.
pub fn parse_args() -> CliAction {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(|s| s.as_str()) {
        Some("--toggle") => CliAction::Toggle,
        Some("--status") => CliAction::Status,
        _ => CliAction::RunApp,
    }
}

/// Write the PID file. Removes it on drop via PidGuard.
pub struct PidGuard {
    path: PathBuf,
}

impl PidGuard {
    pub fn create(path: &Path) -> Option<Self> {
        let pid = std::process::id();
        match std::fs::write(path, pid.to_string()) {
            Ok(()) => {
                eprintln!("[ptt] PID file: {} (pid={})", path.display(), pid);
                Some(PidGuard { path: path.to_path_buf() })
            }
            Err(e) => {
                eprintln!("[ptt] Warning: failed to write PID file: {e}");
                None
            }
        }
    }
}

impl Drop for PidGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Check if a running instance exists by reading the PID file.
pub fn is_running(pid_path: &Path) -> bool {
    if let Ok(contents) = std::fs::read_to_string(pid_path) {
        if let Ok(pid) = contents.trim().parse::<i32>() {
            // Check if process exists (signal 0 = no-op check)
            unsafe { libc::kill(pid, 0) == 0 }
        } else {
            false
        }
    } else {
        false
    }
}

/// Send a command to a running instance via the Unix socket.
/// Returns the response string.
pub fn send_command(socket_path: &Path, command: &str) -> Result<String, String> {
    let mut stream = UnixStream::connect(socket_path)
        .map_err(|e| format!("Cannot connect to running instance: {e}"))?;

    stream.write_all(command.as_bytes())
        .map_err(|e| format!("Write error: {e}"))?;
    stream.write_all(b"\n")
        .map_err(|e| format!("Write error: {e}"))?;
    stream.flush()
        .map_err(|e| format!("Flush error: {e}"))?;

    // Shutdown write half so server knows we're done
    stream.shutdown(std::net::Shutdown::Write)
        .map_err(|e| format!("Shutdown error: {e}"))?;

    let mut response = String::new();
    let mut reader = BufReader::new(&stream);
    reader.read_line(&mut response)
        .map_err(|e| format!("Read error: {e}"))?;

    Ok(response.trim().to_string())
}

/// Represents the status response for --status queries.
pub fn format_status_json(
    state: &str,
    transcriptions: u32,
    total_latency: f64,
    recording: bool,
) -> String {
    let avg = if transcriptions > 0 {
        total_latency / transcriptions as f64
    } else {
        0.0
    };
    serde_json::json!({
        "state": state,
        "stats": {
            "transcriptions": transcriptions,
            "total_latency": total_latency,
            "avg_latency": avg,
        },
        "recording": recording,
    })
    .to_string()
}

/// Start the IPC socket server on a background thread.
/// Returns the shared state handle for the main thread to poll.
pub fn start_server(socket_path: &Path) -> Arc<Mutex<IpcState>> {
    // Remove stale socket
    let _ = std::fs::remove_file(socket_path);

    let listener = match UnixListener::bind(socket_path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[ptt] Failed to bind IPC socket {}: {e}", socket_path.display());
            return Arc::new(Mutex::new(IpcState::new()));
        }
    };

    eprintln!("[ptt] IPC socket: {}", socket_path.display());

    let state = Arc::new(Mutex::new(IpcState::new()));
    let state_clone = Arc::clone(&state);
    let socket_path_owned = socket_path.to_path_buf();

    std::thread::Builder::new()
        .name("ipc-server".into())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(mut stream) => {
                        let mut reader = BufReader::new(&stream);
                        let mut line = String::new();
                        if reader.read_line(&mut line).is_ok() {
                            let cmd = line.trim();
                            match cmd {
                                "toggle" => {
                                    if let Ok(mut s) = state_clone.lock() {
                                        s.commands.push(IpcCommand::Toggle);
                                    }
                                    let _ = stream.write_all(b"ok\n");
                                }
                                "status" => {
                                    // Status is handled synchronously — the main
                                    // thread will queue the response via a separate
                                    // mechanism. For simplicity, we push the command
                                    // and respond with a placeholder. In practice,
                                    // the main thread polls and responds.
                                    if let Ok(mut s) = state_clone.lock() {
                                        s.commands.push(IpcCommand::Status);
                                    }
                                    // Give main thread a moment to populate response
                                    std::thread::sleep(std::time::Duration::from_millis(50));
                                    // Read back the response that main thread set
                                    let resp = if let Ok(s) = state_clone.lock() {
                                        // Check if there's a status response pending
                                        // For now, just echo a basic acknowledgment
                                        drop(s);
                                        "ok\n".to_string()
                                    } else {
                                        "error\n".to_string()
                                    };
                                    let _ = stream.write_all(resp.as_bytes());
                                }
                                _ => {
                                    let _ = stream.write_all(b"unknown command\n");
                                }
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("[ptt] IPC accept error: {e}");
                    }
                }
            }

            // Clean up socket on exit
            let _ = std::fs::remove_file(&socket_path_owned);
        })
        .expect("Failed to spawn IPC thread");

    state
}
