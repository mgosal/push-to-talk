# Speaker Profile Example
#
# This file helps the transcription model understand your voice, vocabulary,
# and domain context. Keep it focused on transcription cues, not sensitive
# personal details.
#
# Save your profile as a text or markdown file and point to it in config.toml:
#
#   [transcription]
#   speaker_profile = "~/.config/push-to-talk/speaker-profile.md"

## About the speaker

- Primary language: English
- Accent or dialect notes: add only what helps transcription
- Common filler words: "so", "right", "basically"

## Domain terms and proper nouns

These words appear frequently and should be preferred when the audio is ambiguous:

- Project names: Acme Studio, Atlas CLI, Example Service
- Technical terms: API, webhook, OAuth, PostgreSQL
- Names or acronyms that are often misheard: add your own

## Spelling preferences

- Use the spelling style you prefer
- "email" not "e-mail"
- "setup" (noun) vs "set up" (verb)
