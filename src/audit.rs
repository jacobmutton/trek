use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

#[derive(Serialize)]
struct Entry<'a> {
    ts: u64,
    argv: &'a [String],
    exit: u8,
    command: &'a str,
}

/// Best-effort append of one JSON line. Audit failures must never mask the
/// user-visible command result, so io errors here are swallowed.
pub fn record(path: &Path, command: &str, argv: &[String], exit: u8) {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let entry = Entry { ts, argv, exit, command };
    let Ok(line) = serde_json::to_string(&entry) else {
        return;
    };
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let _ = writeln!(f, "{line}");
}
