// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE MESSAGE LOG ON DISK — `<log_dir>/messages.log`, the durable half of
//! the unified message system (docs/DESIGN-unified-messages-2026-09-21.md
//! §3.7, D4). The engine encodes lines and keeps the ring
//! (`aterm_messages::log`); this module is the IO it never does: the bounded
//! tail read at launch, the named writer thread every drain appends through,
//! and the rotation that keeps the file near the ring's own size.
//!
//! # Contract
//!
//! * The file lives beside `aterm.log` and the crash artifacts, created
//!   `0600`; always on, never gated by `$ATERM_LOG` — a person opening
//!   Settings ▸ Messages after a crash must find the record whatever their
//!   logging preference was.
//! * **The log never drops, and never reorders.** Appends go through a
//!   bounded channel to the writer thread; when the channel is full the
//!   caller WAITS for a slot (the thread is alive and draining, so the wait
//!   is one write long) — the ux-first design's drop-on-full was rejected
//!   because D4 asks for a complete log, and the first cut's write-it-
//!   yourself fallback was rejected on review (2026-09-22) because it put
//!   the caller's line on disk AHEAD of the lines still queued, and the
//!   engine's replay reads a `Retired` line before its `Posted` as a dead
//!   process's open row. Only a dead thread makes the caller write.
//! * One `write_all` per line, `O_APPEND`. POSIX guarantees atomicity only for
//!   writes ≤ `PIPE_BUF` on pipes, not on regular files, so line integrity
//!   comes from the codec's `end=` terminator — a torn line fails to decode
//!   and the loader skips it — not from the OS. A second aterm instance of
//!   the same user (a `--diagnose` beside the GUI) may interleave whole lines;
//!   the loader keeps both.
//! * **The handoff.** The parent's record is its own to the end: it keeps
//!   appending through a pending handoff and [`Writer::flush`]es — every
//!   queued line on disk — right before it execs the successor (`exec`
//!   runs no destructor, so the drop-time join would never happen). The
//!   successor's drain stays frozen until Commit (`App::sync_messages`), so
//!   a refused successor writes nothing; after Commit the two records meet
//!   as whole lines, which the loader takes in any order.
//! * **Bounded while running** (design ruling 65, the ux/status-reporting
//!   merge). The writer appends through [`crate::logging::RotatingFile`], the
//!   one rotation `aterm.log` already proves (`aterm_spec`'s `LogRotation`
//!   model and its Tier-1 bind): past [`TAIL_BYTES`] the file is renamed to
//!   `messages.log.1` (one older copy), one rotation at a time under the
//!   sibling `messages.log.lock` (the lock a pre-merge build's load-time
//!   compaction takes too, so the two never overlap), and every writer —
//!   this process's, a second instance's, a successor's — follows the rename
//!   by inode within [`BUDGET`]'s look. A launch reads the last
//!   [`TAIL_BYTES`] across the two files ([`load_tail`]). The load-time
//!   compaction this replaced ran only for a cold launch — never for a
//!   seamless successor, which is every automatic update's launch — and its
//!   rename stranded any other running writer's lines in an unlinked inode.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, TrySendError};
use std::sync::{Arc, Mutex};

use crate::qos;

use aterm_messages::log::MAX_LINE_BYTES;
use aterm_messages::{LogLine, MessageLog};

/// The file's name under the log directory.
pub(crate) const FILE_NAME: &str = "messages.log";
/// How much of the file's tail a launch reads. The ring holds `LOG_CAP`
/// records and a record at every cap is under 32 KiB, so the tail is the
/// ring's ceiling, generously; a crash record with a 1.5 KiB head is the
/// largest producer in practice (design open question 9).
pub(crate) const TAIL_BYTES: u64 = 1024 * 1024;
/// The file's rotation: renamed to `messages.log.1` past the [`TAIL_BYTES`] a
/// launch reads, so the pair stays near 2 MiB; every writer looks within
/// 64 KiB or 30 s, as `aterm.log` does — the `LogRotation` model's rate
/// assumption (a file takes longer than one look to fill) holds a
/// thousandfold for a log that grows kilobytes a day.
pub(crate) const BUDGET: crate::logging::RotationBudget = crate::logging::RotationBudget {
    rotate_at: TAIL_BYTES,
    check_bytes: 64 * 1024,
    check_every: std::time::Duration::from_secs(30),
};
/// The writer channel's depth: how many lines may wait for the thread before
/// the caller waits for a slot.
pub(crate) const QUEUE_CAP: usize = 512;

/// `<log_dir>/messages.log`; `None` when there is no log dir (no `$HOME`).
pub(crate) fn path() -> Option<PathBuf> {
    crate::logging::log_dir().map(|dir| dir.join(FILE_NAME))
}

/// What a launch read.
#[derive(Debug)]
pub(crate) struct Loaded {
    /// The records the tail held, replayed (a `Posted` with no `Retired` from
    /// the dead process reads as Stale, the engine's rule).
    pub(crate) log: MessageLog,
    /// Lines the codec refused (torn, foreign, corrupt) — the log line says
    /// how many, and the loader carries on.
    pub(crate) skipped: usize,
}

/// The launch load: the last [`TAIL_BYTES`] of `path`, decoded line by line.
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

/// A bounded, no-follow read of the last `tail` bytes of the log ACROSS ITS
/// ROTATION: when `path` holds fewer than `tail` bytes, the rest comes from the
/// end of `messages.log.1` first — oldest first, so ids keep rising and a
/// `Posted` in the older copy meets its `Retired` in the current file (an
/// orphan `Retired` is ignored by the replay). Each file is read only as a
/// REGULAR file; the partial first line of a cut is dropped, every whole line
/// is offered to the codec, junk is counted and skipped. A symlink, a
/// directory or an unreadable file is nothing — a message log is never worth
/// refusing to launch over.
pub(crate) fn load_tail(path: &Path, tail: u64) -> Loaded {
    let mut log = MessageLog::empty();
    let mut skipped = 0;
    let current = regular_len(path);
    let older_budget = tail.saturating_sub(current.unwrap_or(0));
    if older_budget > 0 {
        let older = crate::logging::rotated_path(path);
        if let Some(text) = read_tail(&older, older_budget) {
            skipped += replay_into(&mut log, &text);
        }
    }
    if current.is_some()
        && let Some(text) = read_tail(path, tail)
    {
        skipped += replay_into(&mut log, &text);
    }
    Loaded { log, skipped }
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
    let bytes = regular_len(path)?;
    let mut file = File::open(path).ok()?;
    let partial = bytes > tail;
    if partial {
        file.seek(SeekFrom::Start(bytes - tail)).ok()?;
    }
    let mut buf = Vec::with_capacity(usize::try_from(tail.min(bytes)).unwrap_or(0));
    (&mut file).take(tail).read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    Some(if partial {
        // The cut landed inside a line: what is left of it is not a record.
        text.split_once('\n')
            .map_or_else(String::new, |(_, rest)| rest.to_string())
    } else {
        text
    })
}

/// Replay every whole line of `text` into `log`; the count of lines the codec
/// refused.
fn replay_into(log: &mut MessageLog, text: &str) -> usize {
    let mut skipped = 0;
    for line in text.split('\n').filter(|l| !l.is_empty()) {
        match LogLine::decode(line) {
            Ok(decoded) => log.replay(decoded),
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

/// Append one line under the file lock, rotating first when a look is due
/// ([`BUDGET`]). No rotation note is written: a note is not a codec line, and
/// the loader would count it as junk. A write the disk refuses is the one path
/// that loses a line.
fn write_line(file: &Mutex<crate::logging::RotatingFile>, line: &str) {
    if line.is_empty() {
        return;
    }
    let mut f = file
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    f.append(line.as_bytes(), |_| String::new());
}

/// What the caller hands the writer thread: a line to append, or a flush
/// mark the thread answers once every line queued before it is on disk.
enum Job {
    Line(String),
    Flush(mpsc::SyncSender<()>),
}

/// The appender: a bounded channel into the `aterm-messages-log` thread,
/// which owns the append handle behind a lock the dead-thread fallback
/// shares.
pub(crate) struct Writer {
    /// `None` only during drop, so the receiver sees the channel close.
    tx: Option<mpsc::SyncSender<Job>>,
    file: Arc<Mutex<crate::logging::RotatingFile>>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// The folder the file lives in — the log dir, where `aterm.log`,
    /// `packages.log` and the crash reports sit beside it.
    folder: Option<PathBuf>,
}

impl Writer {
    /// Open `path` for appending (`0600`) and start the writer thread; `None`
    /// when the file cannot be opened or the thread not spawned (logged —
    /// the app runs, the record does not).
    pub(crate) fn spawn(path: &Path) -> Option<Writer> {
        Self::spawn_with(path, BUDGET)
    }

    /// [`Self::spawn`] under `budget` — the tests' way to rotate a small file.
    pub(crate) fn spawn_with(
        path: &Path,
        budget: crate::logging::RotationBudget,
    ) -> Option<Writer> {
        let (mut writer, rx) = match Self::open(path, QUEUE_CAP, budget) {
            Ok(pair) => pair,
            Err(error) => {
                aterm_log::warn!("messages log: {} not opened ({error})", path.display());
                return None;
            }
        };
        let file = Arc::clone(&writer.file);
        let spawned = std::thread::Builder::new()
            .name("aterm-messages-log".into())
            .spawn(move || {
                // Nobody is blocked on the record landing: below the
                // keystroke path, like every other log writer (`crate::qos`).
                qos::set_self(qos::Role::Background);
                for job in rx {
                    match job {
                        Job::Line(line) => write_line(&file, &line),
                        Job::Flush(done) => {
                            let _ = done.send(());
                        }
                    }
                }
            });
        match spawned {
            Ok(handle) => {
                writer.thread = Some(handle);
                Some(writer)
            }
            Err(error) => {
                aterm_log::warn!("messages log: writer thread not spawned ({error})");
                None
            }
        }
    }

    /// The handle and its receiver, no thread yet.
    fn open(
        path: &Path,
        cap: usize,
        budget: crate::logging::RotationBudget,
    ) -> std::io::Result<(Writer, mpsc::Receiver<Job>)> {
        let file = crate::logging::RotatingFile::open(path.to_path_buf(), budget)?;
        let (tx, rx) = mpsc::sync_channel::<Job>(cap);
        Ok((
            Writer {
                tx: Some(tx),
                file: Arc::new(Mutex::new(file)),
                thread: None,
                folder: path.parent().map(Path::to_path_buf),
            },
            rx,
        ))
    }

    /// The folder the log lives in (read once, at open — never a directory
    /// probe per publish).
    pub(crate) fn folder(&self) -> Option<&Path> {
        self.folder.as_deref()
    }

    /// A writer with no thread — the test's way of standing in for a dead
    /// one deterministically (drop the receiver and every send is
    /// `Disconnected`).
    #[cfg(test)]
    fn unattended(path: &Path, cap: usize) -> (Writer, mpsc::Receiver<Job>) {
        Self::open(path, cap, BUDGET).expect("a temp file opens")
    }

    /// Append `lines`, oldest first, in that order on disk. `try_send` to
    /// the thread; on `Full` (the thread is behind) the caller waits for a
    /// slot — one write long, and no longer than the write-it-yourself
    /// fallback took under the same file lock — so the file keeps the
    /// order the engine emitted; on `Disconnected` (the thread died) the
    /// line is written here, so nothing is dropped.
    pub(crate) fn append(&self, lines: &[LogLine]) {
        for line in lines {
            let encoded = encoded(line);
            if encoded.is_empty() {
                continue;
            }
            let Some(tx) = &self.tx else {
                write_line(&self.file, &encoded);
                continue;
            };
            let job = match tx.try_send(Job::Line(encoded)) {
                Ok(()) => continue,
                Err(TrySendError::Full(job)) => match tx.send(job) {
                    Ok(()) => continue,
                    Err(mpsc::SendError(job)) => job,
                },
                Err(TrySendError::Disconnected(job)) => job,
            };
            if let Job::Line(line) = job {
                write_line(&self.file, &line);
            }
        }
    }

    /// Wait until every line appended so far is on disk. The parent of a
    /// seamless handoff calls this right before it execs the successor:
    /// `exec` runs no destructor, so what the thread still held would die
    /// with it. A dead thread has nothing queued; nothing to wait for.
    pub(crate) fn flush(&self) {
        let Some(tx) = &self.tx else {
            return;
        };
        let (done, landed) = mpsc::sync_channel::<()>(1);
        if tx.send(Job::Flush(done)).is_ok() {
            let _ = landed.recv();
        }
    }
}

impl Drop for Writer {
    /// Close the channel, then wait for the thread to write what it holds:
    /// the lines a process queued in its last moments are its record.
    fn drop(&mut self) {
        self.tx = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_messages::{
        LOG_CAP, LogRecord, LogState, Message, MessageId, Retired, Severity, WallStamp, tags,
    };

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aterm-messages-store-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn posted(id: u64, title: &str) -> LogLine {
        LogLine::Posted(LogRecord::from_posted(
            MessageId::from_raw(id).unwrap(),
            WallStamp { unix_ms: id },
            &Message::new(tags::SYSTEM, Severity::Info, title),
        ))
    }

    fn retired(id: u64) -> LogLine {
        LogLine::Retired {
            id: MessageId::from_raw(id).unwrap(),
            how: Retired::Folded,
            unix_ms: id + 1,
            title: format!("m{id}"),
            detail: Vec::new(),
            repeats: 1,
        }
    }

    fn write_lines(path: &Path, lines: &[String]) {
        use std::io::Write as _;
        let mut f = File::create(path).unwrap();
        for l in lines {
            f.write_all(l.as_bytes()).unwrap();
        }
    }

    fn read_lines(path: &Path) -> Vec<String> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn load_reads_only_the_tail_and_skips_junk_lines() {
        let dir = scratch("tail");
        let path = dir.join(FILE_NAME);
        let mut lines: Vec<String> = (1..=40)
            .map(|i| encoded(&posted(i, &format!("m{i}"))))
            .collect();
        lines.insert(20, "this is not a record\n".into());
        lines.insert(30, "m1\tkind=posted\tid=99\n".into());
        write_lines(&path, &lines);
        let whole = load_tail(&path, u64::MAX);
        assert_eq!(whole.log.len(), 40, "every record from an uncut file");
        assert_eq!(whole.skipped, 2, "the two junk lines");
        assert_eq!(
            whole.log.next_id(),
            MessageId::from_raw(41).unwrap(),
            "ids continue past the loaded ones"
        );
        // A tail that starts mid-line drops the cut line and reads the rest.
        let last_ten: usize = lines[lines.len() - 10..].iter().map(String::len).sum();
        let cut = load_tail(&path, last_ten as u64 + 7);
        assert_eq!(cut.log.len(), 10, "the partial first line is not a record");
        let ids: Vec<u64> = cut.log.records().map(|r| r.id.raw()).collect();
        assert_eq!(ids, (31..=40).collect::<Vec<_>>());
        // A missing file and a directory are empty rings.
        assert!(load_tail(&dir.join("absent"), TAIL_BYTES).log.is_empty());
        assert!(
            load_tail(&dir, TAIL_BYTES).log.is_empty(),
            "no-follow, no dirs"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_log_is_refused() {
        let dir = scratch("symlink");
        let real = dir.join("real.log");
        write_lines(&real, &[encoded(&posted(1, "m1"))]);
        let link = dir.join(FILE_NAME);
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(load_tail(&link, TAIL_BYTES).log.is_empty());
        assert_eq!(load_tail(&real, TAIL_BYTES).log.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_dead_process_s_open_row_reads_back_stale() {
        let dir = scratch("stale");
        let path = dir.join(FILE_NAME);
        write_lines(
            &path,
            &[
                encoded(&posted(1, "folded")),
                encoded(&retired(1)),
                encoded(&posted(2, "left open")),
            ],
        );
        let loaded = load_for_launch(&path);
        let open = loaded.log.get(MessageId::from_raw(2).unwrap()).unwrap();
        assert_eq!(open.state, LogState::Retired(Retired::Stale));
        let folded = loaded.log.get(MessageId::from_raw(1).unwrap()).unwrap();
        assert_eq!(folded.state, LogState::Retired(Retired::Folded));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn append_never_drops_and_keeps_the_engines_order_past_the_channels_depth() {
        let dir = scratch("append");
        let path = dir.join(FILE_NAME);
        // The thread died (its receiver is gone): every line is written by
        // the caller, in order, and none is lost.
        let (writer, rx) = Writer::unattended(&path, 2);
        drop(rx);
        let lines: Vec<LogLine> = (1..=5).map(|i| posted(i, &format!("m{i}"))).collect();
        writer.append(&lines);
        let on_disk = read_lines(&path);
        assert_eq!(on_disk.len(), 5, "written by the caller for a dead thread");
        for (line, id) in on_disk.iter().zip(1..=5u64) {
            assert_eq!(
                LogLine::decode(line.trim_end())
                    .unwrap()
                    .id()
                    .unwrap()
                    .raw(),
                id
            );
        }
        drop(writer);
        // The real thread, pushed past the channel's depth in one append:
        // every line reaches the file by the time the writer is dropped
        // (drop joins the thread), IN THE ORDER APPENDED — the caller waited
        // for a slot rather than writing ahead of the queue.
        let path = dir.join("threaded.log");
        let writer = Writer::spawn(&path).expect("a writer over a temp file");
        let many: Vec<LogLine> = (1..=(QUEUE_CAP as u64 + 50))
            .map(|i| posted(i, &format!("m{i}")))
            .collect();
        writer.append(&many);
        // A flush returns only once everything before it is on disk.
        writer.flush();
        let flushed = read_lines(&path);
        assert_eq!(flushed.len(), QUEUE_CAP + 50, "flushed before the drop");
        drop(writer);
        let on_disk = read_lines(&path);
        assert_eq!(on_disk.len(), QUEUE_CAP + 50, "nothing dropped");
        let ids: Vec<u64> = on_disk
            .iter()
            .map(|l| LogLine::decode(l.trim_end()).unwrap().id().unwrap().raw())
            .collect();
        assert_eq!(
            ids,
            (1..=(QUEUE_CAP as u64 + 50)).collect::<Vec<_>>(),
            "the file keeps the engine's order"
        );
        let reloaded = load_tail(&path, TAIL_BYTES);
        assert_eq!(reloaded.skipped, 0);
        assert_eq!(reloaded.log.len(), LOG_CAP.min(QUEUE_CAP + 50));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "created owner-only");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A budget small enough to rotate in a test: 4 KiB files, a look after
    /// every write.
    const TINY: crate::logging::RotationBudget = crate::logging::RotationBudget {
        rotate_at: 4096,
        check_bytes: 0,
        check_every: std::time::Duration::ZERO,
    };

    /// THE LOG ROTATES WHILE RUNNING AND STAYS BOUNDED (design ruling 65):
    /// a writer that appends three budgets' worth of records leaves the current
    /// file and one older copy, each within the budget plus one look and one
    /// line, and no second older copy — however long the process runs, and
    /// without a launch in between.
    #[test]
    fn the_log_rotates_while_running_and_stays_bounded() {
        let dir = scratch("rotate");
        let path = dir.join(FILE_NAME);
        let writer = Writer::spawn_with(&path, TINY).expect("the writer");
        let mut id = 0;
        while (id as usize) * 60 < 3 * TINY.rotate_at as usize {
            id += 1;
            writer.append(&[posted(id, &format!("m{id}"))]);
        }
        writer.flush();
        let bound = TINY.rotate_at + TINY.check_bytes + MAX_LINE_BYTES as u64;
        let current = std::fs::metadata(&path).unwrap().len();
        let older = std::fs::metadata(crate::logging::rotated_path(&path))
            .expect("rotated once at least")
            .len();
        assert!(current <= bound && older <= bound, "{current} / {older}");
        let mut second = crate::logging::rotated_path(&path).into_os_string();
        second.push(".1");
        assert!(!Path::new(&second).exists(), "one older copy, never two");
        // Nothing but codec lines: no rotation note in a file the loader reads.
        let reloaded = load_tail(&path, TAIL_BYTES);
        assert_eq!(reloaded.skipped, 0);
        drop(writer);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A LAUNCH READS ACROSS THE ROTATION: the older copy's tail first, then the
    /// current file — every record, the ids rising past the newest, and a
    /// `Posted` in `.1` meeting its `Retired` in the current file (so it is not
    /// a dead process's open row).
    #[test]
    fn a_launch_reads_across_the_rotation() {
        let dir = scratch("across");
        let path = dir.join(FILE_NAME);
        let n = 5;
        write_lines(
            &crate::logging::rotated_path(&path),
            &(1..=n)
                .map(|i| encoded(&posted(i, &format!("m{i}"))))
                .collect::<Vec<_>>(),
        );
        write_lines(
            &path,
            &[encoded(&retired(n)), encoded(&posted(n + 1, "new"))],
        );
        let loaded = load_tail(&path, TAIL_BYTES);
        assert_eq!(loaded.log.len(), usize::try_from(n + 1).unwrap());
        assert_eq!(loaded.skipped, 0);
        let closed = loaded.log.get(MessageId::from_raw(n).unwrap()).unwrap();
        assert_eq!(
            closed.state,
            LogState::Retired(Retired::Folded),
            "its Retired, from the current file"
        );
        assert_eq!(loaded.log.next_id(), MessageId::from_raw(n + 2).unwrap());
        // The older copy is read only for what the current file lacks.
        let current_len = std::fs::metadata(&path).unwrap().len();
        let only_current = load_tail(&path, current_len);
        assert_eq!(only_current.log.len(), 1, "the orphan Retired is ignored");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A SECOND WRITER FOLLOWS THE ROTATION: two writers on one path (another
    /// instance, a successor); after one rotates, the other's next line lands
    /// in the new file, never in the renamed one's inode.
    #[test]
    fn a_second_writer_follows_the_rotation() {
        let dir = scratch("follow");
        let path = dir.join(FILE_NAME);
        let a = Writer::spawn_with(&path, TINY).expect("writer A");
        let b = Writer::spawn_with(&path, TINY).expect("writer B");
        let mut id = 0;
        while !crate::logging::rotated_path(&path).exists() {
            id += 1;
            a.append(&[posted(id, &format!("a{id}"))]);
            a.flush();
            assert!(id < 1000, "A rotated");
        }
        b.append(&[posted(id + 1, "from B")]);
        b.flush();
        let current = std::fs::read_to_string(&path).unwrap();
        assert!(
            current.contains("from%20B") || current.contains("from B"),
            "{current}"
        );
        drop((a, b));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The file is under the log dir, whatever `$ATERM_LOG` says: `path()`
    /// consults the log dir alone.
    #[test]
    fn the_log_is_written_under_the_log_dir_regardless_of_aterm_log() {
        let Some(path) = path() else {
            // No `$HOME` on this worker: nothing to place, nothing to assert.
            return;
        };
        assert_eq!(path.file_name().unwrap(), FILE_NAME);
        assert_eq!(path.parent(), crate::logging::log_dir().as_deref());
        // The shipping half never reads the logging switch: every CODE line
        // before the test module is free of the variable's name (comment lines
        // dropped, the grep guard's own rule — this module's doc names it).
        let shipping = include_str!("messages_store.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap()
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .any(|l| l.contains("ATERM_LOG"));
        assert!(
            !shipping,
            "the shipping half never reads the logging switch"
        );
    }

    /// REVIEW PIN (Lens B, 2026-09-22), now green: the first cut's
    /// write-it-yourself fallback put a `Retired` line on disk AHEAD of its
    /// `Posted` line whenever the channel was full, and the engine's replay
    /// — order-dependent — read the record back as a dead process's open
    /// row (`Stale`) with its final words lost. Posted/Retired pairs pushed
    /// past the channel's depth in one append must every one read back
    /// `Folded`.
    #[test]
    fn a_full_channel_never_puts_a_retired_line_ahead_of_its_posted_line() {
        let dir = scratch("reorder");
        let path = dir.join(FILE_NAME);
        let writer = Writer::spawn(&path).expect("a writer over a temp file");
        let n = QUEUE_CAP as u64 + 20;
        let lines: Vec<LogLine> = (1..=n).flat_map(|i| [posted(i, "m"), retired(i)]).collect();
        writer.append(&lines);
        drop(writer);
        let loaded = load_tail(&path, TAIL_BYTES);
        assert_eq!(loaded.skipped, 0, "every line decodes");
        for i in (n - LOG_CAP as u64 + 1)..=n {
            let rec = loaded.log.get(MessageId::from_raw(i).unwrap()).unwrap();
            assert_eq!(
                rec.state,
                LogState::Retired(Retired::Folded),
                "record {i} must read back as Folded, not as a dead process's open row"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
