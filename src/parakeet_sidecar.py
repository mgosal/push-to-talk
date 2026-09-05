#!/usr/bin/env python3
"""Warm Parakeet transcription sidecar for push-to-talk.

Loads an NVIDIA Parakeet model once via ``parakeet-mlx`` and then serves
transcription requests over a line-delimited JSON protocol on stdin/stdout,
so the model load cost is paid once per app launch rather than per dictation.

Protocol
--------
On startup, exactly one line is written to stdout:

    {"ready": true,  "model": "<model id>"}
    {"ready": false, "error": "<why the model could not be loaded>"}

Then, for each request line read from stdin:

    request   {"audio": "/abs/path/to/recording.wav"}
    response  {"ok": true,  "text": "<transcript>"}
              {"ok": false, "error": "<why it failed>"}

Exactly one response line is written per request line, in order. Diagnostics
go to stderr, never stdout — stdout carries only protocol lines.

This file is embedded in the push-to-talk binary and written to the config
directory at spawn time, so it stays in sync with the app that launched it.
"""

import json
import sys

DEFAULT_MODEL = "mlx-community/parakeet-tdt-0.6b-v3"


def emit(obj):
    """Write one protocol line to stdout and flush immediately."""
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def compact_error(e):
    """Collapse an exception into one short line fit for a notification.

    Audio decoding runs through ffmpeg, whose failures arrive as a multi-line
    dump of version banners and configure flags. The parent surfaces this text
    in the menubar and in a macOS notification, so keep the informative ends
    and drop the middle.
    """
    lines = [ln.strip() for ln in str(e).splitlines() if ln.strip()]
    if not lines:
        detail = ""
    elif len(lines) <= 2:
        detail = " ".join(lines)
    else:
        detail = "{} … {}".format(lines[0], lines[-1])
    message = "{}: {}".format(type(e).__name__, detail).strip().rstrip(":")
    if len(message) > 300:
        message = message[:299] + "…"
    return message


def load_model(model_id):
    try:
        from parakeet_mlx import from_pretrained
    except ImportError as e:
        raise RuntimeError(
            "parakeet-mlx is not installed for this interpreter "
            "({}): {}. Install it with: uv tool install parakeet-mlx "
            "(or pip install parakeet-mlx)".format(sys.executable, e)
        ) from e
    # First call downloads the weights from Hugging Face; later calls hit the
    # local cache. Either way this happens once, before we report ready.
    return from_pretrained(model_id)


def main():
    model_id = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_MODEL

    try:
        model = load_model(model_id)
    except Exception as e:  # noqa: BLE001 — any failure must reach the parent
        emit({"ready": False, "error": compact_error(e)})
        return 1

    emit({"ready": True, "model": model_id})

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
            audio_path = request["audio"]
        except Exception as e:  # noqa: BLE001
            emit({"ok": False, "error": "bad request: {}".format(e)})
            continue

        try:
            result = model.transcribe(audio_path)
            # Parakeet is a transducer: silence yields empty text rather than
            # invented speech. Pass that through as-is; the caller decides.
            text = (getattr(result, "text", "") or "").strip()
            emit({"ok": True, "text": text})
        except Exception as e:  # noqa: BLE001
            emit({"ok": False, "error": compact_error(e)})

    return 0


if __name__ == "__main__":
    sys.exit(main())
