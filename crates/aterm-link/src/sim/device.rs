// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE STATE DIR'S DEVICE: aterm's own model of what a power cut keeps of the
//! bridge's state-dir writes, behind the state-dir door ([`crate::fx::Disk`]).
//!
//! The state dir writes a scratch directory for real, and every write it
//! makes is journalled here as it is made ([`Device::journal`]): what each
//! file held when it was last ON THE MEDIA, and every version written since.
//! A power cut ([`Device::power_cut`]) rewrites the directory to one image of
//! it, file by file, chosen by the seed among what aterm-durable's contract
//! (`aterm_durable`'s crate doc) lets the media hold:
//!
//! * a DURABLE replace — the temp flushed before the rename, the directory
//!   after it (`StateDir::write_durable`) — is on the media once it returns:
//!   the file holds it, whatever came before;
//! * an ATOMIC-ONLY replace (temp + rename, nothing flushed) may have reached
//!   the media or not: the file holds the version before it, or it, or any
//!   later one, WHOLE — the model the state dir is written against (its module
//!   doc: "a reader sees the old value or the new one, never a truncation");
//! * an unlink may or may not have reached the media (a landed
//!   post's in-flight entry came back, and its republish landed it behind its
//!   sender's later posts where the log had lost it on its own; the next
//!   boot's first bridge now retires what a cut brings back,
//!   `StateDir::retire_in_flight_of_an_earlier_boot`);
//! * an append (`acked`) may have reached it whole or not at all;
//! * a directory flush puts every ENTRY change in that directory on the
//!   media — an unlink is kept, a rename's older versions can no longer come
//!   back (its data still may be torn), a file an append created stays — and
//!   no file's unflushed data: an append after the newest entry change may
//!   still be lost. The state dir flushes a directory on its own
//!   (`StateDir::clear_rewound_log`'s, `StateDir::drop_in_flight`'s) and as
//!   the second half of every durable replace, which flushes the replaced
//!   file's directory for its SIBLINGS as much as for it (`replace_bytes`'s
//!   `sync_dir`): an unlink made before a durable write beside it is kept;
//! * an ORDERED write (`aterm_durable::ordered`, `F_BARRIERFSYNC`: a clean
//!   stop's, `StateDir::end_run_clean`) is a FLUSH POINT: what it flushed —
//!   a file's bytes as they stand (an in-place flush), an ordered replace's
//!   bytes, whose rename comes after it, or an ordered directory's entry
//!   changes — reaches the media before ANY write issued after it, flushed
//!   or not. A full flush is one too, and puts every write
//!   flushed before it on the media ("a successful full flush also persists
//!   the earlier, already-flushed I/O on its device"). So a power cut draws
//!   how many of the flush points since the last full flush the media holds
//!   (`k`): what the first `k` flushed is kept, and nothing issued after the
//!   `k`-th's successor is ([`Journal::points`]).
//!
//! Within what the flushes fixed, each file's image is drawn on its own:
//! nothing orders two files' unflushed writes against each other, and no
//! cross-log/state ordering of a real device is modelled. A process kill (not
//! a power cut) keeps everything: the page cache outlives the process.
//!
//! ## Torn files: POSIX's worst case
//!
//! POSIX promises nothing of an unflushed file's DATA across a power loss, so
//! a rename that reached the media may name a file whose bytes did not — an
//! empty or cut file. [`Device::tears`] lets a power cut leave that too (an
//! atomic-only replace cut at any byte short of its last, nothing included;
//! an append cut at any byte), and the committed worlds' devices do
//! (`world::Opts::seed`).
//!
//! a torn `inflight/<seq>`
//! (`StateDir::in_flight`) or `sent/` reservation
//! (`StateDir::forget_log_offsets`) was refused as damaged, fail-closed, at
//! every attach of every later boot, and the bridge never attached again until
//! a person repaired the dir — while the state dir's own table calls losing
//! those files harmless after a reboot, which orphans every session they
//! served. Decided for the table: after an unclean stop in an earlier boot,
//! the bridge moves each file of those kinds that is not provably whole into
//! the state dir's quarantine and goes on (`StateDir::set_aside_torn`);
//! within one boot, and after a clean stop, damage is still refused. Its
//! negative controls are the old refusal and the first fix's sweep, which
//! kept an in-flight reply cut where it still reads
//! (`tests::a_torn_state_file_after_a_power_cut_is_set_aside_and_the_attach_goes_on`).
//!
//! A cut number reads as a prefix, a LOWER number (a cursor that re-reads, a
//! watermark that re-answers, a `seq` the durable `inc` fences), and every
//! reader that refuses damage is one of `StateDir::set_aside_torn`'s kinds —
//! but `logpin`, the mirror's pin, which holds what no reboot ends (the alias
//! key and the lifetime total). It used to be kept torn,
//! and the mirror stopped for good. Both now have durable copies, `logkey`
//! and `logfloor`, on the media before each atomic batch pin or send. The
//! current version fence is first written durably, before those copies;
//! a new open reestablishes it. So the next boot's first bridge sets a torn
//! pin aside too and the mirror resumes
//! (`StateDir::reconcile_logpin`); the world holds it to the key and to a
//! total never below the highest one a pin was written with
//! ([`Device::pin_total`]).
//!
//! ## Write errors (sim item S7)
//!
//! Armed ([`Device::arm_write_errors`], the committed worlds' every other
//! eight seeds), the device fails the state dir's writes from a stream of
//! their own, one in `per` (20, 50 or 120, the seed's):
//!
//! * BEFORE THE EFFECT ([`Device::fails_before`], the door's `before`): a
//!   replace — its temp's write, its flush, its rename — or an append, `EIO`
//!   or `ENOSPC`; an unlink, `EIO` (or `ENOSPC`, as a copy-on-write volume
//!   whose metadata takes space can answer); a flush of a file or a
//!   directory, `EIO`. Nothing is made: the temp is removed, nothing renamed.
//! * AFTER IT ([`Device::made`], the door's `after`): a durable replace whose
//!   rename is made and whose directory flush fails — its temp's flush
//!   returned, so its bytes are on the media whole, and its name may be or
//!   not, until a later flush of its directory; and an append cut part-way,
//!   `ENOSPC`, the part written in the file and in the journal.
//! * A FULL DISK while the scheduler says so ([`Device::fill`]): every write
//!   that takes space fails `ENOSPC`; unlinks and flushes go on.
//!
//! The settle phase's disk is clean ([`Device::clean_disk`]). Not modelled: a
//! failed flush that drops dirty pages it did not write (a later read of the
//! file sees an older version within the boot), and a write that reports
//! success and is lost — a disk that lies is no contract to hold a bridge to.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use aterm_durable::Flush;

use super::{lock, Rng};
use crate::fx::Write;

/// `EIO`, as macOS and Linux both number it: the device failed the I/O.
const EIO: i32 = 5;
/// `ENOSPC`, as macOS and Linux both number it: no space left on the device.
const ENOSPC: i32 = 28;

/// The name of a simulated write error, for the trace: the same on every
/// machine, where the OS's message for it is not.
pub(crate) fn errno_name(e: &io::Error) -> &'static str {
    match e.raw_os_error() {
        Some(EIO) => "EIO",
        Some(ENOSPC) => "ENOSPC",
        _ => "error",
    }
}

/// WHAT THE DEVICE FAILED (sim item S7), counted by kind: the world reads
/// them into its stats, so a run that injected none cannot pass for one that
/// did.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct WriteErrors {
    /// A replace (its temp's write, its flush or its rename) or an append
    /// that failed with nothing made, `EIO` or `ENOSPC`.
    pub(crate) writes: u64,
    /// An unlink that failed, nothing removed.
    pub(crate) unlinks: u64,
    /// A flush of a file where it is, or of a directory, that failed, nothing
    /// more on the media for it.
    pub(crate) flushes: u64,
    /// A durable replace whose rename was made and whose directory flush then
    /// failed: the new version stands, its name not known to be on the media.
    pub(crate) renamed_unflushed: u64,
    /// An append that wrote part of its line and failed `ENOSPC`.
    pub(crate) appends_cut: u64,
    /// Writes refused `ENOSPC` because the disk was full ([`Device::fill`]).
    pub(crate) disk_full: u64,
}

/// THE DEVICE'S WRITE ERRORS (sim item S7): `EIO` and `ENOSPC` on a write, a
/// rename, a flush or an unlink, drawn from a stream of their own at a rate
/// the seed chose, and a full disk while the scheduler says so.
#[derive(Debug)]
struct Faults {
    rng: Rng,
    /// One write in `per` fails before its effect, and one in `per` of the
    /// writes that can fail after it does, each by its own draw.
    per: u64,
    /// The disk is full: every write that takes space fails `ENOSPC`.
    full: bool,
    /// Off from the settle phase on: a clean disk.
    on: bool,
    counts: WriteErrors,
}

/// How a write's last step failed after its effect was made
/// ([`Device::made`]).
#[derive(Clone, Copy, Debug)]
enum After {
    /// A durable replace's rename was made and its directory flush failed.
    Renamed,
    /// An append wrote this many bytes of its line, short of all of them.
    Cut(usize),
}

/// What a file held at one point: nothing, or bytes under a mode.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Version {
    Absent,
    Bytes(Vec<u8>, u32),
}

/// One write since the file was last on the media.
#[derive(Clone, Debug)]
enum Op {
    Replace(Vec<u8>, u32),
    Append(Vec<u8>),
    Remove,
}

/// One write since the file was last on the media, and where it stands
/// against the device's flush points ([`Journal::points`]).
#[derive(Clone, Debug)]
struct Pending {
    op: Op,
    /// The flush points issued before it: it can be on the media only if the
    /// writes flushed before the last of them are.
    epoch: u64,
    /// The flush point that flushed its bytes, for an ordered replace: they
    /// are on the media whole once that point is, whether or not its rename
    /// (issued after it) is.
    data_at: Option<u64>,
}

/// One file's journal.
#[derive(Clone, Debug)]
struct File {
    /// What the media holds for sure.
    durable: Version,
    /// Every write since, oldest first.
    pending: Vec<Pending>,
    /// How many of `pending`'s versions a flushed directory entry rules out:
    /// an image may hold version `k` only for `k + 1 >= floor` (`durable` is
    /// version `-1`).
    floor: usize,
}

impl File {
    /// A file the media holds nothing of, with nothing written since.
    fn blank() -> Self {
        Self {
            durable: Version::Absent,
            pending: Vec::new(),
            floor: 0,
        }
    }

    /// The versions an image may hold, oldest first, of the first `keep`
    /// writes with `floor` ruled out, each with how it may be torn:
    /// `(version, torn_from, torn_whole)` — a torn image of a replace is cut
    /// anywhere short of its last byte (`torn_whole`: one whose bytes no
    /// flush point at or below `cut` put on the media), of an append any
    /// prefix at or above `torn_from` bytes.
    fn versions(&self, keep: usize, floor: usize, cut: u64) -> Vec<(Version, Option<usize>, bool)> {
        let mut out = Vec::new();
        if floor == 0 {
            out.push((self.durable.clone(), None, false));
        }
        let mut now = self.durable.clone();
        for (k, p) in self.pending[..keep].iter().enumerate() {
            let (next, torn_from, replace) = match (&p.op, &now) {
                (Op::Replace(bytes, mode), _) => (
                    Version::Bytes(bytes.clone(), *mode),
                    None,
                    p.data_at.is_none_or(|at| at >= cut),
                ),
                (Op::Remove, _) => (Version::Absent, None, false),
                (Op::Append(bytes), Version::Bytes(have, mode)) => {
                    let mut joined = have.clone();
                    joined.extend_from_slice(bytes);
                    (Version::Bytes(joined, *mode), Some(have.len()), false)
                }
                (Op::Append(bytes), Version::Absent) => {
                    (Version::Bytes(bytes.clone(), 0o666), Some(0), false)
                }
            };
            now = next.clone();
            if k + 1 >= floor {
                out.push((next, torn_from, replace));
            }
        }
        out
    }
}

/// The newest ENTRY change among `pending` (a rename's version, an unlink,
/// the file an append created), over a file whose media holds `durable`:
/// what a flush of its directory puts on the media.
fn newest_entry(durable: &Version, pending: &[Pending]) -> Option<usize> {
    let mut absent = *durable == Version::Absent;
    let mut entry = None;
    for (k, p) in pending.iter().enumerate() {
        if !matches!(p.op, Op::Append(_)) || absent {
            entry = Some(k);
        }
        absent = matches!(p.op, Op::Remove);
    }
    entry
}

/// The whole journal under one lock: every file's, and the flush points.
#[derive(Debug, Default)]
struct Journal {
    files: BTreeMap<PathBuf, File>,
    /// FLUSH POINTS issued since the last power cut: every ordered write and
    /// every full flush is one, numbered from 0 in the order issued.
    points: u64,
    /// How many of them are on the media for sure: every one up to the newest
    /// full flush.
    persisted: u64,
    /// Each ordered directory flush: its flush point, the directory, and how
    /// many of each of its files' pending writes it covers.
    dir_points: Vec<(u64, PathBuf, BTreeMap<PathBuf, usize>)>,
}

impl Journal {
    /// A FULL FLUSH OF DIRECTORY `dir` (the module doc): each file in it keeps
    /// its newest ENTRY change, and the versions before it can no longer come
    /// back; the appends after it are data, and still may be lost.
    fn flush_dir(&mut self, dir: &Path) {
        let mut cleared = Vec::new();
        for (p, f) in &mut self.files {
            if p.parent() != Some(dir) {
                continue;
            }
            let Some(j) = newest_entry(&f.durable, &f.pending) else {
                continue;
            };
            if j + 1 == f.pending.len() && matches!(f.pending[j].op, Op::Remove) {
                f.durable = Version::Absent;
                f.pending.clear();
                f.floor = 0;
                cleared.push(p.clone());
            } else {
                f.floor = f.floor.max(j + 1);
            }
        }
        for p in cleared {
            self.forget(&p);
        }
    }

    /// `path`'s pending writes were settled on the media: no ordered
    /// directory flush covers any of them any more.
    fn forget(&mut self, path: &Path) {
        for (_, _, lens) in &mut self.dir_points {
            lens.remove(path);
        }
    }

    /// A full flush was issued: it is a flush point, and every one before it
    /// is on the media.
    fn full_flush(&mut self) {
        self.points += 1;
        self.persisted = self.points;
    }
}

/// THE DEVICE under one world's state dir.
#[derive(Debug)]
pub(crate) struct Device {
    root: PathBuf,
    journal: Mutex<Journal>,
    /// A CONTROL: a device that keeps every write (no power cut loses
    /// anything), so a failure only a lossy cut can cause vanishes under it.
    pub(crate) lossless: bool,
    /// POSIX's worst case: a power cut may also TEAR an unflushed file.
    /// On in the committed worlds.
    pub(crate) tears: bool,
    /// THE HIGHEST TOTAL any write of `logpin` named, in any boot: what the
    /// node's mirror had counted, which no later bridge's total
    /// may fall below, whatever a power cut did to the pin.
    pin_total: Mutex<u64>,
    /// WRITE ERRORS (sim item S7), once armed ([`Device::arm_write_errors`]).
    faults: Mutex<Option<Faults>>,
    /// The path whose next durable replace fails after its rename, once
    /// ([`Device::fail_after_its_rename`]): a test's exact fault.
    after_once: Mutex<Option<PathBuf>>,
}

impl Device {
    pub(crate) fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            journal: Mutex::new(Journal::default()),
            lossless: false,
            tears: false,
            pin_total: Mutex::new(0),
            faults: Mutex::new(None),
            after_once: Mutex::new(None),
        }
    }

    /// The next durable replace of `path` has its rename made and its
    /// directory flush fail (`EIO`), once — [`Device::made`]'s
    /// [`After::Renamed`], where a test needs it and not where a seed draws it.
    pub(crate) fn fail_after_its_rename(&self, path: &Path) {
        *lock(&self.after_once) = Some(path.to_path_buf());
    }

    /// ARM WRITE ERRORS (sim item S7): from now on one write in `per` fails
    /// with nothing made, and one in `per` of the durable replaces and
    /// appends fails after its effect, each by a draw of `rng`.
    pub(crate) fn arm_write_errors(&self, rng: Rng, per: u64) {
        *lock(&self.faults) = Some(Faults {
            rng,
            per: per.max(2),
            full: false,
            on: true,
            counts: WriteErrors::default(),
        });
    }

    /// The disk fills (`true`) or has room again (`false`): while it is full,
    /// every write that takes space — a replace's temp, an append — fails
    /// `ENOSPC`. Nothing where write errors are not armed.
    pub(crate) fn fill(&self, full: bool) {
        if let Some(f) = lock(&self.faults).as_mut().filter(|f| f.on) {
            f.full = full;
        }
    }

    /// Whether the disk is full now ([`Device::fill`]).
    pub(crate) fn is_full(&self) -> bool {
        lock(&self.faults).as_ref().is_some_and(|f| f.on && f.full)
    }

    /// A CLEAN DISK from now on: no write fails, the disk has room. The
    /// settle phase's, so a wedge a write error left is one a clean disk
    /// does not heal.
    pub(crate) fn clean_disk(&self) {
        if let Some(f) = lock(&self.faults).as_mut() {
            f.on = false;
            f.full = false;
        }
    }

    /// What the device failed so far ([`WriteErrors`]).
    pub(crate) fn write_errors(&self) -> WriteErrors {
        lock(&self.faults)
            .as_ref()
            .map(|f| f.counts)
            .unwrap_or_default()
    }

    /// `w` as the trace names it: its kind, and its path inside the state dir.
    pub(crate) fn describe(&self, w: Write<'_>) -> String {
        let rel = |p: &Path| {
            let rel = p
                .strip_prefix(&self.root)
                .unwrap_or(p)
                .display()
                .to_string();
            if rel.is_empty() {
                ".".to_string()
            } else {
                rel
            }
        };
        match w {
            Write::Replace { path, flush, .. } => format!(
                "replace {}{}",
                rel(path),
                match flush {
                    Some(Flush::Durable) => " durable",
                    Some(Flush::Ordered) => " ordered",
                    None => "",
                }
            ),
            Write::Append { path, .. } => format!("append {}", rel(path)),
            Write::Remove { path } => format!("unlink {}", rel(path)),
            Write::Flush { path, .. } => format!("flush {}", rel(path)),
            Write::SyncDir { path, .. } => format!("flush dir {}", rel(path)),
        }
    }

    /// WHETHER `w` FAILS BEFORE ITS EFFECT ([`crate::fx::Disk::before`]): on
    /// a full disk a write that takes space fails `ENOSPC`; otherwise, one
    /// write in `per` fails — a replace or an append `EIO` or `ENOSPC` (its
    /// temp's write, its flush, its rename: the temp is removed, nothing is
    /// renamed), an unlink `EIO` (or, on a copy-on-write volume such as APFS,
    /// whose metadata takes space, `ENOSPC`), a flush `EIO`. Nothing is made
    /// of a write that fails here.
    pub(crate) fn fails_before(&self, w: Write<'_>) -> Option<io::Error> {
        let mut faults = lock(&self.faults);
        let f = faults.as_mut().filter(|f| f.on)?;
        let takes_space = matches!(w, Write::Replace { .. } | Write::Append { .. });
        if f.full && takes_space {
            f.counts.disk_full += 1;
            return Some(io::Error::from_raw_os_error(ENOSPC));
        }
        if !f.rng.chance(1, f.per) {
            return None;
        }
        let errno = match w {
            Write::Replace { .. } | Write::Append { .. } => {
                f.counts.writes += 1;
                if f.rng.chance(1, 2) {
                    ENOSPC
                } else {
                    EIO
                }
            }
            Write::Remove { .. } => {
                f.counts.unlinks += 1;
                if f.rng.chance(1, 4) {
                    ENOSPC
                } else {
                    EIO
                }
            }
            Write::Flush { .. } | Write::SyncDir { .. } => {
                f.counts.flushes += 1;
                EIO
            }
        };
        Some(io::Error::from_raw_os_error(errno))
    }

    /// Whether `w`, made, fails its last step after its effect: one in `per`
    /// of the durable replaces (its rename made, its directory flush `EIO`)
    /// and of the appends (part of its line written, `ENOSPC`). Nothing else
    /// has a step after its effect: an atomic replace's rename, an unlink and
    /// a flush are all or nothing.
    fn fails_after(&self, w: Write<'_>) -> Option<After> {
        if let Write::Replace {
            path,
            flush: Some(Flush::Durable),
            ..
        } = w
        {
            let mut once = lock(&self.after_once);
            if once.as_deref() == Some(path) {
                *once = None;
                return Some(After::Renamed);
            }
        }
        let mut faults = lock(&self.faults);
        let f = faults.as_mut().filter(|f| f.on)?;
        match w {
            Write::Replace {
                flush: Some(Flush::Durable),
                ..
            } if f.rng.chance(1, f.per) => {
                f.counts.renamed_unflushed += 1;
                Some(After::Renamed)
            }
            Write::Append { bytes, .. } if !bytes.is_empty() && f.rng.chance(1, f.per) => {
                f.counts.appends_cut += 1;
                let len = bytes.len() as u64;
                Some(After::Cut(usize::try_from(f.rng.below(len)).unwrap_or(0)))
            }
            _ => None,
        }
    }

    /// THE STATE DIR MADE `w` ([`crate::fx::Disk::after`]): journal what the
    /// media may hold of it, unless its last step fails after its effect
    /// ([`Device::fails_after`]) — then journal what that left, and answer the
    /// error the real call returns:
    ///
    /// * a durable replace whose directory flush failed: its temp's flush
    ///   returned, so its bytes are on the media whole (a full flush point),
    ///   and its rename is made and not known to be — a later flush of its
    ///   directory settles it, a power cut before one keeps it or not;
    /// * an append cut short: the file holds the part written, and the media
    ///   may hold it, as any append's.
    pub(crate) fn made(&self, w: Write<'_>) -> io::Result<()> {
        match (self.fails_after(w), w) {
            (
                Some(After::Renamed),
                Write::Replace {
                    path, bytes, mode, ..
                },
            ) => {
                self.note_pin(path, bytes);
                let mut j = lock(&self.journal);
                let at = j.points;
                j.full_flush();
                j.files
                    .entry(path.to_path_buf())
                    .or_insert_with(File::blank)
                    .pending
                    .push(Pending {
                        op: Op::Replace(bytes.to_vec(), mode),
                        epoch: at + 1,
                        data_at: Some(at),
                    });
                Err(io::Error::from_raw_os_error(EIO))
            }
            (Some(After::Cut(k)), Write::Append { path, bytes }) => {
                let len = std::fs::metadata(path).expect("the appended file").len();
                let keep = len - bytes.len() as u64 + k as u64;
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(path)
                    .and_then(|f| f.set_len(keep))
                    .expect("cut an append short");
                self.journal(Write::Append {
                    path,
                    bytes: &bytes[..k],
                });
                Err(io::Error::from_raw_os_error(ENOSPC))
            }
            _ => {
                self.journal(w);
                Ok(())
            }
        }
    }

    /// The highest total any write of `logpin` named ([`Device::pin_total`]).
    pub(crate) fn pin_total(&self) -> u64 {
        *lock(&self.pin_total)
    }

    /// A file the world wrote before the bridge ever ran (the provisioned
    /// node id), on the media.
    pub(crate) fn seed(&self, path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).expect("seed a state file");
        lock(&self.journal).files.insert(
            path.to_path_buf(),
            File {
                durable: Version::Bytes(bytes.to_vec(), 0o666),
                pending: Vec::new(),
                floor: 0,
            },
        );
    }

    /// A replace of `path` by `bytes` was made: where it is the mirror's pin,
    /// the highest total any pin was written with ([`Device::pin_total`]).
    fn note_pin(&self, path: &Path, bytes: &[u8]) {
        if path == self.root.join(crate::state::LOGPIN) {
            if let Some(pin) = std::str::from_utf8(bytes)
                .ok()
                .and_then(|t| crate::mirror::Pin::parse(t).ok())
            {
                let mut max = lock(&self.pin_total);
                *max = (*max).max(pin.total);
            }
        }
    }

    /// Journal one write the state dir made, whole ([`Device::made`]).
    pub(crate) fn journal(&self, w: Write<'_>) {
        if let Write::Replace { path, bytes, .. } = &w {
            self.note_pin(path, bytes);
        }
        let mut j = lock(&self.journal);
        let blank = File::blank;
        let epoch = j.points;
        let push = |j: &mut Journal, path: &Path, p: Pending| {
            j.files
                .entry(path.to_path_buf())
                .or_insert_with(blank)
                .pending
                .push(p);
        };
        match w {
            Write::Replace {
                path,
                bytes,
                flush: Some(Flush::Durable),
                mode,
            } => {
                j.full_flush();
                let f = j.files.entry(path.to_path_buf()).or_insert_with(blank);
                f.durable = Version::Bytes(bytes.to_vec(), mode);
                f.pending.clear();
                f.floor = 0;
                j.forget(path);
                // The replace's directory flush is the directory's, for
                // every entry in it.
                if let Some(dir) = path.parent() {
                    j.flush_dir(dir);
                }
            }
            Write::Replace {
                path,
                bytes,
                flush: Some(Flush::Ordered),
                mode,
            } => {
                // The barrier flushed the temp's bytes; the rename comes
                // after it, a write of the next epoch.
                j.points += 1;
                push(
                    &mut j,
                    path,
                    Pending {
                        op: Op::Replace(bytes.to_vec(), mode),
                        epoch: epoch + 1,
                        data_at: Some(epoch),
                    },
                );
            }
            Write::Replace {
                path,
                bytes,
                flush: None,
                mode,
            } => push(
                &mut j,
                path,
                Pending {
                    op: Op::Replace(bytes.to_vec(), mode),
                    epoch,
                    data_at: None,
                },
            ),
            Write::Append { path, bytes } => push(
                &mut j,
                path,
                Pending {
                    op: Op::Append(bytes.to_vec()),
                    epoch,
                    data_at: None,
                },
            ),
            Write::Remove { path } => push(
                &mut j,
                path,
                Pending {
                    op: Op::Remove,
                    epoch,
                    data_at: None,
                },
            ),
            Write::Flush { path, flush } => {
                // The file's bytes as they stand — its newest version's, a
                // replace's whose rename came before — reach the media with
                // this flush point; its name is still its directory's.
                let at = j.points;
                match flush {
                    Flush::Durable => j.full_flush(),
                    Flush::Ordered => j.points += 1,
                }
                if let Some(last) = j.files.get_mut(path).and_then(|f| f.pending.last_mut()) {
                    if matches!(last.op, Op::Replace(..)) && last.data_at.is_none() {
                        last.data_at = Some(at);
                    }
                }
            }
            Write::SyncDir {
                path,
                flush: Flush::Durable,
            } => {
                j.full_flush();
                j.flush_dir(path);
            }
            Write::SyncDir {
                path,
                flush: Flush::Ordered,
            } => {
                let lens = j
                    .files
                    .iter()
                    .filter(|(p, _)| p.parent() == Some(path))
                    .map(|(p, f)| (p.clone(), f.pending.len()))
                    .collect();
                j.dir_points.push((epoch, path.to_path_buf(), lens));
                j.points += 1;
            }
        }
    }

    /// A POWER CUT: every file under the state dir rewritten to one image the
    /// media may hold (the module doc), drawn from `rng`; each image is then
    /// what the media holds. Answers one line per file whose image is not its
    /// newest version, for the trace.
    ///
    /// First how many flush points since the last full flush reached the
    /// media, `cut` (drawn only where any ordered write left one in doubt, so
    /// a world that orders nothing draws what it always drew): what the first
    /// `cut` flushed is kept — an ordered replace's bytes whole, an ordered
    /// directory's entries — and no write issued after the next one is.
    pub(crate) fn power_cut(&self, rng: &mut Rng) -> Vec<String> {
        let mut j = lock(&self.journal);
        let open = j.points - j.persisted;
        let cut = if self.lossless || open == 0 {
            j.points
        } else {
            j.persisted + rng.below(open + 1)
        };
        let mut notes = Vec::new();
        if cut < j.points {
            notes.push(format!(
                "power-cut flush points {}/{open} on the media",
                cut - j.persisted
            ));
        }
        notes.extend(self.cut_files(&mut j, rng, cut));
        notes
    }

    /// The image of every file once `cut` flush points are on the media
    /// ([`Device::power_cut`]).
    fn cut_files(&self, j: &mut Journal, rng: &mut Rng, cut: u64) -> Vec<String> {
        let dir_points = std::mem::take(&mut j.dir_points);
        let mut notes = Vec::new();
        for (path, f) in &mut j.files {
            // Nothing issued after the flush point past the cut's last.
            let keep = f.pending.iter().take_while(|p| p.epoch <= cut).count();
            let mut floor = f.floor;
            for (at, dir, lens) in &dir_points {
                if *at >= cut || path.parent() != Some(dir.as_path()) {
                    continue;
                }
                let covered = lens.get(path).copied().unwrap_or(0).min(keep);
                if let Some(e) = newest_entry(&f.durable, &f.pending[..covered]) {
                    floor = floor.max(e + 1);
                }
            }
            let versions = f.versions(keep, floor, cut);
            if versions.is_empty() {
                continue;
            }
            let newest = versions.len() - 1;
            let (pick, torn) = if self.lossless {
                (newest, false)
            } else {
                let pick = usize::try_from(rng.below(versions.len() as u64)).unwrap_or(newest);
                let tearable = self.tears && (versions[pick].1.is_some() || versions[pick].2);
                (pick, tearable && rng.chance(1, 4))
            };
            let (version, torn_from, replace) = versions[pick].clone();
            let image = match (torn, version) {
                // A replace torn: its data reached the media up to any byte
                // short of its last, nothing included.
                (true, Version::Bytes(bytes, mode)) if replace => {
                    let cut = usize::try_from(rng.below(bytes.len().max(1) as u64)).unwrap_or(0);
                    Version::Bytes(bytes[..cut.min(bytes.len())].to_vec(), mode)
                }
                (true, Version::Bytes(bytes, mode)) => {
                    let from = torn_from.unwrap_or(0);
                    let cut = from
                        + usize::try_from(rng.below((bytes.len() - from) as u64 + 1)).unwrap_or(0);
                    Version::Bytes(bytes[..cut.min(bytes.len())].to_vec(), mode)
                }
                (_, v) => v,
            };
            if pick != newest || torn || keep < f.pending.len() {
                let rel = path.strip_prefix(&self.root).unwrap_or(path);
                notes.push(format!(
                    "power-cut {} -> version {pick}/{newest}{}{}",
                    rel.display(),
                    if keep < f.pending.len() {
                        " (later writes past the cut)"
                    } else {
                        ""
                    },
                    if torn { " torn" } else { "" }
                ));
            }
            match &image {
                Version::Absent => {
                    let _ = std::fs::remove_file(path);
                }
                Version::Bytes(bytes, mode) => {
                    use std::os::unix::fs::PermissionsExt as _;
                    if let Some(parent) = path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    std::fs::write(path, bytes).expect("write a power-cut image");
                    let _ = std::fs::set_permissions(
                        path,
                        std::fs::Permissions::from_mode(*mode & 0o755),
                    );
                }
            }
            f.durable = image;
            f.pending.clear();
            f.floor = 0;
        }
        j.points = 0;
        j.persisted = 0;
        notes
    }
}
