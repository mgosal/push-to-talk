//! Transcription evaluation: export a dataset, take corrections back, score.
//!
//! The point is to answer whether an enrichment step earns its place, with
//! measurements rather than impressions. Three versions of the same audio get
//! compared: what Parakeet produced, what the speaker says was actually said,
//! and what an enrichment pass makes of the first. Without the middle one
//! there is no ground truth and no way to tell an improvement from a rewrite.

use std::collections::HashMap;
use std::path::Path;

/// One row of the evaluation set.
pub struct Sample {
    pub id: i64,
    pub timestamp: String,
    pub duration_s: Option<f64>,
    pub raw: String,
    pub corrected: Option<String>,
    pub audio_path: Option<String>,
    /// Sentence-level timing as a JSON array, when the local backend
    /// produced this sample — lets a correction tool seek audio per sentence.
    pub sentences_json: Option<String>,
}

// ── Scoring ───────────────────────────────────────────────────────────

/// Levenshtein distance over any sequence of comparable items.
fn edit_distance<T: PartialEq>(a: &[T], b: &[T]) -> usize {
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    // Two rows is enough: the recurrence only looks at the previous one.
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];

    for (i, ai) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, bj) in b.iter().enumerate() {
            let cost = usize::from(ai != bj);
            curr[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(curr[j] + 1);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

/// Lowercase, strip punctuation, collapse whitespace.
///
/// Scoring should reflect whether the right words were heard, not whether a
/// comma landed in the same place — punctuation is the enrichment step's job,
/// and counting it here would flatter or punish the wrong thing.
pub fn normalise(text: &str) -> String {
    let cleaned: String = text
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c.is_whitespace() || c == '\'' { c } else { ' ' })
        .collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Word error rate of `hypothesis` against `reference`, as a fraction.
pub fn wer(reference: &str, hypothesis: &str) -> f64 {
    let nr = normalise(reference);
    let nh = normalise(hypothesis);
    let r: Vec<&str> = nr.split_whitespace().collect();
    let h: Vec<&str> = nh.split_whitespace().collect();
    if r.is_empty() {
        return if h.is_empty() { 0.0 } else { 1.0 };
    }
    edit_distance(&r, &h) as f64 / r.len() as f64
}

/// Character error rate of `hypothesis` against `reference`, as a fraction.
pub fn cer(reference: &str, hypothesis: &str) -> f64 {
    let r: Vec<char> = normalise(reference).chars().collect();
    let h: Vec<char> = normalise(hypothesis).chars().collect();
    if r.is_empty() {
        return if h.is_empty() { 0.0 } else { 1.0 };
    }
    edit_distance(&r, &h) as f64 / r.len() as f64
}

/// Words present in the reference but missing from the hypothesis, counted.
///
/// This is what surfaces a systematically mis-heard proper noun: the name
/// itself tops the missing list while whatever the model heard instead tops
/// the spurious one.
pub fn word_diff(reference: &str, hypothesis: &str) -> (HashMap<String, usize>, HashMap<String, usize>) {
    let mut missing: HashMap<String, usize> = HashMap::new();
    let mut spurious: HashMap<String, usize> = HashMap::new();

    let mut ref_counts: HashMap<&str, i64> = HashMap::new();
    let norm_r = normalise(reference);
    let norm_h = normalise(hypothesis);
    for w in norm_r.split_whitespace() {
        *ref_counts.entry(w).or_default() += 1;
    }
    for w in norm_h.split_whitespace() {
        *ref_counts.entry(w).or_default() -= 1;
    }
    for (w, n) in ref_counts {
        if n > 0 {
            *missing.entry(w.to_string()).or_default() += n as usize;
        } else if n < 0 {
            *spurious.entry(w.to_string()).or_default() += (-n) as usize;
        }
    }
    (missing, spurious)
}

// ── Export ────────────────────────────────────────────────────────────

const HEADER: &str = "\
# Transcription evaluation set
#
# Each block below is one dictation. The `corrected:` section is pre-filled
# with exactly what Parakeet produced — edit it so it says what you actually
# said, and leave the rest alone. Blocks you do not touch score as perfect,
# so it is safe to correct only the ones that are wrong and skip the others.
#
# Do not edit the `## id` lines or the `raw:` sections: the importer matches
# on the id, and `raw:` is the record of what the model produced.
#
# Re-import with:  push-to-talk --import-corrections <this file>
";

/// Render samples as a hand-editable document.
///
/// `audio:` and `sentences:` are `#`-prefixed like the header, so a human
/// editor sees only the header's comment convention and the importer's line
/// parser skips them for the same reason it skips the header: it only ever
/// looks for `## `, `raw:`, and `corrected:`. They exist for a correction
/// tool that wants to play back the audio a block came from.
pub fn export(samples: &[Sample]) -> String {
    let mut out = String::from(HEADER);
    for s in samples {
        let dur = s.duration_s.map(|d| format!("{d:.1}s")).unwrap_or_else(|| "?".into());
        out.push_str(&format!("\n## {} | {} | {}\n", s.id, s.timestamp, dur));
        if let Some(audio) = &s.audio_path {
            out.push_str(&format!("# audio: {audio}\n"));
        }
        if let Some(sentences) = &s.sentences_json {
            out.push_str(&format!("# sentences: {sentences}\n"));
        }
        out.push_str(&format!("\nraw:\n{}\n\ncorrected:\n{}\n",
            s.raw.trim(),
            s.corrected.as_deref().unwrap_or(&s.raw).trim()));
    }
    out
}

/// One scored block: id, raw, corrected, and whether a correction tool
/// marked it reviewed (`# reviewed: yes`).
pub struct ScoredBlock {
    /// Unused by scoring itself, but part of the parsed shape for a caller
    /// that wants to report which block a word-error came from.
    #[allow(dead_code)]
    pub id: i64,
    pub raw: String,
    pub corrected: String,
    pub reviewed: bool,
}

/// Parse an export into scored blocks.
///
/// Scoring reads the file rather than the database on purpose. Only *changed*
/// blocks are stored as corrections, so scoring from the database would take
/// the wrong denominator — it would measure error over the subset already
/// known to be wrong, and report a terrible rate for a model that is mostly
/// right. The file carries every reviewed sample, corrected or not.
///
/// An untouched block (`corrected` equal to `raw`) is ambiguous on its own:
/// it could mean "reviewed, and it was already right" or "never looked at
/// yet". `# reviewed: yes` (written by the HTML correction viewer, never by
/// hand-editing) disambiguates it. Filtering on that marker only kicks in
/// once at least one block in the file carries it — a file with none, which
/// is what hand-editing in a text editor produces, keeps scoring every block
/// as before, so that workflow is untouched.
pub fn parse_scored(text: &str) -> Vec<ScoredBlock> {
    let mut out = Vec::new();
    let mut id: Option<i64> = None;
    let mut section: Option<&str> = None;
    let mut reviewed = false;
    let (mut raw, mut corrected): (Vec<&str>, Vec<&str>) = (Vec::new(), Vec::new());

    fn flush(
        id: &mut Option<i64>,
        raw: &mut Vec<&str>,
        corrected: &mut Vec<&str>,
        reviewed: &mut bool,
        out: &mut Vec<ScoredBlock>,
    ) {
        if let Some(i) = id.take() {
            let r = raw.join("\n").trim().to_string();
            let c = corrected.join("\n").trim().to_string();
            if !r.is_empty() {
                out.push(ScoredBlock { id: i, raw: r, corrected: c, reviewed: *reviewed });
            }
        }
        raw.clear();
        corrected.clear();
        *reviewed = false;
    }

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            flush(&mut id, &mut raw, &mut corrected, &mut reviewed, &mut out);
            section = None;
            id = rest.split('|').next().and_then(|s| s.trim().parse().ok());
        } else if line.trim() == "# reviewed: yes" {
            reviewed = true;
        } else if line.trim() == "raw:" {
            section = Some("raw");
        } else if line.trim() == "corrected:" {
            section = Some("corrected");
        } else if !line.starts_with('#') {
            match section {
                Some("raw") => raw.push(line),
                Some("corrected") => corrected.push(line),
                _ => {}
            }
        }
    }
    flush(&mut id, &mut raw, &mut corrected, &mut reviewed, &mut out);
    out
}

/// Parse an edited export back into (id, corrected text) pairs.
pub fn parse_import(text: &str) -> Vec<(i64, String)> {
    let mut out = Vec::new();
    let mut id: Option<i64> = None;
    let mut in_corrected = false;
    let mut buf: Vec<&str> = Vec::new();

    let flush = |id: &mut Option<i64>, buf: &mut Vec<&str>, out: &mut Vec<(i64, String)>| {
        if let Some(i) = id.take() {
            let text = buf.join("\n").trim().to_string();
            if !text.is_empty() {
                out.push((i, text));
            }
        }
        buf.clear();
    };

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            flush(&mut id, &mut buf, &mut out);
            in_corrected = false;
            id = rest.split('|').next().and_then(|s| s.trim().parse().ok());
        } else if line.trim() == "corrected:" {
            in_corrected = true;
            buf.clear();
        } else if line.trim() == "raw:" {
            in_corrected = false;
        } else if in_corrected && !line.starts_with('#') {
            buf.push(line);
        }
    }
    flush(&mut id, &mut buf, &mut out);
    out
}

// ── Database access ───────────────────────────────────────────────────

/// Load successful transcripts, newest first, for evaluation.
pub fn load_samples(db_path: &Path, limit: u32) -> Vec<Sample> {
    let Ok(conn) = rusqlite::Connection::open(db_path) else {
        return vec![];
    };
    let Ok(mut stmt) = conn.prepare(
        "SELECT id, timestamp, duration_s, transcript, corrected, audio_path, sentences_json
         FROM dictations
         WHERE status = 'success' AND transcript IS NOT NULL AND length(trim(transcript)) > 0
         ORDER BY id DESC LIMIT ?1",
    ) else {
        return vec![];
    };
    stmt.query_map(rusqlite::params![limit], |row| {
        Ok(Sample {
            id: row.get(0)?,
            timestamp: row.get(1)?,
            duration_s: row.get(2).ok(),
            raw: row.get::<_, String>(3)?,
            corrected: row.get::<_, Option<String>>(4)?,
            audio_path: row.get::<_, Option<String>>(5)?,
            sentences_json: row.get::<_, Option<String>>(6)?,
        })
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scores_a_perfect_transcript_as_zero_error() {
        assert_eq!(wer("the contract is signed", "the contract is signed"), 0.0);
        assert_eq!(cer("the contract is signed", "the contract is signed"), 0.0);
    }

    #[test]
    fn ignores_case_and_punctuation() {
        assert_eq!(wer("The Contract, is signed.", "the contract is signed"), 0.0);
    }

    #[test]
    fn scores_a_mis_heard_proper_noun() {
        // One word wrong out of five.
        let e = wer("we should ask orbit about", "we should ask orbits about");
        assert!((e - 0.2).abs() < 1e-9, "wer was {e}");
        // Character-level catches that it is a near miss, not a different word.
        assert!(cer("orbit", "orbits") < 0.5);
    }

    #[test]
    fn names_the_systematically_wrong_word() {
        let (missing, spurious) = word_diff("orbit ai and orbit cli", "orbitai and orbits cli");
        assert_eq!(missing.get("orbit"), Some(&2));
        assert_eq!(spurious.get("orbits"), Some(&1));
        assert_eq!(spurious.get("orbitai"), Some(&1));
    }

    #[test]
    fn scoring_counts_untouched_blocks_as_correct() {
        // The denominator must include samples the reviewer skipped, or the
        // score measures only the ones already known to be wrong.
        let doc = "## 1 | t | 1s\n\nraw:\nthe orbit cli import\n\ncorrected:\nthe orbit cli import\n\
                   \n## 2 | t | 1s\n\nraw:\nthe orbits cli import\n\ncorrected:\nthe orbit cli import\n";
        let scored = parse_scored(doc);
        assert_eq!(scored.len(), 2, "both blocks must be scored");

        let total: f64 = scored.iter().map(|b| wer(&b.corrected, &b.raw)).sum();
        // One block perfect, one with a single wrong word out of four.
        assert!((total / 2.0 - 0.125).abs() < 1e-9, "mean wer was {}", total / 2.0);
    }

    #[test]
    fn parse_scored_reports_which_blocks_are_marked_reviewed() {
        // Written by the correction viewer, one reviewed and confirmed
        // correct without editing, one never opened yet.
        let doc = "## 1 | t | 1s\n# reviewed: yes\n\nraw:\nthe orbit cli import\n\
                   \n\ncorrected:\nthe orbit cli import\n\
                   \n## 2 | t | 1s\n\nraw:\nthe orbit cli import\n\ncorrected:\nthe orbit cli import\n";
        let scored = parse_scored(doc);
        assert_eq!(scored.len(), 2, "parsing itself does not filter");
        assert!(scored[0].reviewed);
        assert!(!scored[1].reviewed, "no marker means not yet reviewed");
    }

    #[test]
    fn parse_scored_never_assumes_reviewed_on_its_own() {
        // No block carries "# reviewed: yes" — a file edited directly in a
        // text editor, never opened in the viewer. Deciding that an absent
        // marker everywhere means "score it all anyway" is the caller's
        // call (the --score CLI handler), not this parser's — it just
        // reports what it found.
        let doc = "## 1 | t | 1s\n\nraw:\nthe orbit cli import\n\ncorrected:\nthe orbit cli import\n";
        let scored = parse_scored(doc);
        assert!(!scored[0].reviewed);
    }

    #[test]
    fn round_trips_an_edited_export() {
        let samples = vec![Sample {
            id: 42,
            timestamp: "2026-09-08T10:00:00".into(),
            duration_s: Some(3.5),
            raw: "the orbits cli import".into(),
            corrected: None,
            audio_path: None,
            sentences_json: None,
        }];
        let doc = export(&samples);
        // Pre-filled with the raw text, so untouched blocks score as perfect.
        assert!(doc.contains("corrected:\nthe orbits cli import"));

        let edited = doc.replace("corrected:\nthe orbits cli import", "corrected:\nthe orbit cli import");
        let parsed = parse_import(&edited);
        assert_eq!(parsed, vec![(42, "the orbit cli import".to_string())]);
    }

    #[test]
    fn export_carries_audio_and_timing_metadata_without_breaking_import() {
        let samples = vec![Sample {
            id: 9,
            timestamp: "2026-09-08T10:00:00".into(),
            duration_s: Some(2.4),
            raw: "hi there. bye now.".into(),
            corrected: None,
            audio_path: Some("/tmp/ptt/ptt-123.wav".into()),
            sentences_json: Some(
                r#"[{"text":"hi there.","start":0.0,"end":1.2},{"text":"bye now.","start":1.2,"end":2.4}]"#
                    .into(),
            ),
        }];
        let doc = export(&samples);
        assert!(doc.contains("# audio: /tmp/ptt/ptt-123.wav"));
        assert!(doc.contains("# sentences: [{\"text\":\"hi there.\""));

        // A correction tool reads the metadata; the plain importer must
        // still see only the raw/corrected sections underneath it.
        let edited = doc.replace(
            "corrected:\nhi there. bye now.",
            "corrected:\nhi there, bye now.",
        );
        assert_eq!(
            parse_import(&edited),
            vec![(9, "hi there, bye now.".to_string())]
        );
    }

    #[test]
    fn import_ignores_the_raw_sections_and_comments() {
        let doc = "# a comment\n\n## 7 | ts | 1.0s\n\nraw:\nwrong text\n\ncorrected:\nright text\n";
        assert_eq!(parse_import(doc), vec![(7, "right text".to_string())]);
    }

    #[test]
    fn handles_multi_line_and_empty_corrections() {
        let doc = "## 1 | t | 1s\n\nraw:\nx\n\ncorrected:\nline one\nline two\n\n## 2 | t | 1s\n\nraw:\ny\n\ncorrected:\n\n";
        let got = parse_import(doc);
        assert_eq!(got, vec![(1, "line one\nline two".to_string())]);
    }
}
