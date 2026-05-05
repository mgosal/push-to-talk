//! Audio recording and system sound playback.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2::AnyThread;
use objc2_avf_audio::{
    AVAudioApplication, AVAudioApplicationRecordPermission, AVAudioCommonFormat, AVAudioFile,
    AVAudioFormat, AVAudioRecorder,
};
use objc2_foundation::{NSError, NSString, NSURL};

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

pub struct NativeRecorder {
    recorder: Retained<AVAudioRecorder>,
}

impl NativeRecorder {
    pub fn stop(self) -> f64 {
        let duration = unsafe { self.recorder.currentTime() };
        unsafe {
            self.recorder.stop();
        }
        duration
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MicrophonePermission {
    Undetermined,
    Denied,
    Granted,
}

fn file_url(path: &Path) -> Result<Retained<NSURL>, String> {
    let path = path
        .to_str()
        .ok_or_else(|| format!("path is not valid UTF-8: {}", path.display()))?;
    Ok(NSURL::fileURLWithPath(&NSString::from_str(path)))
}

fn ns_error_message(error: &NSError) -> String {
    error.localizedDescription().to_string()
}

/// Start recording 16 kHz mono signed 16-bit PCM audio to a WAV file.
pub fn start_recording(output_path: &Path) -> Result<NativeRecorder, String> {
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create audio directory: {e}"))?;
    }

    let url = file_url(output_path)?;
    let format = unsafe {
        AVAudioFormat::initWithCommonFormat_sampleRate_channels_interleaved(
            AVAudioFormat::alloc(),
            AVAudioCommonFormat::PCMFormatInt16,
            16_000.0,
            1,
            true,
        )
    }
    .ok_or_else(|| "failed to create 16 kHz mono PCM audio format".to_string())?;

    let recorder = unsafe {
        AVAudioRecorder::initWithURL_format_error(AVAudioRecorder::alloc(), &url, &format)
    }
    .map_err(|e| format!("failed to create audio recorder: {}", ns_error_message(&e)))?;

    if !unsafe { recorder.prepareToRecord() } {
        return Err("audio recorder could not prepare the output file".into());
    }
    if !unsafe { recorder.record() } {
        return Err("audio recorder could not start recording".into());
    }

    Ok(NativeRecorder { recorder })
}

pub fn microphone_permission() -> MicrophonePermission {
    let permission = unsafe { AVAudioApplication::sharedInstance().recordPermission() };
    if permission == AVAudioApplicationRecordPermission::Granted {
        MicrophonePermission::Granted
    } else if permission == AVAudioApplicationRecordPermission::Denied {
        MicrophonePermission::Denied
    } else {
        MicrophonePermission::Undetermined
    }
}

pub fn microphone_access_granted() -> bool {
    microphone_permission() == MicrophonePermission::Granted
}

pub fn request_microphone_access() -> Result<(), String> {
    match microphone_permission() {
        MicrophonePermission::Granted => return Ok(()),
        MicrophonePermission::Denied => {
            return Err("microphone access is denied in System Settings".into());
        }
        MicrophonePermission::Undetermined => {}
    }

    let (tx, rx) = mpsc::channel();
    let block = RcBlock::new(move |granted: Bool| {
        let _ = tx.send(granted.as_bool());
    });

    unsafe {
        AVAudioApplication::requestRecordPermissionWithCompletionHandler(&block);
    }

    match rx.recv_timeout(Duration::from_secs(120)) {
        Ok(true) => Ok(()),
        Ok(false) => Err("microphone access was not granted".into()),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            Err("microphone permission request timed out".into())
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err("microphone permission request was interrupted".into())
        }
    }
}

/// Get audio duration in seconds using AVAudioFile metadata.
pub fn get_duration(path: &Path) -> Option<f64> {
    let url = file_url(path).ok()?;
    let file = unsafe { AVAudioFile::initForReading_error(AVAudioFile::alloc(), &url).ok()? };
    let frames = unsafe { file.length() };
    let sample_rate = unsafe { file.processingFormat().sampleRate() };
    if frames > 0 && sample_rate > 0.0 {
        Some(frames as f64 / sample_rate)
    } else {
        None
    }
}
