# Speaker Profile Example
#
# This file helps the transcription model understand your voice, vocabulary,
# and domain context. Keep it focused on transcription cues, not sensitive
# personal details.
#
# Save your profile as ~/.config/push-to-talk/speaker-profile.md. The app will
# use that generated profile automatically. Set transcription.speaker_profile in
# config.toml only when you want to use a different file.

## Profile Summary

- Recurring work: add the topics, projects, or workflows you dictate about
- Preferred output: add whether you want terse notes, polished prose, or literal drafts
- Important context: add stable facts that help future dictation without adding sensitive details

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

## Personalization notes

- Preferred terminology: add words the model should favor when audio is ambiguous
- Reusable context: add non-sensitive background that would save repeated explanation
- Unknowns: leave anything uncertain blank instead of guessing
