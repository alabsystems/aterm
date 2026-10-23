// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Lane transcripts, table formatting, and once-per-day notifications.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::error::{Error, Result};

/// Maximum log size before trimming to [`LOG_KEEP`] bytes.
pub const LOG_MAX: u64 = 8 * 1024 * 1024;
/// What a trimmed log keeps.
pub const LOG_KEEP: u64 = 2 * 1024 * 1024;

/// Where a transcript's lines go besides its own record.
#[derive(Debug)]
pub enum Sink {
    /// stdout (a person at a terminal).
    Stdout,
    /// Appended to this log file.
    File(PathBuf),
    /// Nowhere else (tests read [`Transcript::lines`]).
    Memory,
}

/// One run's transcript.
#[derive(Debug)]
pub struct Transcript {
    me: &'static str,
    sink: Sink,
    lines: Mutex<Vec<String>>,
}

impl Transcript {
    /// A transcript prefixed `<me>: `, written to `sink`.
    #[must_use]
    pub fn new(me: &'static str, sink: Sink) -> Self {
        if let Sink::File(path) = &sink {
            trim_log(path);
        }
        Self {
            me,
            sink,
            lines: Mutex::new(Vec::new()),
        }
    }

    /// A transcript that only records.
    #[must_use]
    pub fn memory(me: &'static str) -> Self {
        Self::new(me, Sink::Memory)
    }

    /// Every line said so far.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        self.lines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Everything said so far, one string.
    #[must_use]
    pub fn text(&self) -> String {
        self.lines().join("\n")
    }

    /// A raw line (a banner, a table row).
    pub fn raw(&self, line: impl Into<String>) {
        let line = line.into();
        match &self.sink {
            Sink::Stdout => println!("{line}"),
            Sink::File(path) => {
                let ok = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .and_then(|mut f| writeln!(f, "{line}"));
                if let Err(e) = ok {
                    eprintln!("{}: cannot append to {}: {e}", self.me, path.display());
                }
            }
            Sink::Memory => {}
        }
        self.lines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(line);
    }

    /// `<me>: <what>`.
    pub fn say(&self, what: impl AsRef<str>) {
        self.raw(format!("{}: {}", self.me, what.as_ref()));
    }

    /// `<me>: WARNING — <what>`.
    pub fn warn(&self, what: impl AsRef<str>) {
        self.raw(format!("{}: WARNING — {}", self.me, what.as_ref()));
    }
}

fn trim_log(path: &Path) {
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    if meta.len() <= LOG_MAX {
        return;
    }
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let keep = usize::try_from(LOG_KEEP).unwrap_or(usize::MAX);
    let tail = &bytes[bytes.len().saturating_sub(keep)..];
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    if std::fs::write(&tmp, tail).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// Shows a person one line, out of band.
pub trait Notifier {
    /// Show `title`: `message`. Best effort: a notifier that cannot show it says nothing.
    fn notify(&self, title: &str, message: &str);
}

/// macOS Notification Center through `osascript`; nothing elsewhere.
#[derive(Clone, Copy, Debug, Default)]
pub struct Osascript;

impl Notifier for Osascript {
    fn notify(&self, title: &str, message: &str) {
        if !cfg!(target_os = "macos") {
            return;
        }
        let clean = |s: &str| -> String {
            s.chars()
                .filter(|c| *c != '"' && *c != '\\')
                .take(200)
                .collect()
        };
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            clean(message),
            clean(title)
        );
        let _ = aterm_uds::spawnfd::command(aterm_uds::os_path!("osascript"))
            .arg("-e")
            .arg(script)
            .output();
    }
}

/// A notifier that records (tests).
#[derive(Debug, Default)]
pub struct Recorded(Mutex<Vec<String>>);

impl Recorded {
    /// Every message shown, in order.
    #[must_use]
    pub fn shown(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl Notifier for Recorded {
    fn notify(&self, _title: &str, message: &str) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(message.to_string());
    }
}

/// The UTC day `now_unix` falls on (`YYYYMMDD`).
#[must_use]
pub fn utc_day(now_unix: i64) -> String {
    let secs = u64::try_from(now_unix).unwrap_or(0);
    let stamp = aterm_types::rfc3339::format_rfc3339(secs);
    stamp[..10].replace('-', "")
}

/// Show `message` unless it was already shown on this UTC day.
pub fn notify_once(
    notifier: &dyn Notifier,
    title: &str,
    dir: &Path,
    now_unix: i64,
    dry_run: bool,
    message: &str,
) {
    if dry_run {
        return;
    }
    let message: String = message.replace('\n', " ").chars().take(200).collect();
    let day = utc_day(now_unix);
    let file = dir.join(format!("notified-{day}"));
    let seen = std::fs::read_to_string(&file).unwrap_or_default();
    if seen.lines().any(|l| l == message) {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("notified-") && *name != *format!("notified-{day}") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
        .and_then(|mut f| writeln!(f, "{message}"));
    notifier.notify(title, &message);
}

/// Render rows as aligned columns: two spaces between, the last column unpadded.
#[must_use]
pub fn columns(rows: &[Vec<String>]) -> Vec<String> {
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![0usize; width];
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i + 1 < row.len() {
                widths[i] = widths[i].max(cell.chars().count());
            }
        }
    }
    rows.iter()
        .map(|row| {
            let mut line = String::new();
            for (i, cell) in row.iter().enumerate() {
                if i + 1 < row.len() {
                    line.push_str(cell);
                    let pad = widths[i] - cell.chars().count() + 2;
                    line.extend(std::iter::repeat_n(' ', pad));
                } else {
                    line.push_str(cell);
                }
            }
            line
        })
        .collect()
}

/// Write `text` to `path` through a temp file and a rename, so a reader never sees half.
///
/// # Errors
///
/// A could-not-run naming the file.
pub fn write_atomic(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::io(format!("mkdir {}", parent.display()), &e))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text).map_err(|e| Error::io(format!("write {}", tmp.display()), &e))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| Error::io(format!("rename {} into place", path.display()), &e))
}
