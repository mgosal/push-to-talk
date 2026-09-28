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

# Below this gap between words, treat it as ordinary speech cadence and
# encode nothing — only a real pause is worth a signal.
PAUSE_THRESHOLD_S = 1.0
# A hung-open mic or a long silent stretch shouldn't turn into a wall of
# spaces; anything past this many seconds still reads as "a long pause".
MAX_PAUSE_SPACES = 20


def _has_letter_or_digit(text):
    return any(ch.isalnum() for ch in text)


def build_paused_text(tokens):
    """Reconstruct text from aligned tokens, encoding inter-word pauses as
    extra spaces: an N-second gap between words becomes N extra spaces on
    top of the normal one, so a downstream LLM enrichment pass can see where
    the speaker paused without a separate side channel of timing data.

    Tokens are BPE pieces, not words — a piece starting with a space marks a
    new word, a continuation piece does not. Only word boundaries are
    checked for a pause; the (effectively zero) gaps between pieces of the
    same word are never touched, so a word can't be split by inserted
    spaces.

    A pause is often measured against a trailing punctuation token (".",
    ",") rather than the word after it — the decoder holds punctuation back
    during a pause and only emits it once the next word arrives, so its
    timestamp reflects when the pause ended, not when it started, and
    punctuation timing is rougher than a spoken word's in general. Gaps are
    tracked from the last token that actually had letters or digits in it,
    so a pause landing right before a "." is still caught instead of being
    absorbed into the period's own uninformative gap.
    """
    parts = []
    prev_content_end = None
    for t in tokens:
        piece = t.text
        if prev_content_end is not None and piece.startswith(" ") and _has_letter_or_digit(piece):
            gap = t.start - prev_content_end
            if gap >= PAUSE_THRESHOLD_S:
                extra = min(round(gap), MAX_PAUSE_SPACES)
                piece = (" " * extra) + piece
        parts.append(piece)
        if _has_letter_or_digit(piece):
            prev_content_end = t.end
    return "".join(parts).strip()


# Only a genuinely long pause is worth splitting on. Testing against a real
# recording that reproduced this bug (an 11s pause swallowed the ~2.5s of
# speech right after it) found that splitting on shorter pauses too (down to
# 5-6s) introduces small new seam artifacts of its own — the newly-cut start
# of a segment sometimes loses just enough context to mangle the next word
# or two, or a nearby acronym's tokenization — for pauses that were never
# actually dropping anything. 7s caught the one pause that mattered in that
# recording and left a 5.3s pause elsewhere in the same file untouched.
MIN_SILENCE_S = 7.0
# Window size for the energy scan that finds those pauses.
SILENCE_WINDOW_S = 0.1
# Split just before speech resumes, not at the pause's midpoint or its very
# end — leaving the decoder the least possible silence to sit through before
# real content starts, without risking a cut into the word itself.
SILENCE_END_BUFFER_S = 0.5


def find_silence_splits(audio_path):
    """Return timestamps (seconds) safe to split `audio_path` at — each one
    just before speech resumes after a silence of at least MIN_SILENCE_S.

    Returns an empty list on any failure to read or make sense of the file
    (not 16-bit PCM, too short, wrong dependency missing, anything) — the
    caller's fallback is to transcribe the whole file in one piece, exactly
    as it always has.
    """
    import wave
    import numpy as np

    with wave.open(audio_path, "rb") as w:
        if w.getsampwidth() != 2:
            return []  # only handling 16-bit PCM, what push-to-talk records
        rate = w.getframerate()
        n_channels = w.getnchannels()
        raw = w.readframes(w.getnframes())

    samples = np.frombuffer(raw, dtype=np.int16)
    if n_channels > 1:
        samples = samples.reshape(-1, n_channels).mean(axis=1)

    window = int(SILENCE_WINDOW_S * rate)
    n_windows = len(samples) // window if window > 0 else 0
    if n_windows < 5:
        return []

    trimmed = samples[: n_windows * window].astype(np.float32).reshape(n_windows, window)
    rms = np.sqrt(np.mean(np.square(trimmed), axis=1))

    # Relative to this recording's own noise floor, not a fixed absolute
    # level — mic gain and ambient noise vary too much across recordings for
    # one hardcoded threshold to mean the same thing everywhere. The 1.0
    # floor only matters for a recording that is digital silence throughout.
    noise_floor = float(np.percentile(rms, 10))
    threshold = max(noise_floor * 3.0, 1.0)
    is_silent = rms < threshold

    splits = []
    run_start = None
    for i, silent in enumerate(list(is_silent) + [False]):  # sentinel flushes a trailing run
        if silent and run_start is None:
            run_start = i
        elif not silent and run_start is not None:
            run_len_s = (i - run_start) * SILENCE_WINDOW_S
            if run_len_s >= MIN_SILENCE_S:
                run_start_s = run_start * SILENCE_WINDOW_S
                run_end_s = i * SILENCE_WINDOW_S
                splits.append(max(run_start_s, run_end_s - SILENCE_END_BUFFER_S))
            run_start = None
    return splits


class _CombinedResult:
    """Just enough of AlignedResult's shape for main()'s own use of it —
    tokens and sentences already merged and re-timed across every segment a
    recording was split into."""

    def __init__(self, tokens, sentences):
        self.tokens = tokens
        self.sentences = sentences
        # Only read as a fallback when there are no tokens at all, in which
        # case every segment was silence and "" is the right answer anyway.
        self.text = ""


def transcribe_with_silence_splits(model, audio_path, chunk_duration_s, chunk_overlap_s):
    """Transcribe `audio_path`, first splitting it on long internal silences.

    A transducer decoder can lose its footing after sitting through an
    extended silent stretch and drop the first second or two of speech once
    it resumes — a real, observed failure mode distinct from (and not fixed
    by) chunking an overly long recording: it can happen well inside a
    recording chunk_duration would never split. Splitting at those silences
    and transcribing each piece independently gives the decoder a clean
    start right where it matters. A recording with no qualifying pause never
    reaches the splitting code at all.
    """
    try:
        splits = find_silence_splits(audio_path)
    except Exception:
        splits = []

    if not splits:
        return model.transcribe(
            audio_path, chunk_duration=chunk_duration_s, overlap_duration=chunk_overlap_s
        )

    import wave
    import tempfile
    import os

    with wave.open(audio_path, "rb") as w:
        params = w.getparams()
        rate = w.getframerate()
        raw = w.readframes(w.getnframes())
    frame_size = params.sampwidth * params.nchannels

    boundaries = [0.0] + splits + [len(raw) / frame_size / rate]
    all_tokens = []
    all_sentences = []
    tmp_paths = []
    try:
        for start_s, end_s in zip(boundaries, boundaries[1:]):
            if end_s - start_s < 0.2:
                continue
            start_frame = int(start_s * rate)
            end_frame = int(end_s * rate)
            segment = raw[start_frame * frame_size : end_frame * frame_size]
            if not segment:
                continue

            fd, tmp_path = tempfile.mkstemp(suffix=".wav")
            os.close(fd)
            tmp_paths.append(tmp_path)
            with wave.open(tmp_path, "wb") as out:
                out.setparams(params)
                out.writeframes(segment)

            # Recurses in spirit only — a segment that is itself still
            # longer than chunk_duration_s still gets chunk_duration_s's
            # own overlap-and-merge handling from parakeet-mlx.
            seg_result = model.transcribe(
                tmp_path, chunk_duration=chunk_duration_s, overlap_duration=chunk_overlap_s
            )
            for t in getattr(seg_result, "tokens", []) or []:
                t.start += start_s
                t.end += start_s
                all_tokens.append(t)
            for s in getattr(seg_result, "sentences", []) or []:
                s.start += start_s
                s.end += start_s
                all_sentences.append(s)
    finally:
        for p in tmp_paths:
            try:
                os.remove(p)
            except OSError:
                pass

    return _CombinedResult(all_tokens, all_sentences)


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
            chunk_duration_s = request.get("chunk_duration_s")
            chunk_overlap_s = request.get("chunk_overlap_s", 15.0)
        except Exception as e:  # noqa: BLE001
            emit({"ok": False, "error": "bad request: {}".format(e)})
            continue

        try:
            # Unchunked, the TDT decoder silently drops the tail of long
            # recordings instead of erroring — it just stops emitting tokens
            # partway through. chunk_duration splits audio longer than that
            # into overlapping windows and stitches the results back
            # together; parakeet-mlx processes the file in one pass anyway
            # when it's shorter than chunk_duration, so it's safe to always
            # pass this through. transcribe_with_silence_splits additionally
            # splits on long internal pauses first, a different failure mode
            # chunk_duration alone does not cover.
            result = transcribe_with_silence_splits(
                model, audio_path, chunk_duration_s, chunk_overlap_s
            )
            # Parakeet is a transducer: silence yields empty text rather than
            # invented speech. Pass that through as-is; the caller decides.
            # Built from tokens rather than result.text so pauses can be
            # encoded — see build_paused_text. Falls back to result.text
            # when there's no token-level timing to work from.
            text = build_paused_text(getattr(result, "tokens", []) or []) or (
                getattr(result, "text", "") or ""
            ).strip()
            # Sentence-level timing, straight from the forced alignment the
            # decoder already computes — lets a correction tool seek audio to
            # the start of the sentence a word belongs to.
            sentences = [
                {
                    "text": s.text.strip(),
                    "start": round(s.start, 2),
                    "end": round(s.end, 2),
                }
                for s in getattr(result, "sentences", [])
            ]
            emit({"ok": True, "text": text, "sentences": sentences})
        except Exception as e:  # noqa: BLE001
            emit({"ok": False, "error": compact_error(e)})

    return 0


if __name__ == "__main__":
    sys.exit(main())
