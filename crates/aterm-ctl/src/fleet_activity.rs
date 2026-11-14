// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fleet activity record's file: the rows of [`aterm_types::fleet_activity`], one line each,
//! in `<state>/fleet-activity/rows`.
//!
//! Every process that sees a request cross between this Mac and another writes it here: the node
//! job and a window (what came in, admitted or refused by this Mac's fleet access) and `aterm
//! ctl` (what went out, and what the other Mac's lock said). They share the file under its lock
//! ([`aterm_uds::line_log`]: a sibling `rows.lock`, waited for at most 250 ms), so two writers
//! never tear a row and a rewrite never loses one written beside it.
//!
//! A write only ever adds lines or replaces the whole file at once (a temporary file renamed
//! over it), so a write that fails or a writer that dies leaves the record as it was, short at
//! most of the rows being written: no row already on disk is lost to a later one.
//!
//! The record is bounded by size and by age. A write that would leave the file past
//! [`MAX_BYTES`] replaces it with the same requests counted by the minute
//! ([`aterm_types::fleet_activity::folded`]: a refusal with the session that asked), none older
//! than [`MAX_AGE_MS`]; only where those still outgrow [`KEPT_BYTES`] do the oldest go, and the
//! file's first line then says up to when ([`Kept::cut_ms`]), so a reader names the span it
//! still covers. A read passes over any row older than [`MAX_AGE_MS`] and shows a minute's
//! admitted reads of one Mac as one counted row.
//!
//! A row stamped after the clock (the clock was set back) reads as written now
//! ([`aterm_types::fleet_activity::settled`]), and the next write restamps it on disk, so it
//! ages like any other.
//!
//! Writing is best effort and is never on a request's way: [`Record::note`] drops a row it
//! cannot write (no folder, a full disk, a lock another writer holds too long), and every
//! process hands its rows to its one writer thread ([`note_later`]), so the answer to a request
//! never waits for the file. A client that exits once its answer is out waits a moment for that
//! thread first ([`drain`]).

use std::io;
use std::path::{Path, PathBuf};

use aterm_types::fleet_activity::{AHEAD_MS, DIR, ROWS, Row, folded, settled};
use aterm_uds::line_log;

/// The size a write does not leave the record past: it replaces it, counted by the minute.
pub const MAX_BYTES: u64 = 512 * 1024;

/// The most a replaced record holds: the newest of its counted rows that fit this many bytes.
pub const KEPT_BYTES: u64 = MAX_BYTES / 2;

/// The age past which a row is dropped by a rewrite and passed over by a read: a week.
pub const MAX_AGE_MS: u64 = 7 * 24 * 3_600_000;

/// The file beside the rows that holds when the menu-bar item's menu was last opened
/// ([`Record::note_menu_seen`]).
const MENU_SEEN: &str = "menu-seen";

/// The word that leads the record's first line once rows were dropped for its size, before the
/// time (Unix milliseconds) of the newest one dropped.
const CUT: &str = "cut=";

/// How far back from the record's end its last row is looked for: longer than any row's line.
const LAST_ROW: u64 = 2048;

/// How long a client that is about to exit waits for its writer thread ([`drain`]): the lock's
/// own wait and a replacement of the whole file, with room.
const DRAIN: std::time::Duration = std::time::Duration::from_secs(1);

/// The rows a process's writer thread holds unwritten at most ([`note_later`]); a row past
/// them is dropped.
const QUEUE_ROWS: usize = 1024;

/// One Mac's fleet activity record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    rows: PathBuf,
}

/// Which file a record's rows were, how long and when last written, as a reader last read it
/// ([`Record::read_new`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Seen {
    file: line_log::Identity,
    modified: Option<std::time::SystemTime>,
}

impl Record {
    /// The record of the state folder `state`.
    #[must_use]
    pub fn in_state(state: &Path) -> Self {
        Self {
            rows: state.join(DIR).join(ROWS),
        }
    }

    /// This Mac's record, where it names a state folder.
    #[must_use]
    pub fn here() -> Option<Self> {
        aterm_types::dirs::state_dir().map(|state| Self::in_state(&state))
    }

    /// The rows' file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.rows
    }

    /// Write `row`, or drop it where it cannot be written: nothing is said and nothing fails.
    pub fn note(&self, row: &Row) {
        let _ = self.append(std::slice::from_ref(row));
    }

    /// Write `rows` at the record's end under one hold of its lock, in the order given. Where
    /// they would leave it past [`MAX_BYTES`], or its last row is stamped after them (the clock
    /// was set back), the record is replaced whole with them in it ([`replaced`]). The record's
    /// folder is made where it is missing.
    ///
    /// # Errors
    /// The folder could not be made, the lock was not taken in time, or a read, write or
    /// replacement of the file failed. The record is then as it was, or holds some of `rows`.
    pub fn append(&self, rows: &[Row]) -> io::Result<()> {
        let Some(now_ms) = rows.iter().map(|row| row.at_ms).max() else {
            return Ok(());
        };
        if let Some(dir) = self.rows.parent() {
            private_dir(dir)?;
        }
        let mut held = line_log::hold(&self.rows)?;
        let len = held.identity()?.len;
        let lines: Vec<String> = rows.iter().map(Row::line).collect();
        let added: u64 = lines.iter().map(|line| line.len() as u64 + 1).sum();
        if len + added <= MAX_BYTES && !ends_ahead(&mut held, len, now_ms)? {
            let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
            return held.append(&lines).map(|_| ());
        }
        let bytes = held.read_from(0, len)?;
        let mut text = String::from_utf8_lossy(&bytes).into_owned();
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        for line in &lines {
            text.push_str(line);
            text.push('\n');
        }
        let kept = replaced(&text, now_ms);
        aterm_uds::atomic_file::write_flushed(&self.rows, kept.as_bytes(), 0o600, None)
    }

    /// The record at `now_ms`: every row no older than [`MAX_AGE_MS`], oldest first, a minute's
    /// admitted reads of one Mac as one row. A record that is missing or cannot be read has
    /// none; a line that is no row is passed over.
    #[must_use]
    pub fn read(&self, now_ms: u64) -> Kept {
        self.read_text().map_or_else(Kept::default, |text| Kept {
            rows: folded(rows_of(&text, now_ms), false),
            cut_ms: cut_of(&text, now_ms),
        })
    }

    /// [`Self::read`] where the file is not as `seen` names it (replaced, another length, or
    /// written since), leaving `seen` naming it as read; `None` where nothing was written, so a
    /// reader that polls keeps what it has. A row's age is not a change: a reader that shows a
    /// window summarizes the rows it holds again as the clock moves.
    #[must_use]
    pub fn read_new(&self, seen: &mut Option<Seen>, now_ms: u64) -> Option<Kept> {
        let now = std::fs::symlink_metadata(&self.rows).ok().map(|meta| Seen {
            file: line_log::identity_of(&meta),
            modified: meta.modified().ok(),
        });
        if now == *seen {
            return None;
        }
        let kept = self.read(now_ms);
        *seen = now;
        Some(kept)
    }

    /// When this Mac's person last opened the menu-bar item's menu, in Unix milliseconds, as
    /// [`Self::note_menu_seen`] last wrote it; `None` where it never did or the file cannot be
    /// read.
    #[must_use]
    pub fn menu_seen(&self) -> Option<u64> {
        let (bytes, _) = aterm_uds::atomic_file::read_regular(&self.menu_seen_path(), 64).ok()?;
        let text = std::str::from_utf8(&bytes).ok()?.trim();
        (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
            .then(|| text.parse().ok())
            .flatten()
    }

    /// Keep that this Mac's person opened the menu-bar item's menu at `at_ms`, beside the rows:
    /// the item's blocked mark shows for a refusal later than this, so a restart of the app
    /// does not show a refusal its person has already looked at again.
    ///
    /// # Errors
    /// The folder could not be made or the file could not be replaced.
    pub fn note_menu_seen(&self, at_ms: u64) -> io::Result<()> {
        let path = self.menu_seen_path();
        if let Some(dir) = path.parent() {
            private_dir(dir)?;
        }
        aterm_uds::atomic_file::write_flushed(&path, format!("{at_ms}\n").as_bytes(), 0o600, None)
    }

    fn menu_seen_path(&self) -> PathBuf {
        self.rows.with_file_name(MENU_SEEN)
    }

    /// The file's text. No lock is taken: a writer only adds whole lines or replaces the file
    /// at once, so what is read is the record as it stood, short at most of a line being
    /// written, which is passed over and read at the next look.
    fn read_text(&self) -> Option<String> {
        let (bytes, _) = aterm_uds::atomic_file::read_regular(&self.rows, 2 * MAX_BYTES).ok()?;
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }
}

/// The record as one read finds it ([`Record::read`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Kept {
    /// Its rows, oldest first.
    pub rows: Vec<Row>,
    /// Up to when rows were dropped to keep the record within its size (Unix milliseconds):
    /// the rows since then are whole. `None` where none was, or that was over [`MAX_AGE_MS`]
    /// ago.
    pub cut_ms: Option<u64>,
}

/// `text`'s rows no older than [`MAX_AGE_MS`] at `now_ms`, oldest first, a stamp from before
/// the clock was set back read as `now_ms` ([`settled`]).
fn rows_of(text: &str, now_ms: u64) -> Vec<Row> {
    let mut rows: Vec<Row> = text.lines().filter_map(Row::parse).collect();
    settled(&mut rows, now_ms);
    rows.retain(|row| now_ms.saturating_sub(row.at_ms) <= MAX_AGE_MS);
    rows.sort_by_key(|row| row.at_ms);
    rows
}

/// Up to when `text` says rows were dropped ([`CUT`]), where that is no longer than
/// [`MAX_AGE_MS`] before `now_ms` and not after it.
fn cut_of(text: &str, now_ms: u64) -> Option<u64> {
    text.lines()
        .filter_map(|line| line.strip_prefix(CUT)?.parse::<u64>().ok())
        .max()
        .filter(|cut| *cut <= now_ms && now_ms - cut <= MAX_AGE_MS)
}

/// Whether the record's last row (`len` the record's length) is stamped more than
/// [`AHEAD_MS`] after `now_ms`: this Mac's clock was set back since it was written.
fn ends_ahead(held: &mut line_log::Held, len: u64, now_ms: u64) -> io::Result<bool> {
    let bytes = held.read_from(len.saturating_sub(LAST_ROW), LAST_ROW)?;
    let text = String::from_utf8_lossy(&bytes);
    Ok(text
        .lines()
        .next_back()
        .and_then(Row::parse)
        .is_some_and(|row| row.at_ms > now_ms.saturating_add(AHEAD_MS)))
}

/// The record's `text` as a write at `now_ms` replaces it: the same requests, each minute's
/// like ones as one counted row ([`folded`]), none older than [`MAX_AGE_MS`], a stamp from
/// before the clock was set back restamped `now_ms`; and where those still outgrow
/// [`KEPT_BYTES`], the newest that fit under a first line that says up to when the rest went
/// ([`CUT`]), which an earlier replacement's line carries on.
fn replaced(text: &str, now_ms: u64) -> String {
    let rows = folded(rows_of(text, now_ms), true);
    let mut kept = 0u64;
    let from = rows
        .iter()
        .rposition(|row| {
            kept += row.line().len() as u64 + 1;
            kept > KEPT_BYTES
        })
        .map_or(0, |at| at + 1);
    let cut = cut_of(text, now_ms).max(from.checked_sub(1).map(|last| rows[last].at_ms));
    let mut out = cut.map_or_else(String::new, |cut| format!("{CUT}{cut}\n"));
    for row in &rows[from..] {
        out.push_str(&row.line());
        out.push('\n');
    }
    out
}

/// Make `dir` for this account alone where it is missing.
fn private_dir(dir: &Path) -> io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    #[cfg(unix)]
    let made = {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new().mode(0o700).create(dir)
    };
    #[cfg(not(unix))]
    let made = std::fs::create_dir(dir);
    match made {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => Err(e),
        _ => Ok(()),
    }
}

/// Now, in Unix milliseconds on this Mac's clock: a row's `at`.
#[must_use]
pub fn now_ms() -> u64 {
    crate::board::now_ms()
}

#[cfg(test)]
thread_local! {
    /// The rows [`note_here`] was handed on this test thread, which writes none.
    pub(crate) static NOTED: std::cell::RefCell<Vec<Row>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Hand `rows` to this Mac's record: what a client that ran a request calls with what it
/// asked. They go to the writer thread ([`note_later`]) as one write, so the client's answer
/// never waits for the file, and the client waits for the thread only once its answer is out
/// ([`drain`]). A unit test's rows stay on its thread.
pub(crate) fn note_here(rows: Vec<Row>) {
    #[cfg(test)]
    NOTED.with(|noted| noted.borrow_mut().extend(rows));
    #[cfg(not(test))]
    if let Some(state) = aterm_types::dirs::state_dir() {
        note_later(&state, rows);
    }
}

/// What this process's writer thread is handed.
enum Job {
    /// Rows for the record of a state folder, written as one.
    Rows(PathBuf, Vec<Row>),
    /// Answer once every row handed over before this is written or dropped.
    Settle(std::sync::mpsc::Sender<()>),
}

static WRITER: std::sync::OnceLock<Option<std::sync::mpsc::SyncSender<Job>>> =
    std::sync::OnceLock::new();

fn writer() -> Option<&'static std::sync::mpsc::SyncSender<Job>> {
    WRITER
        .get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::sync_channel::<Job>(QUEUE_ROWS);
            std::thread::Builder::new()
                .name("aterm-fleet-activity".to_owned())
                .spawn(move || {
                    for job in rx {
                        match job {
                            Job::Rows(state, rows) => {
                                let _ = Record::in_state(&state).append(&rows);
                            }
                            Job::Settle(done) => {
                                let _ = done.send(());
                            }
                        }
                    }
                })
                .ok()
                .map(|_| tx)
        })
        .as_ref()
}

/// Hand `rows` to this process's writer thread, which writes them to the record of `state`
/// under one hold of its lock ([`Record::append`]): no answer waits for a file. It returns at
/// once; the rows are dropped where the thread could not start or holds [`QUEUE_ROWS`] writes
/// unwritten.
pub fn note_later(state: &Path, rows: Vec<Row>) {
    if !rows.is_empty()
        && let Some(writer) = writer()
    {
        let _ = writer.try_send(Job::Rows(state.to_path_buf(), rows));
    }
}

/// Wait until every row handed to [`note_later`] before this call is written or dropped: a
/// test's read of the record a served request wrote.
#[cfg(any(test, debug_assertions))]
pub fn settle() {
    let (done, settled) = std::sync::mpsc::channel();
    if let Some(writer) = writer()
        && writer.send(Job::Settle(done)).is_ok()
    {
        let _ = settled.recv();
    }
}

/// A CLIENT'S ANSWER IS OUT AND IT IS ABOUT TO EXIT: give the writer thread at most [`DRAIN`]
/// to write the rows handed to it, where any were. A process that noted nothing started no
/// thread and waits for none; one whose record is slow or locked exits without the rows.
pub fn drain() {
    let Some(Some(writer)) = WRITER.get() else {
        return;
    };
    let (done, drained) = std::sync::mpsc::channel();
    if writer.try_send(Job::Settle(done)).is_ok() {
        let _ = drained.recv_timeout(DRAIN);
    }
}

#[cfg(test)]
#[path = "fleet_activity_tests.rs"]
mod tests;
