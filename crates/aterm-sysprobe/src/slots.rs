// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE MACHINE'S BUILD SLOTS, READ WITHOUT A LOCK: which of Trust's
//! `build-slot` flock files a live build holds, which are free, and which a
//! dead wrapper's orphaned build still occupies
//! (`docs/DESIGN-harness-occupancy-2026-10-20.md` §4.2).
//!
//! `build-slot` (`$HOME/trust/scripts/build-slot`) admits a heavy build into one
//! of a few slots: `slot-<i>` files in `~/.cache/build-slot` (or its
//! `BUILD_SLOT_DIR`), each taken by `flock(LOCK_EX | LOCK_NB)` on a
//! descriptor the wrapper keeps until its build exits. The winner truncates
//! the file and writes one JSON record, `{"pid": …, "cmd": …, "cwd": …,
//! "started": …}` (Python's `json.dumps`: the wrapper's pid and its
//! `time.time()`), and never clears it. Its `status` verb takes and drops
//! each free slot's lock, so a reader that did the same would make a racing
//! acquirer skip a free slot.
//!
//! # What this reader never does
//!
//! It takes no lock of any kind (`flock`, `fcntl` `F_SETLK`, `O_EXLOCK`),
//! never opens for writing or creates (`O_RDONLY | O_NOFOLLOW | O_NONBLOCK`,
//! close on exec), never truncates or touches times, follows no symlinked
//! slot and starts no process. `flock(2)` conflicts only between open file
//! descriptions that hold a lock, and the reader's holds none, so no
//! acquirer's `LOCK_EX | LOCK_NB` can fail because of it. It asks whether a
//! lock is held with `fcntl(F_GETLK)`, a query that takes none and leaves
//! none: Darwin keeps a vnode's `flock` locks and record locks in one list,
//! so the query sees `build-slot`'s `flock` (`live::locked`).
//!
//! # A slot's sight, and its state over two reads
//!
//! Each read ([`observe`]) gives every `slot-<digits>` regular file a
//! [`Sight`]: its record is empty, unreadable ([`Sight::Bad`]), held by the
//! process it names (alive, born no later than the record's `started` plus
//! [`BIRTH_SLACK_US`], and holding a descriptor to this very file: device and
//! inode, from libproc's `PROC_PIDLISTFDS` and `PROC_PIDFDVNODEPATHINFO`), or
//! not held. The lock query, asked after the record is read, has the last
//! word on not held: a file whose lock someone holds is never empty or not
//! held ([`Sight::Unproven`]: a winner between its `flock` and its record,
//! however long it stalls there), and a writer whose descriptors cannot be
//! read has let go once nobody holds the lock. A [`SlotBook`] pairs two reads
//! at least [`MIN_GAP_MS`] apart ([`SlotState`]): held is the same holder
//! seen on both, free is no holder on both, unknown is a read that could not
//! be judged, and pending is everything else: a first read, a single
//! sighting, a change.
//!
//! # A dead wrapper's live build
//!
//! A wrapper killed mid-build frees its flock while its build (a session of
//! its own) runs on under launchd. A free slot is counted occupied by such an
//! orphan ([`SlotCounts::discounted`]) only when its record's wrapper is gone
//! (dead, or its pid reused), it started within [`ORPHAN_HORIZON_US`], the
//! machine's 5-minute load is more than the held slots and the orphans
//! already counted explain, and a live witness is found: a process launchd
//! adopted, leading its own session and group, of this user, born within
//! [`WITNESS_WINDOW_US`] of the record's `started`, working in the record's
//! `cwd` ([`adopted_in_window`] and the live check), and no witness already
//! of another candidate in this read. The process table is read only when a
//! candidate gets that far.
//!
//! # What enters aterm
//!
//! The record's pid and start; its `cwd` only to compare with a witness's
//! working directory, never printed. Its `cmd` is checked to be a string and
//! dropped.
//!
//! Bounded: at most [`MAX_ENTRIES`] directory entries are looked at,
//! [`MAX_SLOTS`] slots kept, [`RECORD_CAP`] bytes read per record. macOS
//! only; elsewhere every read is [`SlotsRead::Unread`]`(`[`Unread::Unsupported`]`)`.
//! No timer and no thread: [`SlotReader`] re-reads when its caller asks, at
//! most once per [`PERIOD`] while a wait names the slots, else once per
//! [`IDLE_PERIOD`].

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Where `build-slot` keeps its slots under a home directory.
pub const SLOT_DIR: &str = ".cache/build-slot";
/// A record longer than this is unreadable ([`Sight::Bad`]).
pub const RECORD_CAP: usize = 16 << 10;
/// Two reads closer than this are not paired: the later one is not kept.
pub const MIN_GAP_MS: u64 = 1_000;
/// [`SlotReader`] reads the slots again at most this often while a wait names
/// them (`armed`, [`SlotReader::read`]).
pub const PERIOD: Duration = Duration::from_secs(5);
/// …and at most this often otherwise: the occupancy board's look
/// (`DESIGN-harness-occupancy-2026-10-20.md` §3.1, the cadence).
pub const IDLE_PERIOD: Duration = Duration::from_secs(60);
/// The line a host logs when the first wait naming the slots arms its
/// reader's [`PERIOD`] (the lab's sign that a wait is armed).
pub const ARMED_LINE: &str = "occupancy: build slots read every 5 s (a wait names them)";
/// …and when the last one goes, back to [`IDLE_PERIOD`].
pub const AT_REST_LINE: &str = "occupancy: build slots read once a minute (no wait names them)";
/// An orphan is looked for only behind a record this young (12 hours).
pub const ORPHAN_HORIZON_US: u64 = 12 * 3600 * 1_000_000;
/// A witness was born within this long after its record's `started`.
pub const WITNESS_WINDOW_US: u64 = 10_000_000;
/// The record's `started` is a float of the wrapper's `time.time()`, read
/// after its exec: a holder born up to this much later is still the writer.
pub const BIRTH_SLACK_US: u64 = 1_000;
/// At most this many directory entries are looked at per read.
pub const MAX_ENTRIES: usize = 1024;
/// At most this many slots (the lowest indices) are kept per read.
pub const MAX_SLOTS: usize = 256;

/// Why the slots were not read. A closed set: the host spells each with the
/// word its readings refuse with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unread {
    /// A platform with no reader.
    Unsupported,
    /// A missing directory, or one with no slot in it.
    NotSampled,
    /// A directory, or every slot in it, refused to this user.
    Refused,
    /// Every slot's record is unreadable: `build-slot` changed its record.
    UnreadFormat,
}

/// The slot directory under `home` ([`SLOT_DIR`]).
#[must_use]
pub fn default_dir(home: &Path) -> PathBuf {
    home.join(SLOT_DIR)
}

/// What a slot's record says: who wrote it and when.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// The wrapper's pid.
    pub pid: u32,
    /// Its `started`, in microseconds since the epoch (rounded down).
    pub started_us: u64,
    /// Its `cwd`, kept only to compare with a witness's working directory.
    pub cwd: Option<PathBuf>,
}

/// One slot file's contents, parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordRead {
    /// No bytes: a slot `status` created and nobody won, or a winner between
    /// its truncate and its write.
    Empty,
    /// Not a record this reader knows.
    Bad,
    /// A record.
    Rec(Record),
}

impl Record {
    /// Parse a slot file's bytes: a JSON object whose `pid` is an integer in
    /// `2..=i32::MAX` and whose `started` is a finite number at least 0;
    /// `cmd` and `cwd`, when present, are strings; `"v"`, when present, is 1.
    /// Other keys are allowed.
    #[must_use]
    pub fn parse(bytes: &[u8]) -> RecordRead {
        use aterm_json::Value;
        if bytes.is_empty() {
            return RecordRead::Empty;
        }
        let Ok(v) = aterm_json::from_slice::<Value>(bytes) else {
            return RecordRead::Bad;
        };
        if v.as_object().is_none() {
            return RecordRead::Bad;
        }
        let version_known = v.get("v").is_none_or(|n| n.as_u64() == Some(1));
        let strings = ["cmd", "cwd"]
            .iter()
            .all(|k| v.get(k).is_none_or(|s| s.as_str().is_some()));
        let pid = v
            .get("pid")
            .and_then(Value::as_u64)
            .filter(|p| (2..=i32::MAX as u64).contains(p))
            .and_then(|p| u32::try_from(p).ok());
        let started = v
            .get("started")
            .and_then(Value::as_f64)
            .filter(|s| s.is_finite() && *s >= 0.0);
        match (pid, started) {
            (Some(pid), Some(started)) if version_known && strings => RecordRead::Rec(Self {
                pid,
                // Saturating: a `started` past `u64` microseconds reads as the
                // far future, never wraps.
                started_us: (started * 1e6) as u64,
                cwd: v.get("cwd").and_then(Value::as_str).map(PathBuf::from),
            }),
            _ => RecordRead::Bad,
        }
    }
}

/// A process that holds a slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Holder {
    /// Its pid.
    pub pid: u32,
    /// When it was born, microseconds since the epoch: with the pid, its
    /// identity across reads.
    pub birth_us: u64,
}

/// One slot as one read saw it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sight {
    /// An empty record.
    Empty,
    /// Not a regular file, past [`RECORD_CAP`], or a record this reader does
    /// not know.
    Bad,
    /// Could not be judged this read: the file was replaced or refused while
    /// it was read, the named process's descriptors could not be read while
    /// the lock query gave no answer, or someone holds the file's lock whom
    /// the record does not show holding it.
    Unproven,
    /// A record whose writer does not hold the file now. `gone` carries the
    /// record when the writer is dead or its pid reused (an orphan's
    /// candidate); `None` when the writer lives and has let go.
    NotHeld {
        /// The record of a gone writer.
        gone: Option<Record>,
    },
    /// The record's writer holds the file open.
    Sighted(Holder),
}

/// One slot in one read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotObs {
    /// Its `<i>` in `slot-<i>`.
    pub index: u32,
    /// The file's device (as `u32`, the kernel's `dev_t` bits) and inode.
    pub file: (u32, u64),
    /// What the read saw.
    pub sight: Sight,
}

/// One read of the slot directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observation {
    /// When, on the caller's monotonic clock, in milliseconds.
    pub mono_ms: u64,
    /// When, in microseconds since the epoch.
    pub now_us: u64,
    /// Every slot, by index.
    pub slots: Vec<SlotObs>,
}

/// A slot's state over two reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotState {
    /// The same holder on both.
    Held(Holder),
    /// No holder on both.
    Free,
    /// Neither yet: a first read, a single sighting, a change.
    Pending,
    /// This read could not judge it ([`Sight::Bad`], [`Sight::Unproven`]):
    /// never free.
    Unknown,
}

/// How many slots are in each state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SlotCounts {
    /// Slots present.
    pub n: u16,
    /// Held.
    pub held: u16,
    /// Free (orphan-occupied ones included; see `discounted`).
    pub free: u16,
    /// Pending.
    pub pending: u16,
    /// Unknown.
    pub unknown: u16,
    /// Free slots a dead wrapper's live build still occupies: usable is
    /// `free - discounted`.
    pub discounted: u16,
}

/// The slots as a reading proved them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlotsRead {
    /// Not read, and why.
    Unread(Unread),
    /// Read.
    Read {
        /// The counts.
        counts: SlotCounts,
        /// Every held slot's holder, by slot index.
        holders: Vec<Holder>,
    },
}

/// The machine facts the orphan rule weighs.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Machine {
    /// The 5-minute load average.
    pub load5: Option<f64>,
    /// Logical cores.
    pub ncpu: Option<usize>,
}

/// The reads a judgement pairs: the last one kept, and what it judged.
#[derive(Clone, Debug, Default)]
pub struct SlotBook {
    prev: Option<Observation>,
    last: Option<SlotsRead>,
}

impl SlotBook {
    /// Judge `obs` against the last read kept (see the module docs). A read
    /// fewer than [`MIN_GAP_MS`] after the kept one is not kept, and the last
    /// judgement stands. `witness` answers whether a gone writer's build
    /// provably runs on; it is asked only of a free slot that passed the
    /// other orphan rules.
    pub fn judge(
        &mut self,
        obs: Observation,
        m: &Machine,
        witness: &mut dyn FnMut(&Record) -> bool,
    ) -> SlotsRead {
        if let (Some(prev), Some(last)) = (&self.prev, &self.last)
            && obs.mono_ms < prev.mono_ms.saturating_add(MIN_GAP_MS)
        {
            return last.clone();
        }
        let read = judged(self.prev.as_ref(), &obs, m, witness);
        self.prev = Some(obs);
        self.last = Some(read.clone());
        read
    }
}

/// A slot's state from its sight before (`None`: not read before) and now.
fn state(before: Option<&Sight>, now: &Sight) -> SlotState {
    match (before, now) {
        (_, Sight::Bad | Sight::Unproven) => SlotState::Unknown,
        (Some(Sight::Sighted(a)), Sight::Sighted(b)) if a == b => SlotState::Held(*b),
        (Some(Sight::Empty | Sight::NotHeld { .. }), Sight::Empty | Sight::NotHeld { .. }) => {
            SlotState::Free
        }
        _ => SlotState::Pending,
    }
}

fn judged(
    prev: Option<&Observation>,
    obs: &Observation,
    m: &Machine,
    witness: &mut dyn FnMut(&Record) -> bool,
) -> SlotsRead {
    if obs.slots.is_empty() {
        return SlotsRead::Unread(Unread::NotSampled);
    }
    if obs.slots.iter().all(|s| s.sight == Sight::Bad) {
        return SlotsRead::Unread(Unread::UnreadFormat);
    }
    let mut counts = SlotCounts {
        n: u16::try_from(obs.slots.len()).unwrap_or(u16::MAX),
        ..SlotCounts::default()
    };
    let mut holders = Vec::new();
    let mut candidates = Vec::new();
    for s in &obs.slots {
        let before = prev
            .and_then(|p| p.slots.iter().find(|q| q.index == s.index))
            .map(|q| &q.sight);
        let tally = match state(before, &s.sight) {
            SlotState::Held(h) => {
                holders.push(h);
                &mut counts.held
            }
            SlotState::Free => {
                if let Sight::NotHeld { gone: Some(r) } = &s.sight {
                    candidates.push(r);
                }
                &mut counts.free
            }
            SlotState::Pending => &mut counts.pending,
            SlotState::Unknown => &mut counts.unknown,
        };
        *tally = tally.saturating_add(1);
    }
    counts.discounted = discounted(candidates, counts, obs.now_us, m, witness);
    SlotsRead::Read { counts, holders }
}

/// How many of the free slots' gone writers' builds run on (module docs):
/// the newest record first, each counted while the 5-minute load exceeds what
/// the held slots and the orphans counted so far, this one included, explain
/// (`load5 > (held + k) * ncpu / n`).
fn discounted(
    mut candidates: Vec<&Record>,
    counts: SlotCounts,
    now_us: u64,
    m: &Machine,
    witness: &mut dyn FnMut(&Record) -> bool,
) -> u16 {
    let (Some(load5), Some(ncpu)) = (m.load5, m.ncpu) else {
        return 0;
    };
    if !load5.is_finite() || ncpu == 0 || counts.n == 0 {
        return 0;
    }
    candidates.retain(|r| now_us.saturating_sub(r.started_us) <= ORPHAN_HORIZON_US);
    candidates.sort_by_key(|r| std::cmp::Reverse(r.started_us));
    let per_slot = ncpu as f64 / f64::from(counts.n);
    let mut found: u16 = 0;
    for r in candidates {
        let k = f64::from(counts.held) + f64::from(found) + 1.0;
        if load5 <= k * per_slot {
            break;
        }
        if witness(r) {
            found = found.saturating_add(1);
        }
    }
    found
}

/// Whether the process `p` could be `rec`'s orphaned build by its kernel
/// record alone: launchd adopted it, it leads its own group, it is `uid`'s,
/// it is no zombie, and it was born within [`WITNESS_WINDOW_US`] after
/// `rec`'s `started` (less [`BIRTH_SLACK_US`] for the float). The live check
/// adds that it leads its session and works in `rec`'s `cwd`.
#[must_use]
pub fn adopted_in_window(p: &crate::kproc::Record, rec: &Record, uid: u32) -> bool {
    let birth = birth_us(p);
    p.ppid == 1
        && u32::try_from(p.pgid).ok() == Some(p.pid)
        && p.uid == uid
        && !p.zombie
        && birth >= rec.started_us.saturating_sub(BIRTH_SLACK_US)
        && birth <= rec.started_us.saturating_add(WITNESS_WINDOW_US)
}

/// A kernel record's birth in microseconds since the epoch.
fn birth_us(p: &crate::kproc::Record) -> u64 {
    p.start_s
        .saturating_mul(1_000_000)
        .saturating_add(u64::from(p.start_us))
}

/// Read the slot directory `dir` once, at `mono_ms` (the caller's monotonic
/// clock) and `now_us` (the epoch). `Err` says why not:
/// [`Unread::NotSampled`] for a missing directory or one with no slot,
/// [`Unread::Refused`] when it or every slot is refused,
/// [`Unread::Unsupported`] off macOS.
///
/// # Errors
/// As above.
pub fn observe(dir: &Path, mono_ms: u64, now_us: u64) -> Result<Observation, Unread> {
    #[cfg(target_os = "macos")]
    {
        live::observe_in(dir, mono_ms, now_us, &mut |_| {})
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (dir, mono_ms, now_us);
        Err(Unread::Unsupported)
    }
}

/// The index of a file named `slot-<digits>`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn slot_index(name: &str) -> Option<u32> {
    let digits = name.strip_prefix("slot-")?;
    (!digits.is_empty() && digits.len() <= 9 && digits.bytes().all(|b| b.is_ascii_digit()))
        .then(|| digits.parse().ok())
        .flatten()
}

/// The slot reader a host keeps: one directory, its [`SlotBook`], and the
/// last reading, handed back until [`PERIOD`] has passed while a wait names
/// the slots, [`IDLE_PERIOD`] otherwise.
#[derive(Debug)]
pub struct SlotReader {
    dir: PathBuf,
    book: SlotBook,
    origin: Option<Instant>,
    last: Option<(Instant, SlotsRead)>,
}

impl SlotReader {
    /// A reader of the slots in `dir` ([`default_dir`]).
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            book: SlotBook::default(),
            origin: None,
            last: None,
        }
    }

    /// The directory it reads.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The slots at `now` (`now_us` since the epoch): the last reading while
    /// it is younger than [`PERIOD`] (`armed`: a wait names the slots) or
    /// [`IDLE_PERIOD`], else a fresh read judged against the last one. A
    /// directory that cannot be read starts the pairing over.
    pub fn read(&mut self, now: Instant, now_us: u64, m: &Machine, armed: bool) -> SlotsRead {
        let period = if armed { PERIOD } else { IDLE_PERIOD };
        if let Some((at, read)) = &self.last
            && now.saturating_duration_since(*at) < period
        {
            return read.clone();
        }
        let origin = *self.origin.get_or_insert(now);
        let mono_ms =
            u64::try_from(now.saturating_duration_since(origin).as_millis()).unwrap_or(u64::MAX);
        let read = match observe(&self.dir, mono_ms, now_us) {
            Ok(obs) => self.book.judge(obs, m, &mut live_witness()),
            Err(why) => {
                self.book = SlotBook::default();
                SlotsRead::Unread(why)
            }
        };
        self.last = Some((now, read.clone()));
        read
    }

    /// When the reading [`Self::read`] hands back was read; `None` before the
    /// first.
    #[must_use]
    pub fn read_at(&self) -> Option<Instant> {
        self.last.as_ref().map(|(at, _)| *at)
    }
}

/// The live witness check (module docs): the process table read once, on the
/// first question, and each candidate's session and working directory read
/// from the kernel. Off macOS, never a witness.
pub fn live_witness() -> impl FnMut(&Record) -> bool {
    #[cfg(target_os = "macos")]
    {
        live::witness()
    }
    #[cfg(not(target_os = "macos"))]
    {
        |_: &Record| false
    }
}

#[cfg(target_os = "macos")]
mod live {
    use std::io::{ErrorKind, Read as _};
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    use std::path::{Path, PathBuf};

    use super::{
        BIRTH_SLACK_US, Holder, MAX_ENTRIES, MAX_SLOTS, Observation, RECORD_CAP, Record,
        RecordRead, Sight, SlotObs, Unread, adopted_in_window, birth_us, slot_index,
    };
    use crate::kproc;

    /// One slot's read: kept, vanished between the listing and the read, or
    /// refused to this user.
    enum Got {
        Slot(SlotObs),
        Vanished,
        Refused,
    }

    /// [`super::observe`], with `while_open` called on each slot file while
    /// the reader's descriptor to it is open (the tests' acquirer seam).
    pub(super) fn observe_in(
        dir: &Path,
        mono_ms: u64,
        now_us: u64,
        while_open: &mut dyn FnMut(&Path),
    ) -> Result<Observation, Unread> {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == ErrorKind::PermissionDenied => return Err(Unread::Refused),
            Err(_) => return Err(Unread::NotSampled),
        };
        let mut named: Vec<(u32, PathBuf)> = entries
            .take(MAX_ENTRIES)
            .filter_map(Result::ok)
            .filter_map(|e| Some((slot_index(e.file_name().to_str()?)?, e.path())))
            .collect();
        named.sort_unstable_by_key(|(i, _)| *i);
        named.truncate(MAX_SLOTS);
        let mut slots = Vec::with_capacity(named.len());
        let mut refused = 0usize;
        for (index, path) in &named {
            match slot(*index, path, while_open) {
                Got::Slot(s) => slots.push(s),
                Got::Vanished => {}
                Got::Refused => {
                    refused += 1;
                    slots.push(SlotObs {
                        index: *index,
                        file: (0, 0),
                        sight: Sight::Unproven,
                    });
                }
            }
        }
        if slots.is_empty() {
            return Err(Unread::NotSampled);
        }
        if refused == slots.len() {
            return Err(Unread::Refused);
        }
        Ok(Observation {
            mono_ms,
            now_us,
            slots,
        })
    }

    /// The file's identity as the kernel's vnode answer spells it: `vst_dev`
    /// is a 32-bit `dev_t`, which std widens with its sign.
    fn identity(meta: &std::fs::Metadata) -> (u32, u64) {
        (meta.dev() as u32, meta.ino())
    }

    fn slot(index: u32, path: &Path, while_open: &mut dyn FnMut(&Path)) -> Got {
        let kept = |file, sight| Got::Slot(SlotObs { index, file, sight });
        let meta = match std::fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == ErrorKind::NotFound => return Got::Vanished,
            Err(e) if e.kind() == ErrorKind::PermissionDenied => return Got::Refused,
            Err(_) => return kept((0, 0), Sight::Unproven),
        };
        let file = identity(&meta);
        if !meta.file_type().is_file() {
            return kept(file, Sight::Bad);
        }
        // Read only: no create, no write, no lock; a symlink swapped in since
        // the `lstat` fails (`ELOOP`) rather than being followed; a FIFO
        // swapped in does not block the open.
        let opened = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path);
        let mut f = match opened {
            Ok(f) => f,
            Err(e) if e.kind() == ErrorKind::NotFound => return Got::Vanished,
            Err(e) if e.kind() == ErrorKind::PermissionDenied => return Got::Refused,
            Err(_) => return kept(file, Sight::Unproven),
        };
        let same = f
            .metadata()
            .is_ok_and(|m| m.file_type().is_file() && identity(&m) == file);
        if !same {
            return kept(file, Sight::Unproven);
        }
        let mut bytes = Vec::new();
        let read = (&mut f).take(RECORD_CAP as u64 + 1).read_to_end(&mut bytes);
        // Asked after the record is read: a winner that took the lock since
        // is seen holding it, whatever record the read saw.
        let lock = locked(&f);
        while_open(path);
        drop(f);
        if read.is_err() {
            return kept(file, Sight::Unproven);
        }
        if bytes.len() > RECORD_CAP {
            return kept(file, Sight::Bad);
        }
        kept(
            file,
            match Record::parse(&bytes) {
                // A winner between its truncate and its write.
                RecordRead::Empty if lock == Some(true) => Sight::Unproven,
                RecordRead::Empty => Sight::Empty,
                RecordRead::Bad => Sight::Bad,
                RecordRead::Rec(rec) => holder(rec, file, lock),
            },
        )
    }

    /// Whether any process holds a lock on `f`'s file now: `fcntl(F_GETLK)`
    /// for the whole file, a query that takes no lock and leaves none, and on
    /// Darwin sees `flock` locks too (they share the vnode's one lock list),
    /// whoever's they are. `None` when the kernel gives no answer (a file
    /// system that keeps no local locks).
    pub(super) fn locked(f: &std::fs::File) -> Option<bool> {
        use std::os::fd::AsRawFd as _;
        let mut query = libc::flock {
            l_start: 0,
            l_len: 0,
            l_pid: 0,
            l_type: libc::F_WRLCK as libc::c_short,
            l_whence: libc::SEEK_SET as libc::c_short,
        };
        // SAFETY: `f` is an open descriptor this function borrows, and
        // `query` a `struct flock` `F_GETLK` reads and writes in place.
        let rc = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_GETLK, &raw mut query) };
        (rc != -1).then_some(query.l_type != libc::F_UNLCK as libc::c_short)
    }

    /// Whether `rec`'s writer holds `file` now (module docs), the file's lock
    /// as [`locked`] found it (`lock`) having the last word on not held.
    pub(super) fn holder(rec: Record, file: (u32, u64), lock: Option<bool>) -> Sight {
        match (lock, by_descriptor(rec, file)) {
            // Someone holds the lock whom the record does not show holding
            // the file: a winner between its `flock` and its record, or
            // `status` probing. Never free.
            (Some(true), Sight::NotHeld { .. }) => Sight::Unproven,
            // Nobody holds the lock: a living writer whose descriptors could
            // not be read has let go.
            (Some(false), Sight::Unproven) => Sight::NotHeld { gone: None },
            (_, sight) => sight,
        }
    }

    /// Whether `rec`'s writer holds `file` open now, by its descriptors.
    fn by_descriptor(rec: Record, file: (u32, u64)) -> Sight {
        let alive = |p: &kproc::Record| !p.zombie;
        let Some(p) = kproc::record(rec.pid).filter(alive) else {
            return Sight::NotHeld { gone: Some(rec) };
        };
        let birth = birth_us(&p);
        if birth > rec.started_us.saturating_add(BIRTH_SLACK_US) {
            // Born after the record was written: another process with the
            // writer's pid.
            return Sight::NotHeld { gone: Some(rec) };
        }
        match crate::jobs::macos_vnode_ids(rec.pid) {
            Some((ids, _)) if ids.contains(&file) => Sight::Sighted(Holder {
                pid: rec.pid,
                birth_us: birth,
            }),
            Some((_, false)) => Sight::NotHeld { gone: None },
            Some((_, true)) => Sight::Unproven,
            // Refused: the writer may have exited since.
            None => match kproc::record(rec.pid).filter(alive) {
                Some(q) if birth_us(&q) == birth => Sight::Unproven,
                _ => Sight::NotHeld { gone: Some(rec) },
            },
        }
    }

    /// [`super::live_witness`] on macOS. A process witnesses one candidate:
    /// once it has, it is no witness of another, so one orphaned build never
    /// discounts two slots.
    pub(super) fn witness() -> impl FnMut(&Record) -> bool {
        let mut table: Option<Option<Vec<kproc::Record>>> = None;
        let mut used: Vec<u32> = Vec::new();
        // SAFETY: `geteuid` reads this process's effective uid.
        let uid = unsafe { libc::geteuid() };
        move |rec: &Record| {
            let Some(cwd) = rec.cwd.as_deref() else {
                return false;
            };
            let found = table
                .get_or_insert_with(kproc::records)
                .iter()
                .flatten()
                .filter(|p| !used.contains(&p.pid) && adopted_in_window(p, rec, uid))
                .find(|p| {
                    i32::try_from(p.pid)
                        .ok()
                        .and_then(crate::jobs::macos_kinfo)
                        .is_some_and(|k| k.session_leader)
                        && crate::process_cwd(p.pid).as_deref() == Some(cwd)
                })
                .map(|p| p.pid);
            found.inspect(|pid| used.push(*pid)).is_some()
        }
    }
}

#[cfg(test)]
#[path = "slots_tests.rs"]
mod tests;
