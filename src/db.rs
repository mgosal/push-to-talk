//! SQLite database for dictation history and corrections.

use rusqlite::{params, Connection};
use std::path::Path;
use std::time::SystemTime;

fn timestamp_now() -> String {
    let dur = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    // ISO-ish format from epoch seconds
    let secs = dur.as_secs();
    let dt = time_from_epoch(secs);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        dt.0, dt.1, dt.2, dt.3, dt.4, dt.5
    )
}

pub fn time_from_epoch(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    // Simple UTC breakdown (no TZ library needed)
    let days = (secs / 86400) as u32;
    let time = (secs % 86400) as u32;
    let h = time / 3600;
    let m = (time % 3600) / 60;
    let s = time % 60;

    // Days since 1970-01-01
    let mut y = 1970u32;
    let mut remaining = days;
    loop {
        let days_in_year = if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) { 366 } else { 365 };
        if remaining < days_in_year { break; }
        remaining -= days_in_year;
        y += 1;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let month_days = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut mo = 1u32;
    for &md in &month_days {
        if remaining < md { break; }
        remaining -= md;
        mo += 1;
    }
    (y, mo, remaining + 1, h, m, s)
}

pub fn init(db_path: &Path) {
    let conn = Connection::open(db_path).expect("Failed to open database");
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS dictations (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp TEXT NOT NULL,
            audio_path TEXT,
            transcript TEXT,
            corrected TEXT,
            status TEXT NOT NULL DEFAULT 'success',
            error_detail TEXT,
            latency_s REAL,
            wps REAL,
            duration_s REAL
        )",
    )
    .expect("Failed to create table");
}

#[allow(clippy::too_many_arguments)]
pub fn record(
    db_path: &Path,
    audio_path: Option<&str>,
    transcript: Option<&str>,
    status: &str,
    error_detail: Option<&str>,
    latency: Option<f64>,
    wps: Option<f64>,
    duration: Option<f64>,
) {
    if let Ok(conn) = Connection::open(db_path) {
        let now = timestamp_now();
        let _ = conn.execute(
            "INSERT INTO dictations (timestamp, audio_path, transcript, status, error_detail, latency_s, wps, duration_s)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![now, audio_path, transcript, status, error_detail, latency, wps, duration],
        );
    }
}

pub fn save_correction(db_path: &Path, row_id: i64, corrected: &str) {
    if let Ok(conn) = Connection::open(db_path) {
        let _ = conn.execute(
            "UPDATE dictations SET corrected = ?1 WHERE id = ?2",
            params![corrected, row_id],
        );
    }
}

#[derive(Debug, Clone)]
pub struct DictationRow {
    pub id: i64,
    pub timestamp: String,
    pub transcript: Option<String>,
    pub corrected: Option<String>,
    pub status: String,
    pub latency_s: Option<f64>,
    pub wps: Option<f64>,
    pub duration_s: Option<f64>,
    pub error_detail: Option<String>,
    pub audio_path: Option<String>,
}

pub fn recent(db_path: &Path, limit: u32) -> Vec<DictationRow> {
    let conn = match Connection::open(db_path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let mut stmt = match conn.prepare(
        "SELECT id, timestamp, transcript, corrected, status, latency_s, wps, duration_s, error_detail, audio_path
         FROM dictations ORDER BY id DESC LIMIT ?1",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };

    stmt.query_map(params![limit], |row| {
        Ok(DictationRow {
            id: row.get(0)?,
            timestamp: row.get(1)?,
            transcript: row.get(2)?,
            corrected: row.get(3)?,
            status: row.get(4)?,
            latency_s: row.get(5)?,
            wps: row.get(6)?,
            duration_s: row.get(7)?,
            error_detail: row.get(8)?,
            audio_path: row.get(9)?,
        })
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}
