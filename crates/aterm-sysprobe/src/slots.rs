// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Lock-free reader for Trust `build-slot` occupancy and orphaned builds.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Relative path under `$HOME` where `build-slot` stores slot files.
pub const SLOT_DIR: &str = ".cache/build-slot";
/// Maximum byte length of a valid slot record (`16 KiB`).
pub const RECORD_CAP: usize = 16 << 10;
/// Minimum gap in milliseconds between two paired slot observations.
pub const MIN_GAP_MS: u64 = 1_000;
/// Polling period while a wait names build slots.
pub const PERIOD: Duration = Duration::from_secs(5);
/// Polling period when no wait names build slots.
pub const IDLE_PERIOD: Duration = Duration::from_secs(60);
/// Log line emitted when a slot wait arms [`PERIOD`].
pub const ARMED_LINE: &str = "occupancy: build slots read every 5 s (a wait names them)";
/// Log line emitted when the last slot wait finishes and resets to [`IDLE_PERIOD`].
pub const AT_REST_LINE: &str = "occupancy: build slots read once a minute (no wait names them)";
/// Maximum age in microseconds for considering an orphaned build record (12 hours).
pub const ORPHAN_HORIZON_US: u64 = 12 * 3600 * 1_000_000;
/// Maximum birth window in microseconds after `started` for matching an orphan witness.
pub const WITNESS_WINDOW_US: u64 = 10_000_000;
/// Clock slack in microseconds allowed between `started` and holder birth.
pub const BIRTH_SLACK_US: u64 = 1_000;
/// Maximum directory entries inspected per read.
pub const MAX_ENTRIES: usize = 1024;
/// Maximum slots retained per read (lowest indices).
pub const MAX_SLOTS: usize = 256;

/// Reason why build slots could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unread {
    /// Platform has no slot reader.
    Unsupported,
    /// Slot directory is missing or contains no slot files.
    NotSampled,
    /// Slot directory or all slot files were unreadable by this user.
    Refused,
    /// Every slot file held an unrecognized record format.
    UnreadFormat,
}

/// Returns `<home>/.cache/build-slot`.
#[must_use]
pub fn default_dir(home: &Path) -> PathBuf {
    home.join(SLOT_DIR)
}

/// Parsed metadata from a `slot-<i>` record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// Wrapper process ID.
    pub pid: u32,
    /// Wrapper start time in microseconds since the Unix epoch.
    pub started_us: u64,
    /// Wrapper working directory.
    pub cwd: Option<PathBuf>,
}

/// Outcome of parsing a slot file's bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordRead {
    /// Empty file.
    Empty,
    /// Unrecognized or malformed record.
    Bad,
    /// Valid [`Record`].
    Rec(Record),
}

impl Record {
    /// Parses a `build-slot` JSON record from `bytes`.
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
                started_us: (started * 1e6) as u64,
                cwd: v.get("cwd").and_then(Value::as_str).map(PathBuf::from),
            }),
            _ => RecordRead::Bad,
        }
    }
}

/// Process holding a build slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Holder {
    /// Process ID.
    pub pid: u32,
    /// Process birth time in microseconds since the Unix epoch.
    pub birth_us: u64,
}

impl Holder {
    /// Returns `true` if `self.pid` is still running with `self.birth_us`.
    #[must_use]
    pub fn running(&self) -> bool {
        crate::kproc::record(self.pid).is_some_and(|p| birth_us(&p) == self.birth_us)
    }
}

/// Single-read observation of one slot file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sight {
    /// Empty slot file.
    Empty,
    /// Non-regular, oversized, or unparseable slot file.
    Bad,
    /// Slot state could not be proven on this read.
    Unproven,
    /// Writer does not hold the file open.
    NotHeld {
        /// Record of a writer that is dead or had its PID reused.
        gone: Option<Record>,
    },
    /// Writer holds the slot file open.
    Sighted(Holder),
}

/// A missing holder is proven only by an unlocked file. A refused query
/// keeps it unproven; positive descriptor evidence still stands.
#[cfg(any(target_os = "macos", test))]
fn lock_checked(sight: Sight, lock: Option<bool>) -> Sight {
    match (lock, sight) {
        (Some(false), Sight::Unproven) => Sight::NotHeld { gone: None },
        (Some(false), sight) => sight,
        (_, Sight::Empty | Sight::NotHeld { .. }) => Sight::Unproven,
        (_, sight) => sight,
    }
}

/// Observation of a single `slot-<i>` file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotObs {
    /// Slot index `i`.
    pub index: u32,
    /// Slot file `(dev, ino)` identity.
    pub file: (u32, u64),
    /// Observed [`Sight`].
    pub sight: Sight,
}

/// Single point-in-time read of a slot directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observation {
    /// Caller monotonic timestamp in milliseconds.
    pub mono_ms: u64,
    /// Wall-clock timestamp in microseconds since the Unix epoch.
    pub now_us: u64,
    /// Observed slots ordered by index.
    pub slots: Vec<SlotObs>,
}

/// Slot state paired across two reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotState {
    /// Held by the same process on both reads.
    Held(Holder),
    /// Unheld on both reads.
    Free,
    /// Single sighting or transition awaiting confirmation.
    Pending,
    /// Unproven or malformed on the latest read.
    Unknown,
}

/// Summary counts across all observed slots.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SlotCounts {
    /// Total slots present.
    pub n: u16,
    /// Held slots.
    pub held: u16,
    /// Free slots (including `discounted`).
    pub free: u16,
    /// Pending slots.
    pub pending: u16,
    /// Unknown slots.
    pub unknown: u16,
    /// Free slots discounted due to orphaned builds under load.
    pub discounted: u16,
}

/// Proven slot reading.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlotsRead {
    /// Slots could not be read.
    Unread(Unread),
    /// Slots were read and judged.
    Read {
        /// Summary counts.
        counts: SlotCounts,
        /// Active slot holders ordered by slot index.
        holders: Vec<Holder>,
    },
}

/// Machine load facts used when discounting orphaned builds.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Machine {
    /// Five-minute load average.
    pub load5: Option<f64>,
    /// Logical CPU count.
    pub ncpu: Option<usize>,
}

/// Paired slot observation state across reads.
#[derive(Clone, Debug, Default)]
pub struct SlotBook {
    prev: Option<Observation>,
    last: Option<SlotsRead>,
}

impl SlotBook {
    /// Judges `obs` against the previously kept read, discounting orphaned builds via `witness`.
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
            .and_then(|p| {
                p.slots
                    .iter()
                    .find(|q| q.index == s.index && q.file == s.file)
            })
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

/// How many of the free slots' gone writers' builds run on:
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

/// Returns `true` if `p` matches an orphaned build spawned by `rec` within `now_us`.
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

/// Reads all slots in `dir` at `(mono_ms, now_us)`.
/// # Errors
/// Returns [`Unread`] if `dir` is unsupported, missing, refused, or unreadable.
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

/// Cached build-slot reader respecting [`PERIOD`] and [`IDLE_PERIOD`].
#[derive(Debug)]
pub struct SlotReader {
    dir: PathBuf,
    book: SlotBook,
    origin: Option<Instant>,
    last: Option<(Instant, SlotsRead)>,
}

impl SlotReader {
    /// Creates a reader for `dir`.
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            book: SlotBook::default(),
            origin: None,
            last: None,
        }
    }

    /// Returns the slot directory path.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Returns the current or cached [`SlotsRead`] at `(now, now_us)`.
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

    /// Returns when the cached reading was taken, if any.
    #[must_use]
    pub fn read_at(&self) -> Option<Instant> {
        self.last.as_ref().map(|(at, _)| *at)
    }
}

/// Returns a closure that checks whether a gone slot record has a live orphaned build witness.
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
        RecordRead, Sight, SlotObs, Unread, adopted_in_window, birth_us, lock_checked, slot_index,
    };
    use crate::kproc;

    /// One slot's read: kept, vanished between the listing and the read, or
    /// refused to this user.
    enum Got {
        Slot(SlotObs),
        Vanished,
        Refused,
    }

    /// Implements [`observe`] with a test hook invoked while each slot file is open.
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
                RecordRead::Empty => lock_checked(Sight::Empty, lock),
                RecordRead::Bad => Sight::Bad,
                RecordRead::Rec(rec) => holder(rec, file, lock),
            },
        )
    }

    /// Queries `fcntl(F_GETLK)` on `f` without blocking.
    pub(super) fn locked(f: &std::fs::File) -> Option<bool> {
        use std::os::fd::AsRawFd as _;
        let mut query = libc::flock {
            l_start: 0,
            l_len: 0,
            l_pid: 0,
            l_type: libc::F_WRLCK as libc::c_short,
            l_whence: libc::SEEK_SET as libc::c_short,
        };
        // SAFETY: `f` is an open borrowed descriptor and `query` is a valid `flock` struct.
        let rc = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_GETLK, &raw mut query) };
        (rc != -1).then_some(query.l_type != libc::F_UNLCK as libc::c_short)
    }

    /// Determines whether `rec`'s writer currently holds `file`.
    pub(super) fn holder(rec: Record, file: (u32, u64), lock: Option<bool>) -> Sight {
        lock_checked(by_descriptor(rec, file), lock)
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
            None => match kproc::record(rec.pid).filter(alive) {
                Some(q) if birth_us(&q) == birth => Sight::Unproven,
                _ => Sight::NotHeld { gone: Some(rec) },
            },
        }
    }

    /// macOS implementation of [`live_witness`].
    pub(super) fn witness() -> impl FnMut(&Record) -> bool {
        let mut table: Option<Option<Vec<kproc::Record>>> = None;
        let mut used: Vec<u32> = Vec::new();
        // SAFETY: `geteuid` is always safe to call.
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
