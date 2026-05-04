# push-to-talk

Push-to-talk voice typing for macOS. Hold a key, speak, release — text appears at your cursor.

Native Rust binary. No Python, no Electron. ~3.4 MB release binary, with `ffmpeg` used for audio capture.

## Name

The name nods to Wiz Khalifa's "Black and Yellow": "No keys, push to start."

## Requirements

- macOS 13+ (Apple Silicon or Intel)
- [Rust toolchain](https://rustup.rs/) (to build)
- [ffmpeg](https://formulae.brew.sh/formula/ffmpeg) (for audio recording)
- An API key for any OpenAI-compatible audio endpoint

## Quick start

### 1. Install

```bash
git clone https://github.com/mgosal/push-to-talk.git
cd push-to-talk
make install
```

This builds the release binary, packages it as an ad-hoc signed `.app` bundle, and copies it to `/Applications/Push to Talk.app`.

### 2. Launch

```bash
open '/Applications/Push to Talk.app'
```

### 3. Complete setup

The setup window opens automatically until the required pieces are complete. You can reopen it from the menubar via **Setup…**.

- Choose **OpenRouter** or **OpenAI** and save your API key.
- Click **Enable Shortcut Access** when you are ready to grant the global right-Option hotkey and paste insertion permission.
- Click **Enable Microphone** when you are ready to grant audio recording permission.

The setup window also includes direct buttons for **Accessibility**, **Input Monitoring**, and **Microphone** settings if macOS needs manual approval.

### 4. Set up your speaker profile

The speaker profile teaches the transcription model your accent, vocabulary, and domain terms. This is what makes the tool accurate for *you*.

**Option A: Automated wizard** (recommended)

Click **"Set Up Speaker Profile…"** in the menubar menu. Select a text file containing vocabulary, project names, and writing preferences. The app calls the LLM to generate a personalised profile and opens it in your editor for review.

**Option B: Manual**

```bash
cp speaker-profile.example.md ~/.config/push-to-talk/speaker-profile.md
# Edit to match your voice and vocabulary
```

Then point to it in your config:

```toml
[transcription]
speaker_profile = "~/.config/push-to-talk/speaker-profile.md"
```

### 5. Optional: Calibrate your voice

After creating a profile, click **"Calibrate Voice"** in the menu. The app generates 20 sentences tailored to your domain vocabulary. Read each one using push-to-talk. The system compares what you said against what the model heard, identifies systematic error patterns (accent quirks, tool name misrecognition, consonant clipping), and updates your profile automatically.

### 6. Optional: Learn from corrections

As you use the app, correct mistakes in the **History & Corrections** window. Over time, click **"Learn from Corrections"** to analyse your correction patterns and update the speaker profile with new pronunciation rules.

## Usage

A ⚪ icon appears in the menubar.

### Push-to-talk

Hold **right Option (⌥)** to record. Release to transcribe and paste at your cursor.

| Icon | State |
|------|-------|
| ⚪ | Idle |
| 🔴 | Recording |
| 🔒 | Locked (hands-free) |
| 🟡 | Transcribing (pulses) |

### Locked dictation

For longer dictation without holding a key:

1. Hold **right Option** (starts recording)
2. Press **left arrow** while holding right Option (engages lock)
3. Release everything — recording continues hands-free
4. Press **right Option** again to stop and transcribe

### Audio feedback

| Sound | Event |
|-------|-------|
| Tink | Recording started |
| Morse | Locked mode engaged |
| Pop | Sent to API |
| Glass | Text pasted |
| Basso | Error or too short |

### Menu

```
Idle — ready to dictate       (click to copy last result)
────────────────────────
Toggle Recording
History & Corrections
────────────────────────
Setup…
Set Up Speaker Profile… / Calibrate Voice
Learn from Corrections
────────────────────────
3 transcriptions · avg 2.1s
────────────────────────
Quit Dictate               ⌘Q
```

### External control (Stream Deck / scripts)

```bash
push-to-talk --toggle     # Start/stop recording on running instance
push-to-talk --status     # Query state
```

Uses a Unix socket (`/tmp/ptt.sock`) and PID file (`/tmp/ptt.pid`).

### History & Corrections

Open via the menu. Native Cocoa window with:
- Table of recent dictations (last 50)
- Editable text view for correcting transcripts
- "Save Correction" button to update the database

### Transcript files

Save every transcription as a markdown file with YAML frontmatter:

```toml
[transcription]
transcripts_dir = "~/dictation/transcripts"
```

## Configuration

All config lives in `~/.config/push-to-talk/config.toml`. Every field has a default — the only requirement is an API key.

See [`config.example.toml`](config.example.toml) for the full reference with comments.

### API key resolution order

1. `key = "..."` in config.toml (inline)
2. Contents of `key_file` (default: `~/.config/push-to-talk/api-key`)
3. `OPENROUTER_API_KEY` environment variable
4. `OPENAI_API_KEY` environment variable

### Switching providers

Use **Setup…** in the menubar for OpenRouter or OpenAI. It writes `~/.config/push-to-talk/api-key` and updates `config.toml` to one of the supported API patterns:

```toml
[api]
endpoint = "https://openrouter.ai/api/v1/chat/completions"
model = "openai/gpt-4o-audio-preview"
```

```toml
[api]
endpoint = "https://api.openai.com/v1/chat/completions"
model = "gpt-4o-audio-preview"
```

Any endpoint that accepts the OpenAI chat completions format with audio input will work.

## Building

### Manual build (without install)

```bash
cargo build --release
make bundle    # creates Push to Talk.app in the project root
```

### Makefile targets

| Target | Description |
|--------|-------------|
| `make build` | Build release binary |
| `make bundle` | Build + create ad-hoc signed `.app` bundle |
| `make install` | Build + bundle + copy to `/Applications` |
| `make uninstall` | Remove from `/Applications` |
| `make clean` | Remove build artifacts and bundle |

## Privacy

Audio recordings are sent to the configured OpenAI-compatible API provider for transcription. Speaker profile generation, calibration, and correction learning also send the selected context text, profile text, calibration samples, or correction pairs to that provider.

API keys are stored locally in `~/.config/push-to-talk/api-key` by default. Dictation history is stored locally in SQLite, and optional transcript files are written only when `transcripts_dir` is configured.

## Architecture

```
┌──────────────┐
│  Main Thread  │  NSApplication run loop
│  NSStatusBar  │  Menubar icon + menu
│  NSTimer      │  100ms poll for events
│  History UI   │  NSTableView + NSTextView
└──────┬───────┘
       │ polls
┌──────┴───────┐  ┌──────────────┐
│ Hotkey Thread │  │  IPC Thread   │
│  CGEventTap   │  │  Unix Socket  │
│  CFRunLoop    │  │  /tmp/ptt.sock│
└──────────────┘  └──────────────┘
       │ triggers
┌──────┴───────┐
│  Transcribe   │  Background thread
│  Thread       │  reqwest → API → paste → SQLite → notify
└──────────────┘
```

## Troubleshooting

### Hotkey not working / text not pasting

Both require Accessibility access. If you rebuilt the app, macOS revokes the permission (the code signature changed).

Fix: open **Setup…** from the menubar and use **Open Accessibility** / **Open Input Monitoring**. If macOS kept an old rebuilt entry, remove it, add the current app, then click **Enable Shortcut Access** again.

### No audio recorded

Check that ffmpeg is installed and accessible:

```bash
which ffmpeg   # should return a path
ffmpeg -f avfoundation -list_devices true -i "" 2>&1 | grep -i audio
```

You can also reopen **Setup…** and click **Enable Microphone** to trigger the microphone approval step deliberately.

### API errors

Check your API key is valid and the endpoint is reachable:

```bash
test -s ~/.config/push-to-talk/api-key && echo "API key file exists"
curl -s https://openrouter.ai/api/v1/models | head -1   # should return JSON
```

## License

BSD-2-Clause
