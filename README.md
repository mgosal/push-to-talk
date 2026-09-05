# push-to-talk

Push-to-talk voice typing for macOS. Hold a key, speak, release — text appears at your cursor.

Native Rust binary. No Python, no Electron. Audio capture uses native macOS AVFAudio APIs.

## Name

The name nods to Wiz Khalifa's "Black and Yellow": "No keys, push to start."

## Requirements

- macOS 14+ (Apple Silicon or Intel)
- [Rust toolchain](https://rustup.rs/) (to build)
- An API key for any OpenAI-compatible audio endpoint — or, for offline transcription, `parakeet-mlx` and `ffmpeg` on Apple Silicon (see [Transcription backends](#transcription-backends))

## Quick start

### Option A: Download the app (no build required)

1. Download the latest zip from the **[Releases page](https://github.com/mgosal/push-to-talk/releases/latest)**
2. Unzip and drag **Push to Talk.app** to your Applications folder
3. Launch the app

> **Gatekeeper notice:** The app is ad-hoc signed but not notarized. On first launch, right-click → **Open** to bypass the warning, or run:
> ```
> xattr -dr com.apple.quarantine '/Applications/Push to Talk.app'
> ```

### Option B: Build from source

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
- Click **Enable Shortcut Access** when you are ready to grant the global right-Option hotkey and paste insertion permission. Once approved in System Settings, return to the app — the hotkey activates immediately without a restart.
- Click **Enable Microphone** when you are ready to grant audio recording permission.
- Use the **Show notifications** checkbox to enable or disable macOS notifications for transcription results.

The setup window also includes direct buttons for **Accessibility** and **Microphone** settings if macOS needs manual approval.

### 4. Set up your speaker profile

The speaker profile teaches the transcription model your accent, vocabulary, and domain terms. This is what makes the tool accurate for *you*.

> **Token cost:** The profile is sent as part of the system prompt on every transcription. Keep it compact — proper nouns, pronunciation hints, and a one-line accent/language note. Aim for under 100 tokens. Verbose profiles work but cost more per dictation.

**Option A: Automated wizard** (recommended)

Click **"Set Up Speaker Profile…"** in the menubar menu. Select a text file containing vocabulary, project names, writing preferences, and any reusable personal context you want the dictation model to remember. The app calls the LLM to generate a compact personalised profile, saves it to `~/.config/push-to-talk/speaker-profile.md`, and opens it in your editor for review.

The generated profile is used automatically for future dictation. You only need to set `transcription.speaker_profile` manually if you want to use a different profile file.

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

A small **PTT** label appears in the menubar, and changes to show the current
state.

> **Tip:** New menu bar items are placed at the *left* end of the icon cluster,
> not next to the clock. If you can't find it, **⌘-drag** it to reorder.

### Push-to-talk

Hold **right Option (⌥)** to record. Release to transcribe and paste at your cursor.

| Menubar | State |
|---------|-------|
| `PTT` | Idle |
| `REC` | Recording |
| `LOCK` | Locked (hands-free) |
| `···` | Transcribing (animates) |
| `✓` | Transcribed successfully (flashes 500ms) |

The label is text rather than an icon on purpose: a template image proved
unreliable to render in the menu bar during testing, and the words state the
mode outright.

### Locked dictation

For longer dictation without holding a key:

1. **Tap right Option** (a quick press, under 400ms) — recording continues hands-free
2. **Tap right Option again**, or press **Escape**, to stop and transcribe

A tap is a press too short to be speech, so it costs nothing: presses that
brief were discarded as "too short" before.

Right Option is also the accent modifier (⌥e, ⌥u, ⌥3), so a press with any
other key involved is never treated as a tap — typing `café` behaves normally.

> **Earlier versions** used Option+Left Arrow to engage the lock. That could
> not work correctly: the event tap is listen-only and cannot swallow a
> keystroke, so the arrow always reached the focused app and moved the caret in
> whatever you were typing. A bare Option tap types nothing, so there is
> nothing to swallow.

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
Quit Push to Talk          ⌘Q
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
- **Retry Transcription** button for failed entries (re-sends the saved audio file)
- **Save Correction** button to update the database

### Transcript files

Save every transcription as a markdown file with YAML frontmatter:

```toml
[transcription]
transcripts_dir = "~/dictation/transcripts"
```

## Configuration

All config lives in `~/.config/push-to-talk/config.toml`. Every field has a default — the only requirement is an API key.

See [`config.example.toml`](config.example.toml) for the full reference with comments.

### Transcription backends

Two engines are available, selected by `[transcription] backend`:

| | `"api"` (default) | `"local"` |
|---|---|---|
| Runs | OpenAI-compatible HTTP API | on-device, via `parakeet-mlx` |
| Needs | API key + network | one-time model download |
| Audio | uploaded to the provider | never leaves the machine |
| Speaker profile | primes the model | not used (acoustic model, no priming) |
| Silence / noise | can return non-transcript text | returns nothing |

Parakeet TDT is a transducer with no autoregressive text decoder, so it
structurally cannot invent fluent text from silence — it returns empty text,
which the app reports as "No speech detected" rather than pasting. The API
backend keeps an edge on domain vocabulary when a speaker profile is supplied,
which is why both are kept.

To run fully offline:

```bash
brew install ffmpeg               # parakeet-mlx decodes audio through it
uv tool install parakeet-mlx      # or: pip install parakeet-mlx
```

```toml
[transcription]
backend = "local"

[transcription.local]
# Must have parakeet-mlx importable. A uv wrapper avoids touching system Python:
command = ["uv", "run", "--no-project", "--with", "parakeet-mlx", "python"]
model = "mlx-community/parakeet-tdt-0.6b-v3"
```

The sidecar is given `/opt/homebrew/bin`, `/usr/local/bin` and `~/.local/bin`
on top of the inherited `PATH`, because an app launched from Finder gets
launchd's minimal one and would otherwise find neither `uv` nor `ffmpeg`.

The model is loaded once into a long-lived sidecar process at app launch, so
only the first dictation of a cold install waits (the initial download is
~2.5 GB). If the sidecar can't start — dependency missing, bad model id — the
app logs the reason, notifies once, and transcribes through the API backend so
dictation keeps working. See `[transcription.local]` in
[`config.example.toml`](config.example.toml) for every knob.

### Audio capture

The app records directly through macOS AVFAudio. There is no external audio recorder dependency to install or configure. Temporary recordings are written as WAV files in the configured `audio_dir` or the system temp directory.

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
model = "openai/gpt-4o-transcribe"
```

```toml
[api]
endpoint = "https://api.openai.com/v1/chat/completions"
model = "gpt-4o-transcribe"
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
| `make check` | Type-check the Rust binary |
| `make test` | Run Rust tests |
| `make build` | Build release binary |
| `make bundle` | Build + create ad-hoc signed `.app` bundle |
| `make install` | Build + bundle + copy to `/Applications` |
| `make uninstall` | Remove from `/Applications` |
| `make clean` | Remove build artifacts and bundle |

### Verification

```bash
make check
make test
make bundle
```

`make bundle` produces a release build, copies it into `Push to Talk.app`, and ad-hoc signs the app bundle. The project sets `MACOSX_DEPLOYMENT_TARGET=14.0` for Cargo builds.

## Privacy

With `backend = "local"`, audio is transcribed on-device and no recording leaves the machine. Note that the local backend falls back to the API backend when the local model cannot be started, so set it up and check the log if offline-only operation matters to you.

With the default `backend = "api"`, audio recordings are sent to the configured OpenAI-compatible API provider for transcription. Speaker profile generation, calibration, and correction learning also send the selected context text, profile text, calibration samples, or correction pairs to that provider.

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
│ Native Audio  │  AVAudioRecorder → WAV
│ Recorder      │  AVAudioApplication permission
└──────┬───────┘
       │ sends WAV
┌──────┴───────┐
│  Transcribe   │  Background thread
│  Thread       │  backend → paste → SQLite → notify
└──────┬───────┘
       │ one of
┌──────┴───────┐  ┌────────────────┐
│  API backend  │  │ Local backend   │
│  reqwest →    │  │ warm sidecar →  │
│  OpenAI-compat│  │ parakeet-mlx    │
└──────────────┘  └────────────────┘
```

## Troubleshooting

### Hotkey not working / text not pasting

Both require Accessibility access. If you rebuilt the app, macOS revokes the permission (the code signature changed).

Fix: open **Setup…** from the menubar and use **Open Accessibility**. If macOS kept an old rebuilt entry, remove it, add the current app, then click **Enable Shortcut Access** again — the hotkey activates immediately on return from System Settings.

### No audio recorded

Open **Setup…** from the menubar and use **Enable Microphone**. If access is denied, use **Open Microphone** and grant permission in System Settings.

You can also reopen **Setup…** and click **Enable Microphone** to trigger the microphone approval step deliberately.

### API errors

Check your API key is valid and the endpoint is reachable:

```bash
test -s ~/.config/push-to-talk/api-key && echo "API key file exists"
curl -s https://openrouter.ai/api/v1/models | head -1   # should return JSON
```

### Local backend falls back to the API

The app logs the reason on startup and before the first fallback. Look for
`[ptt] Local transcription unavailable:` and any `[parakeet]` lines, then
reproduce the sidecar by hand with the same launcher your config uses:

```bash
echo '' | uv run --no-project --with parakeet-mlx python \
  ~/.config/push-to-talk/parakeet_sidecar.py mlx-community/parakeet-tdt-0.6b-v3
```

A healthy sidecar prints `{"ready": true, ...}`. Anything else prints
`{"ready": false, "error": "..."}` naming the cause — most often that
`parakeet-mlx` isn't importable from the interpreter in
`[transcription.local] command`. A sidecar that starts but fails every
recording with `Failed to load audio` is missing `ffmpeg`.

## License

BSD-2-Clause
