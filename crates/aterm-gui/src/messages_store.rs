// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `<log_dir>/messages.log`, the durable half of the unified message system (docs/DESIGN-unified-messages-2026-09-21.md §3.7).

use std::collections::VecDeque;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};

use crate::qos;

use aterm_messages::log::MAX_LINE_BYTES;
use aterm_messages::{LogLine, MessageLog, Shelf};

use crate::logging::{RotatingFile, RotationBudget};

/// The file's name under the log directory.
pub(crate) const FILE_NAME: &str = "messages.log";
/// How much of the file's tail a launch reads.
pub(crate) const TAIL_BYTES: u64 = 1024 * 1024;
/// The file's rotation.
pub(crate) const BUDGET: crate::logging::RotationBudget = crate::logging::RotationBudget {
    rotate_at: TAIL_BYTES,
    check_bytes: 64 * 1024,
    check_every: std::time::Duration::from_secs(30),
};
/// The writer channel's depth.
pub(crate) const QUEUE_CAP: usize = 512;
/// The wire's file, beside [`FILE_NAME`].
pub(crate) const WIRE_FILE_NAME: &str = "messages.wire.log";
/// The wire's file rotates past this, and a launch reads this much of its tail.
pub(crate) const WIRE_TAIL_BYTES: u64 = 256 * 1024;
/// The wire file's rotation.
pub(crate) const WIRE_BUDGET: RotationBudget = RotationBudget {
    rotate_at: WIRE_TAIL_BYTES,
    ..BUDGET
};

/// `<log_dir>/messages.log`.
pub(crate) fn path() -> Option<PathBuf> {
    crate::logging::log_dir().map(|dir| dir.join(FILE_NAME))
}

/// The wire's file beside the log at `path`.
pub(crate) fn wire_path(path: &Path) -> PathBuf {
    path.with_file_name(WIRE_FILE_NAME)
}

/// What a launch read.
#[derive(Debug)]
pub(crate) struct Loaded {
    /// The records the tail held, replayed (a `Posted` with no `Retired` from the dead process reads as Stale, the engine's rule).
    pub(crate) log: MessageLog,
    /// Lines the codec refused (torn, foreign, corrupt).
    pub(crate) skipped: usize,
}

/// The launch load.
pub(crate) fn load_for_launch(path: &Path) -> Loaded {
    let loaded = load_tail(path, TAIL_BYTES);
    if loaded.skipped > 0 {
        aterm_log::warn!(
            "messages log: skipped {} undecodable line(s) in {}",
            loaded.skipped,
            path.display()
        );
    }
    loaded
}

/// A bounded, no-follow read of the last `tail` bytes of the log across its rotation.
pub(crate) fn load_tail(path: &Path, tail: u64) -> Loaded {
    let (lines, skipped) = tail_lines(path, tail);
    let mut log = MessageLog::empty();
    log.replay_all(lines);
    Loaded { log, skipped }
}

/// The decoded lines of the last `tail` bytes across the pair (and the wire's pair), in replay order, and how many the codec refused.
pub(crate) fn tail_lines(path: &Path, tail: u64) -> (Vec<LogLine>, usize) {
    let mut lines = Vec::new();
    let mut skipped = read_pair(path, tail, &mut lines);
    skipped += read_pair(&wire_path(path), tail.min(WIRE_TAIL_BYTES), &mut lines);
    lines.sort_by_key(LogLine::id);
    (lines, skipped)
}

/// The last `tail` bytes across `path` and its `.1`, decoded into `out`
/// oldest first; the count of lines the codec refused.
fn read_pair(path: &Path, tail: u64, out: &mut Vec<LogLine>) -> usize {
    let mut skipped = 0;
    let current = regular_len(path);
    let older_budget = tail.saturating_sub(current.unwrap_or(0));
    if older_budget > 0 {
        let older = crate::logging::rotated_path(path);
        if let Some(text) = read_tail(&older, older_budget) {
            skipped += decode_into(out, &text);
        }
    }
    if current.is_some()
        && let Some(text) = read_tail(path, tail)
    {
        skipped += decode_into(out, &text);
    }
    skipped
}

/// The length of `path` when it is a regular file (never followed).
fn regular_len(path: &Path) -> Option<u64> {
    std::fs::symlink_metadata(path)
        .ok()
        .filter(|meta| meta.file_type().is_file())
        .map(|meta| meta.len())
}

/// The last `tail` bytes of the regular file `path` as text, the partial first
/// line of a cut dropped; `None` when it is not a regular file or unreadable.
fn read_tail(path: &Path, tail: u64) -> Option<String> {
    let (mut file, meta) = crate::logging::open_regular_nonblock_with_meta(path).ok()?;
    let bytes = meta.len();
    let partial = bytes > tail;
    if partial {
        file.seek(SeekFrom::Start(bytes - tail)).ok()?;
    }
    let cap = usize::try_from(tail.min(bytes).saturating_add(1)).unwrap_or(0);
    let mut buf = Vec::with_capacity(cap);
    (&mut file).take(tail).read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    Some(if partial {
        text.split_once('\n')
            .map_or_else(String::new, |(_, rest)| rest.to_string())
    } else {
        text
    })
}

/// Decode every whole line of `text` into `out`; the count of lines the
/// codec refused.
fn decode_into(out: &mut Vec<LogLine>, text: &str) -> usize {
    let mut skipped = 0;
    for line in text.split('\n').filter(|l| !l.is_empty()) {
        match LogLine::decode(line) {
            Ok(decoded) => out.push(decoded),
            Err(_) => skipped += 1,
        }
    }
    skipped
}

/// One line as it lands on disk: the codec's form plus the newline. A line
/// past [`MAX_LINE_BYTES`] would never read back, so it is not written; the
/// caps make that unreachable for a message the engine admitted.
fn encoded(line: &LogLine) -> String {
    let mut s = line.encode();
    if s.len() >= MAX_LINE_BYTES {
        return String::new();
    }
    s.push('\n');
    s
}

/// The two appenders behind the one lock: aterm's own file, and the wire's, opened at
/// its first line.
struct Files {
    host: RotatingFile,
    wire: WireFile,
}

/// The wire's file: not yet needed, open, or refused (its lines then go to
/// the host's file — the log never drops).
enum WireFile {
    Unopened(PathBuf, RotationBudget),
    Open(RotatingFile),
    Refused,
}

impl Files {
    /// The appender a `shelf` line lands in, opening the wire's on first use.
    fn for_shelf(&mut self, shelf: Shelf) -> &mut RotatingFile {
        if shelf == Shelf::Wire
            && let WireFile::Unopened(path, budget) = &self.wire
        {
            self.wire = match RotatingFile::open(path.clone(), *budget) {
                Ok(file) => WireFile::Open(file),
                Err(error) => {
                    aterm_log::warn!(
                        "messages log: {} not opened ({error}); the wire's lines go to {}",
                        path.display(),
                        FILE_NAME
                    );
                    WireFile::Refused
                }
            };
        }
        match (shelf, &mut self.wire) {
            (Shelf::Wire, WireFile::Open(file)) => file,
            _ => &mut self.host,
        }
    }
}

/// Append one line under the file lock, rotating first when a look is due
/// ([`BUDGET`], [`WIRE_BUDGET`]).
fn write_line(files: &Mutex<Files>, line: &str, shelf: Shelf) {
    if line.is_empty() {
        return;
    }
    let mut files = files
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    files
        .for_shelf(shelf)
        .append(line.as_bytes(), |_| String::new());
}

/// What the caller hands the writer thread: a line to append, or a flush
/// mark the thread answers once every line queued before it is on disk —
/// raised by the flush's [`qos::Boost`] until then.
enum Job {
    Line(String, Shelf),
    Flush(mpsc::SyncSender<()>, qos::Boost),
}

/// The appender.
pub(crate) struct Writer {
    /// `None` only during drop, so the receiver sees the channel close.
    tx: Option<mpsc::SyncSender<Job>>,
    /// Lines the channel had no room for, oldest first: handed over ahead of
    /// every later line ([`Self::hand_over`]).
    overflow: Arc<Mutex<VecDeque<(String, Shelf)>>>,
    overflowed: Arc<AtomicUsize>,
    files: Arc<Mutex<Files>>,
    lazy_rx: Mutex<Option<mpsc::Receiver<Job>>>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// The folder the file lives in: the log dir.
    folder: Option<PathBuf>,
}

impl Writer {
    /// Open `path` for appending (`0600`) and start the writer thread.
    pub(crate) fn spawn(path: &Path) -> Option<Writer> {
        Self::spawn_with(path, BUDGET, WIRE_BUDGET)
    }

    /// [`Self::spawn`] under `budget` (and `wire_budget` for the wire's file).
    pub(crate) fn spawn_with(
        path: &Path,
        budget: RotationBudget,
        wire_budget: RotationBudget,
    ) -> Option<Writer> {
        let (writer, rx) = match Self::open(path, QUEUE_CAP, budget, wire_budget) {
            Ok(pair) => pair,
            Err(error) => {
                aterm_log::warn!("messages log: {} not opened ({error})", path.display());
                return None;
            }
        };
        *writer
            .lazy_rx
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(rx);
        Some(writer)
    }

    fn ensure_thread(&self) {
        let rx = self
            .lazy_rx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some(rx) = rx else {
            return;
        };
        let files = Arc::clone(&self.files);
        let overflow = Arc::clone(&self.overflow);
        let overflowed = Arc::clone(&self.overflowed);
        let spawned =
            qos::spawn_lane_thread("aterm-messages-log", qos::Role::Responsive, move || {
                loop {
                    let job = match rx.try_recv() {
                        Ok(job) => job,
                        Err(_) => {
                            let next = {
                                let mut queued =
                                    overflow.lock().unwrap_or_else(PoisonError::into_inner);
                                match rx.try_recv() {
                                    Ok(job) => Some(job),
                                    Err(_) => {
                                        let line = queued
                                            .pop_front()
                                            .map(|(line, shelf)| Job::Line(line, shelf));
                                        overflowed.store(queued.len(), Ordering::Relaxed);
                                        line
                                    }
                                }
                            };
                            match next {
                                Some(job) => job,
                                None => match rx.recv() {
                                    Ok(job) => job,
                                    Err(_) => break,
                                },
                            }
                        }
                    };
                    match job {
                        Job::Line(line, shelf) => write_line(&files, &line, shelf),
                        Job::Flush(done, _boost) => {
                            let _ = done.send(());
                        }
                    }
                }
            });
        match spawned {
            Ok(handle) => {
                *self.thread.lock().unwrap_or_else(PoisonError::into_inner) = Some(handle);
            }
            Err(error) => {
                aterm_log::warn!("messages log: writer thread not spawned ({error})");
            }
        }
    }

    /// The handle and its receiver, no thread yet.
    fn open(
        path: &Path,
        cap: usize,
        budget: RotationBudget,
        wire_budget: RotationBudget,
    ) -> std::io::Result<(Writer, mpsc::Receiver<Job>)> {
        let host = RotatingFile::open(path.to_path_buf(), budget)?;
        let wire = WireFile::Unopened(wire_path(path), wire_budget);
        let (tx, rx) = mpsc::sync_channel::<Job>(cap);
        Ok((
            Writer {
                tx: Some(tx),
                overflow: Arc::new(Mutex::new(VecDeque::new())),
                overflowed: Arc::new(AtomicUsize::new(0)),
                files: Arc::new(Mutex::new(Files { host, wire })),
                lazy_rx: Mutex::new(None),
                thread: Mutex::new(None),
                folder: path.parent().map(Path::to_path_buf),
            },
            rx,
        ))
    }

    /// The folder the log lives in (read once, at open — never a directory probe per publish).
    pub(crate) fn folder(&self) -> Option<&Path> {
        self.folder.as_deref()
    }

    /// A writer with no thread — the test's way of standing in for a dead
    /// one deterministically (drop the receiver and every send is
    /// `Disconnected`).
    #[cfg(test)]
    fn unattended(path: &Path, cap: usize) -> (Writer, mpsc::Receiver<Job>) {
        Self::open(path, cap, BUDGET, WIRE_BUDGET).expect("a temp file opens")
    }

    /// A writer whose thread never answers.
    #[cfg(test)]
    pub(crate) fn stalled(path: &Path) -> (Writer, Box<dyn std::any::Any>) {
        let (writer, rx) = Self::unattended(path, 1);
        (writer, Box::new(rx))
    }

    /// Append `lines`, oldest first, in that order on disk, each to its shelf's file.
    pub(crate) fn append(&self, lines: &[(LogLine, Shelf)]) {
        if lines.is_empty() && self.overflowed.load(Ordering::Relaxed) == 0 {
            return;
        }
        self.ensure_thread();
        let mut queued = self.overflow.lock().unwrap_or_else(PoisonError::into_inner);
        queued.extend(lines.iter().filter_map(|(line, shelf)| {
            let encoded = encoded(line);
            (!encoded.is_empty()).then_some((encoded, *shelf))
        }));
        self.hand_over(&mut queued);
    }

    /// Hand `queued` to the thread, oldest first, until the channel is full;
    /// what is left waits there, in order.
    fn hand_over(&self, queued: &mut VecDeque<(String, Shelf)>) {
        while let Some((line, shelf)) = queued.pop_front() {
            let Some(tx) = &self.tx else {
                write_line(&self.files, &line, shelf);
                continue;
            };
            match tx.try_send(Job::Line(line, shelf)) {
                Ok(()) => {}
                Err(TrySendError::Full(job)) => {
                    if let Job::Line(line, shelf) = job {
                        queued.push_front((line, shelf));
                    }
                    break;
                }
                Err(TrySendError::Disconnected(job)) => {
                    if let Job::Line(line, shelf) = job {
                        write_line(&self.files, &line, shelf);
                    }
                }
            }
        }
        self.overflowed.store(queued.len(), Ordering::Relaxed);
    }

    /// Wait until every line appended so far is on disk, a minute at most.
    #[cfg(test)]
    pub(crate) fn flush(&self) {
        assert!(
            self.flush_within(std::time::Duration::from_secs(60)),
            "the messages writer landed nothing within a minute"
        );
    }

    /// Wait no longer than `within` for every line appended so far to land.
    pub(crate) fn flush_within(&self, within: std::time::Duration) -> bool {
        let now = std::time::Instant::now();
        let deadline = now
            .checked_add(within)
            .unwrap_or_else(|| now + std::time::Duration::from_secs(365 * 24 * 3600));
        let Some(tx) = &self.tx else {
            let mut queued = self.overflow.lock().unwrap_or_else(PoisonError::into_inner);
            self.hand_over(&mut queued);
            return true;
        };
        if self
            .lazy_rx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
            && self.overflowed.load(Ordering::Relaxed) == 0
        {
            return true;
        }
        self.ensure_thread();
        let (done, landed) = mpsc::sync_channel::<()>(1);
        let boost = qos::Boost::start(
            self.thread
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref(),
        );
        let mut job = Job::Flush(done, boost);
        loop {
            {
                let mut queued = self.overflow.lock().unwrap_or_else(PoisonError::into_inner);
                self.hand_over(&mut queued);
                if queued.is_empty() {
                    match tx.try_send(job) {
                        Ok(()) => break,
                        Err(TrySendError::Disconnected(_)) => return true,
                        Err(TrySendError::Full(back)) => job = back,
                    }
                }
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        landed
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .is_ok()
    }
}

impl Drop for Writer {
    /// Hand over what the overflow holds, close the channel, then wait for
    /// the thread to write what it holds: the lines a process queued in its
    /// last moments are its record.
    fn drop(&mut self) {
        let _ = self
            .lazy_rx
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let attended = self
            .thread
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some();
        let queued =
            std::mem::take(&mut *self.overflow.lock().unwrap_or_else(PoisonError::into_inner));
        for (line, shelf) in queued {
            let job = Job::Line(line, shelf);
            let refused = match &self.tx {
                Some(tx) if attended => tx.send(job).err().map(|mpsc::SendError(job)| job),
                Some(tx) => tx.try_send(job).err().map(|error| match error {
                    TrySendError::Full(job) | TrySendError::Disconnected(job) => job,
                }),
                None => Some(job),
            };
            if let Some(Job::Line(line, shelf)) = refused {
                write_line(&self.files, &line, shelf);
            }
        }
        self.tx = None;
        if let Some(thread) = self
            .thread
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
#[path = "messages_store_tests.rs"]
mod tests;
