// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Lane transcripts, table formatting, and deduplicated journal entries.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::{Error, Result};

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn unique_tmp(path: &Path) -> PathBuf {
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp.{}.{seq}", std::process::id()));
    PathBuf::from(tmp)
}

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

/// Slice `bytes` down to at most `keep` trailing bytes, advancing to the start of the first whole
/// line when one fits inside the kept tail (or else to the first UTF-8 codepoint boundary).
fn trim_tail(bytes: &[u8], keep: usize) -> &[u8] {
    if bytes.len() <= keep {
        return bytes;
    }
    let tail = &bytes[bytes.len() - keep..];
    if let Some(nl) = tail.iter().position(|&b| b == b'\n')
        && nl + 1 < tail.len()
    {
        return &tail[nl + 1..];
    }
    let mut start = 0;
    while start < tail.len() && start < 4 && (tail[start] & 0b1100_0000) == 0b1000_0000 {
        start += 1;
    }
    &tail[start..]
}

fn write_staged_new(tmp: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp)?;
    file.write_all(bytes)
}

fn trim_log(path: &Path) {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let Ok((mut file, meta)) = crate::identity::open_regular_nonblock(path) else {
        return;
    };
    if meta.len() <= LOG_MAX {
        return;
    }
    let window = LOG_KEEP.saturating_add(1);
    let offset = meta.len().saturating_sub(window);
    if file.seek(SeekFrom::Start(offset)).is_err() {
        return;
    }
    let want = (meta.len() - offset).min(window);
    let mut bytes = Vec::with_capacity(usize::try_from(want.saturating_add(1)).unwrap_or(0));
    if file.take(window).read_to_end(&mut bytes).is_err() {
        return;
    }
    let keep = usize::try_from(LOG_KEEP).unwrap_or(usize::MAX);
    let tail = trim_tail(&bytes, keep);
    let tmp = unique_tmp(path);
    let _ = std::fs::remove_file(&tmp);
    if write_staged_new(&tmp, tail).is_err() || std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Records one publication notice.
pub trait NoticeSink {
    /// Record `title`: `message` without asking for attention.
    fn record(&self, title: &str, message: &str);
}

/// Appends publication notices to the lane's existing transcript.
#[derive(Clone, Copy, Debug)]
pub struct Journal<'a>(pub &'a Transcript);

impl NoticeSink for Journal<'_> {
    fn record(&self, title: &str, message: &str) {
        self.0.say(format!("{title}: {message}"));
    }
}

/// A journal sink that records in memory (tests).
#[derive(Debug, Default)]
pub struct Recorded(Mutex<Vec<String>>);

impl Recorded {
    /// Every message recorded, in order.
    #[must_use]
    pub fn entries(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl NoticeSink for Recorded {
    fn record(&self, _title: &str, message: &str) {
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

/// Record `message` unless it was already recorded on this UTC day.
pub fn record_once(
    notifier: &dyn NoticeSink,
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
    let seen = crate::identity::read_regular_text(&file, crate::identity::MAX_META_BYTES)
        .unwrap_or_default();
    if seen.lines().any(|l| l == message) {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("notified-")
                && *name != *format!("notified-{day}")
                && e.file_type().is_ok_and(|ft| ft.is_file())
            {
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
    notifier.record(title, &message);
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
    write_atomic_bytes(path, text.as_bytes())
}

/// Write `bytes` to `path` through a temp file and a rename, so a reader never sees half
/// and an existing symlink at `path` is replaced rather than followed.
///
/// # Errors
///
/// A could-not-run naming the file.
pub fn write_atomic_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::io(format!("mkdir {}", parent.display()), &e))?;
    }
    let tmp = unique_tmp(path);
    let _ = std::fs::remove_file(&tmp);
    write_staged_new(&tmp, bytes).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        Error::io(format!("write {}", tmp.display()), &e)
    })?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        Error::io(format!("rename {} into place", path.display()), &e)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// NEGATIVE CONTROL: `trim_tail` aligns on a whole line (or UTF-8 codepoint boundary when a
    /// single line spans the kept window) instead of splitting multi-byte UTF-8 characters like
    /// `—` (`0xE2 0x80 0x94`) into invalid UTF-8 or leaving a torn leading fragment.
    #[test]
    fn trim_tail_preserves_whole_lines_and_utf8_boundaries() {
        let log = "lane: WARNING — first line\nlane: WARNING — second line\n";
        let trimmed = trim_tail(log.as_bytes(), 36);
        assert_eq!(
            std::str::from_utf8(trimmed).expect("valid UTF-8"),
            "lane: WARNING — second line\n"
        );

        let single_line = "——————end\n";
        // Cut 1 byte into the multi-byte `—` sequence with only a trailing newline in `tail`.
        let kept = single_line.len() - 1;
        let single_trimmed = trim_tail(single_line.as_bytes(), kept);
        assert_eq!(
            std::str::from_utf8(single_trimmed).expect("valid UTF-8 without mid-codepoint tear"),
            "—————end\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_bytes_replaces_symlink_target_path_without_following() {
        let root = aterm_tempfile::Builder::new()
            .prefix("atpkg-report-atomic-")
            .tempdir()
            .expect("scratch dir");
        let outside = root.path().join("outside.txt");
        std::fs::write(&outside, b"keep-me").expect("write outside");
        let dest = root.path().join("state.txt");
        std::os::unix::fs::symlink(&outside, &dest).expect("symlink dest");

        write_atomic(&dest, "fresh\n").expect("atomic write");
        assert!(
            std::fs::symlink_metadata(&dest)
                .expect("dest meta")
                .file_type()
                .is_file()
        );
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "fresh\n");
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep-me");
    }
}
