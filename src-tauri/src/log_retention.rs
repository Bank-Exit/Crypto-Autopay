//! Age-based retention of the persistent log file.
//!
//! The user picks how long technical logs are kept (0 h = nothing written to disk,
//! up to 60 days). Rotated files older than the window are deleted and the current
//! file is trimmed in place, so the file never holds more than the chosen period.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use chrono::{Local, NaiveDateTime, TimeZone};
use serde::Serialize;

pub const DEFAULT_HOURS: u32 = 12;
pub const MAX_HOURS: u32 = 60 * 24;
const SETTINGS_FILE: &str = "log_settings.json";
const LOG_PREFIX: &str = "autopay";
const CURRENT_FILE: &str = "autopay.log";
/// Safety net when the window is long: oldest rotated files go first beyond this size.
const MAX_TOTAL_BYTES: u64 = 200 * 1024 * 1024;
/// Lines handed to the logs drawer at startup.
const HISTORY_MAX_LINES: usize = 5000;

static RETENTION_HOURS: AtomicU32 = AtomicU32::new(DEFAULT_HOURS);

pub fn hours() -> u32 {
    RETENTION_HOURS.load(Ordering::Relaxed)
}

/// Used as the file target filter: nothing reaches the disk when retention is 0.
pub fn file_enabled() -> bool {
    hours() > 0
}

pub fn load(data_dir: &Path) {
    let h = std::fs::read_to_string(data_dir.join(SETTINGS_FILE))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("retention_hours").and_then(|h| h.as_u64()))
        .map(|h| (h as u32).min(MAX_HOURS))
        .unwrap_or(DEFAULT_HOURS);
    RETENTION_HOURS.store(h, Ordering::Relaxed);
}

pub fn save(data_dir: &Path, hours: u32) -> Result<(), String> {
    if hours > MAX_HOURS {
        return Err(format!("retention must be between 0 and {MAX_HOURS} hours"));
    }
    let body = serde_json::json!({ "retention_hours": hours }).to_string();
    std::fs::write(data_dir.join(SETTINGS_FILE), body).map_err(|e| e.to_string())?;
    RETENTION_HOURS.store(hours, Ordering::Relaxed);
    Ok(())
}

fn cutoff() -> NaiveDateTime {
    (Local::now() - chrono::Duration::hours(hours() as i64)).naive_local()
}

/// Local timestamp of a line written by tauri-plugin-log: `[YYYY-MM-DD][HH:MM:SS][LEVEL][target] msg`.
fn line_time(line: &str) -> Option<NaiveDateTime> {
    let b = line.as_bytes();
    if b.len() < 22 || b[0] != b'[' || b[11] != b']' || b[12] != b'[' || b[21] != b']' {
        return None;
    }
    NaiveDateTime::parse_from_str(&format!("{} {}", &line[1..11], &line[13..21]), "%Y-%m-%d %H:%M:%S").ok()
}

/// Rotated files first (oldest to newest), then the current file.
fn log_files(log_dir: &Path) -> Vec<PathBuf> {
    let mut rotated: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(log_dir)
        .map(|rd| rd.flatten()
            .filter(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                n.starts_with(LOG_PREFIX) && n.ends_with(".log") && n != CURRENT_FILE
            })
            .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
            .collect())
        .unwrap_or_default();
    rotated.sort();
    let mut files: Vec<PathBuf> = rotated.into_iter().map(|(_, p)| p).collect();
    let current = log_dir.join(CURRENT_FILE);
    if current.exists() {
        files.push(current);
    }
    files
}

/// Delete what falls outside the retention window. Safe to call while the logger runs:
/// the current file is truncated in place, and the plugin appends to its end.
pub fn prune(log_dir: &Path) {
    let (current, rotated): (Vec<PathBuf>, Vec<PathBuf>) = log_files(log_dir)
        .into_iter()
        .partition(|f| f.file_name().map(|n| n == CURRENT_FILE).unwrap_or(false));
    let current = current.into_iter().next();

    if hours() == 0 {
        for f in &rotated {
            let _ = std::fs::remove_file(f);
        }
        // Emptied, not deleted: the logger keeps its handle and writes again if retention comes back.
        if let Some(cur) = &current {
            let _ = std::fs::OpenOptions::new().write(true).truncate(true).open(cur);
        }
        return;
    }

    let limit = Local.from_local_datetime(&cutoff()).earliest()
        .map(std::time::SystemTime::from)
        .unwrap_or(std::time::UNIX_EPOCH);
    let mut kept = Vec::new();
    for f in rotated {
        match std::fs::metadata(&f).and_then(|m| m.modified()) {
            Ok(t) if t < limit => { let _ = std::fs::remove_file(&f); }
            _ => kept.push(f),
        }
    }

    let mut total: u64 = kept.iter().chain(current.iter())
        .filter_map(|f| std::fs::metadata(f).ok())
        .map(|m| m.len())
        .sum();
    for f in &kept {
        if total <= MAX_TOTAL_BYTES { break; }
        if let Ok(m) = std::fs::metadata(f) {
            if std::fs::remove_file(f).is_ok() { total -= m.len(); }
        }
    }

    if let Some(cur) = current {
        trim_file(&cur, cutoff());
    }
}

/// Drop the lines older than `limit`; continuation lines follow their header line.
fn trim_file(path: &Path, limit: NaiveDateTime) {
    let Ok(content) = std::fs::read_to_string(path) else { return };
    let mut start = None;
    let mut offset = 0;
    for line in content.split_inclusive('\n') {
        if let Some(t) = line_time(line) {
            if t >= limit {
                start = Some(offset);
                break;
            }
        }
        offset += line.len();
    }
    let start = start.unwrap_or(content.len());
    if start == 0 {
        return;
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().write(true).truncate(true).open(path) {
        let _ = f.write_all(&content.as_bytes()[start..]);
    }
}

#[derive(Serialize)]
pub struct HistoryLine {
    /// RFC 3339 with the local offset.
    pub t: String,
    pub level: String,
    pub module: String,
    pub msg: String,
}

/// Lines still inside the retention window, oldest first, for the logs drawer.
pub fn history(log_dir: &Path) -> Vec<HistoryLine> {
    if hours() == 0 {
        return Vec::new();
    }
    let limit = cutoff();
    let mut out: Vec<HistoryLine> = Vec::new();
    for f in log_files(log_dir) {
        let Ok(content) = std::fs::read_to_string(&f) else { continue };
        for line in content.lines() {
            match line_time(line) {
                Some(t) => {
                    if t < limit { continue; }
                    // [date][time][LEVEL][target] msg
                    let rest = &line[22..];
                    let mut parts = rest.splitn(3, ']');
                    let level = parts.next().unwrap_or("").trim_start_matches('[').to_lowercase();
                    let target = parts.next().unwrap_or("").trim_start_matches('[');
                    let msg = parts.next().unwrap_or("").trim_start().to_string();
                    let module = match target.rsplit("::").next().unwrap_or(target) {
                        "app_lib" => "lib",
                        m => m,
                    }.to_string();
                    let Some(local) = Local.from_local_datetime(&t).earliest() else { continue };
                    out.push(HistoryLine { t: local.to_rfc3339(), level, module, msg });
                }
                None => {
                    if let Some(last) = out.last_mut() {
                        last.msg.push('\n');
                        last.msg.push_str(line);
                    }
                }
            }
        }
    }
    if out.len() > HISTORY_MAX_LINES {
        out.drain(..out.len() - HISTORY_MAX_LINES);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plugin_timestamp() {
        let t = line_time("[2026-10-31][03:01:29][INFO][app_lib] [Payment] ok").unwrap();
        assert_eq!(t.to_string(), "2026-10-31 03:01:29");
        assert!(line_time("continuation line").is_none());
    }

    #[test]
    fn trim_keeps_recent_lines_and_their_continuations() {
        let dir = std::env::temp_dir().join(format!("autopay-logret-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("autopay.log");
        std::fs::write(&p, "[2020-01-01][00:00:00][INFO][app_lib] old\nold tail\n[2030-01-01][00:00:00][INFO][app_lib] new\nnew tail\n").unwrap();
        trim_file(&p, NaiveDateTime::parse_from_str("2025-01-01 00:00:00", "%Y-%m-%d %H:%M:%S").unwrap());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "[2030-01-01][00:00:00][INFO][app_lib] new\nnew tail\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
