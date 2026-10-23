// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `<log_dir>/messages.log`, the durable half of the unified message system (docs/DESIGN-unified-messages-2026-09-21.md §3.7).

use std::collections::VecDeque;
use std::fs::File;
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

/// One line as it lands on disk: the codec's form plus the newline.
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
    thread: Option<std::thread::JoinHandle<()>>,
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
        let (mut writer, rx) = match Self::open(path, QUEUE_CAP, budget, wire_budget) {
            Ok(pair) => pair,
            Err(error) => {
                aterm_log::warn!("messages log: {} not opened ({error})", path.display());
                return None;
            }
        };
        let files = Arc::clone(&writer.files);
        let overflow = Arc::clone(&writer.overflow);
        let overflowed = Arc::clone(&writer.overflowed);
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
                thread: None,
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
        let deadline = std::time::Instant::now() + within;
        let Some(tx) = &self.tx else {
            let mut queued = self.overflow.lock().unwrap_or_else(PoisonError::into_inner);
            self.hand_over(&mut queued);
            return true;
        };
        let (done, landed) = mpsc::sync_channel::<()>(1);
        let mut job = Job::Flush(done, qos::Boost::start(self.thread.as_ref()));
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
        let attended = self.thread.is_some();
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
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_messages::{
        LOG_CAP, LogRecord, LogState, Message, MessageId, Origin, Retired, Severity, WallStamp,
        tags,
    };

    /// `lines` shelved as aterm's own.
    fn host(lines: &[LogLine]) -> Vec<(LogLine, Shelf)> {
        lines.iter().map(|l| (l.clone(), Shelf::Host)).collect()
    }

    /// A record the wire posted.
    fn wire_posted(id: u64, title: &str) -> LogLine {
        let mut msg = Message::new(tags::SYSTEM, Severity::Warn, title);
        msg.origin = Origin::Wire;
        LogLine::Posted(LogRecord::from_posted(
            MessageId::from_raw(id).unwrap(),
            WallStamp { unix_ms: id },
            &msg,
        ))
    }

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
            mark: None,
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
        let last_ten: usize = lines[lines.len() - 10..].iter().map(String::len).sum();
        let cut = load_tail(&path, last_ten as u64 + 7);
        assert_eq!(cut.log.len(), 10, "the partial first line is not a record");
        let ids: Vec<u64> = cut.log.records().map(|r| r.id.raw()).collect();
        assert_eq!(ids, (31..=40).collect::<Vec<_>>());
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
    fn append_never_drops_and_keeps_the_engines_order_past_the_channels_depth() {
        let dir = scratch("append");
        let path = dir.join(FILE_NAME);
        let (writer, rx) = Writer::unattended(&path, 2);
        drop(rx);
        let lines: Vec<LogLine> = (1..=5).map(|i| posted(i, &format!("m{i}"))).collect();
        writer.append(&host(&lines));
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
        let path = dir.join("threaded.log");
        let writer = Writer::spawn(&path).expect("a writer over a temp file");
        let many: Vec<LogLine> = (1..=(QUEUE_CAP as u64 + 50))
            .map(|i| posted(i, &format!("m{i}")))
            .collect();
        writer.append(&host(&many));
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

    /// A WRITER THAT FALLS BEHIND NEVER HOLDS THE CALLER: with its thread
    /// paused mid-write (the files lock held, as a disk that stopped
    /// answering would), four queues' worth of lines are appended at once and
    /// a bounded flush answers `false` at its deadline; once the thread runs
    /// again every line lands, none lost and in the order appended.
    #[test]
    fn a_paused_writer_holds_no_caller_and_lands_every_line_in_order_after() {
        let dir = scratch("paused");
        let path = dir.join(FILE_NAME);
        let writer = Writer::spawn(&path).expect("a writer over a temp file");
        let n = 2000u64;
        let lines: Vec<LogLine> = (1..=n).map(|i| posted(i, &format!("m{i}"))).collect();
        let mut paused = Some(writer.files.lock().unwrap());
        let (writer_ref, lines_ref) = (&writer, &lines);
        std::thread::scope(|scope| {
            let (appended_tx, appended) = std::sync::mpsc::channel();
            scope.spawn(move || {
                writer_ref.append(&host(lines_ref));
                let _ = appended_tx.send(());
            });
            if appended
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .is_err()
            {
                drop(paused.take());
                panic!("the append held its caller while the thread was paused");
            }
        });
        assert!(
            !writer.flush_within(std::time::Duration::from_millis(20)),
            "a thread that cannot write misses the bound"
        );
        drop(paused);
        writer.flush();
        let ids: Vec<u64> = read_lines(&path)
            .iter()
            .map(|l| LogLine::decode(l).unwrap().id().unwrap().raw())
            .collect();
        assert_eq!(ids, (1..=n).collect::<Vec<_>>(), "every line, in order");
        drop(writer);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Once a paused disk resumes, the writer lands every accepted line even
    /// if the UI has gone idle.
    #[test]
    fn a_recovered_writer_drains_overflow_without_another_ui_call() {
        let dir = aterm_tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join(FILE_NAME);
        let writer = Writer::spawn(&path).expect("a writer over a temp file");
        let n = (QUEUE_CAP * 4) as u64;
        let lines: Vec<LogLine> = (1..=n).map(|id| posted(id, &format!("m{id}"))).collect();
        let bytes: u64 = lines.iter().map(|line| encoded(line).len() as u64).sum();
        let paused = writer.files.lock().unwrap();
        writer.append(&host(&lines));
        assert!(
            writer.overflowed.load(Ordering::Relaxed) > 0,
            "the real writer could not consume the overflowing burst"
        );
        drop(paused);

        let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        loop {
            let landed = std::fs::metadata(&path).expect("the actual log").len();
            if landed >= bytes {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the writer stranded accepted lines after recovery: {landed}/{bytes} bytes"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let ids: Vec<u64> = read_lines(&path)
            .iter()
            .map(|line| LogLine::decode(line).unwrap().id().unwrap().raw())
            .collect();
        assert_eq!(ids, (1..=n).collect::<Vec<_>>(), "every line, in order");
    }

    /// A budget small enough to rotate in a test: 4 KiB files, a look after
    /// every write.
    const TINY: crate::logging::RotationBudget = crate::logging::RotationBudget {
        rotate_at: 4096,
        check_bytes: 0,
        check_every: std::time::Duration::ZERO,
    };

    /// Three budgets' worth of appends leave the current file and one older copy, each
    /// within the budget plus one look and one line.
    #[test]
    fn the_log_rotates_while_running_and_stays_bounded() {
        let dir = scratch("rotate");
        let path = dir.join(FILE_NAME);
        let writer = Writer::spawn_with(&path, TINY, TINY).expect("the writer");
        let mut id = 0;
        while (id as usize) * 60 < 3 * TINY.rotate_at as usize {
            id += 1;
            writer.append(&host(&[posted(id, &format!("m{id}"))]));
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
        let reloaded = load_tail(&path, TAIL_BYTES);
        assert_eq!(reloaded.skipped, 0);
        drop(writer);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The older copy's tail is read first: a `Posted` in `.1` meets its `Retired` in
    /// the current file, and ids rise past the newest.
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
        let current_len = std::fs::metadata(&path).unwrap().len();
        let only_current = load_tail(&path, current_len);
        assert_eq!(only_current.log.len(), 1, "the orphan Retired is ignored");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Posted/Retired pairs pushed past the channel's depth all read back `Folded`: a
    /// `Retired` written ahead of its `Posted` would replay as `Stale`.
    #[test]
    fn a_full_channel_never_puts_a_retired_line_ahead_of_its_posted_line() {
        let dir = scratch("reorder");
        let path = dir.join(FILE_NAME);
        let writer = Writer::spawn(&path).expect("a writer over a temp file");
        let n = QUEUE_CAP as u64 + 20;
        let lines: Vec<LogLine> = (1..=n).flat_map(|i| [posted(i, "m"), retired(i)]).collect();
        writer.append(&host(&lines));
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

    /// Wire lines rotate in `messages.wire.log`, bounded; `messages.log` keeps its one
    /// host record unrotated, and a launch reads it back beside the wire's newest.
    #[test]
    fn a_wire_flood_never_rotates_aterms_own_record_off_disk() {
        let dir = scratch("wire-shelf");
        let path = dir.join(FILE_NAME);
        let writer = Writer::spawn_with(&path, BUDGET, TINY).expect("the writer");
        assert!(
            !wire_path(&path).exists(),
            "no wire file until the wire writes"
        );
        writer.append(&[
            (posted(1, "crash record"), Shelf::Host),
            (retired(1), Shelf::Host),
        ]);
        let mut id = 1;
        while (id as usize) * 60 < 3 * TINY.rotate_at as usize {
            id += 1;
            writer.append(&[
                (wire_posted(id, &format!("w{id}")), Shelf::Wire),
                (retired(id), Shelf::Wire),
            ]);
        }
        writer.flush();
        let host_lines = read_lines(&path);
        assert_eq!(host_lines.len(), 2, "{host_lines:?}");
        assert!(
            !crate::logging::rotated_path(&path).exists(),
            "messages.log never rotated"
        );
        let wire = wire_path(&path);
        let bound = TINY.rotate_at + TINY.check_bytes + MAX_LINE_BYTES as u64;
        let current = std::fs::metadata(&wire).unwrap().len();
        let older = std::fs::metadata(crate::logging::rotated_path(&wire))
            .expect("the wire's file rotated")
            .len();
        assert!(current <= bound && older <= bound, "{current} / {older}");
        drop(writer);
        let loaded = load_tail(&path, TAIL_BYTES);
        assert_eq!(loaded.skipped, 0);
        let crash = loaded.log.get(MessageId::from_raw(1).unwrap());
        assert_eq!(
            crash.map(|r| &r.state),
            Some(&LogState::Retired(Retired::Folded)),
            "the host record reads back"
        );
        let newest = loaded.log.get(MessageId::from_raw(id).unwrap()).unwrap();
        assert!(newest.wire_owned());
        assert_eq!(newest.state, LogState::Retired(Retired::Folded));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Records interleaved across the two files load oldest-first by id, each meeting
    /// its own `Retired`; ids continue past the newest; the wire's tail is read at most
    /// [`WIRE_TAIL_BYTES`].
    #[test]
    fn a_launch_merges_the_two_files_by_id() {
        let dir = scratch("wire-merge");
        let path = dir.join(FILE_NAME);
        write_lines(
            &path,
            &[
                encoded(&posted(1, "h1")),
                encoded(&posted(3, "h3")),
                encoded(&retired(1)),
                encoded(&posted(6, "h6")),
            ],
        );
        write_lines(
            &wire_path(&path),
            &[
                encoded(&wire_posted(2, "w2")),
                encoded(&wire_posted(4, "w4")),
                encoded(&retired(2)),
                encoded(&wire_posted(5, "w5")),
            ],
        );
        let loaded = load_tail(&path, TAIL_BYTES);
        let ids: Vec<u64> = loaded.log.records().map(|r| r.id.raw()).collect();
        assert_eq!(ids, vec![1, 2, 3, 4, 5, 6], "oldest first, by id");
        for (id, how) in [
            (1, Retired::Folded),
            (2, Retired::Folded),
            (4, Retired::Stale),
        ] {
            let rec = loaded.log.get(MessageId::from_raw(id).unwrap()).unwrap();
            assert_eq!(rec.state, LogState::Retired(how), "record {id}");
        }
        assert_eq!(loaded.log.next_id(), MessageId::from_raw(7).unwrap());
        let big: Vec<String> = (10..10 + 2 * WIRE_TAIL_BYTES / 64)
            .map(|i| encoded(&wire_posted(i, &format!("w{i}"))))
            .collect();
        write_lines(&wire_path(&path), &big);
        let loaded = load_tail(&path, TAIL_BYTES);
        let wire_bytes: u64 = loaded
            .log
            .records()
            .filter(|r| r.wire_owned())
            .map(|r| encoded(&LogLine::Posted(r.clone())).len() as u64)
            .sum();
        assert!(wire_bytes <= WIRE_TAIL_BYTES, "{wire_bytes}");
        assert!(loaded.log.get(MessageId::from_raw(6).unwrap()).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
