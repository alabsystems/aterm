// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE STATE DIR'S DEVICE.

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

/// The name of a simulated write error, for the trace.
pub(crate) fn errno_name(e: &io::Error) -> &'static str {
    match e.raw_os_error() {
        Some(EIO) => "EIO",
        Some(ENOSPC) => "ENOSPC",
        _ => "error",
    }
}

/// WHAT THE DEVICE FAILED (sim item S7), counted by kind.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct WriteErrors {
    /// A replace (its temp's write, its flush or its rename) or an append that failed with nothing made, `EIO` or `ENOSPC`.
    pub(crate) writes: u64,
    /// An unlink that failed, nothing removed.
    pub(crate) unlinks: u64,
    /// A flush of a file where it is, or of a directory, that failed, nothing more on the media for it.
    pub(crate) flushes: u64,
    /// A durable replace whose rename was made and whose directory flush then failed.
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
    /// prefix at or above `torn_from` bytes, or its whole length with the bytes
    /// past such a prefix read as zeros (the size on the media, the bytes not).
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
    /// Each flush of a file where it is, either tier: its flush point, the
    /// file, and how many of its pending writes it covers. Once the point is
    /// on the media, so are those writes' bytes: a file whose entry is ruled
    /// in holds no version older than the last of them ([`data_floor`]).
    file_points: Vec<(u64, PathBuf, usize)>,
}

/// Whether a flush of a file's first `covered` writes, its point on the media,
/// puts them on the media for every image: over a file whose media holds
/// `durable`, where every entry change among them is ruled in by `floor` (a
/// flush of a file's bytes says nothing of a directory entry a write moved).
/// Then no image holds fewer of those writes, nor tears the last of them.
fn data_flushed(durable: &Version, pending: &[Pending], covered: usize, floor: usize) -> bool {
    covered > 0 && newest_entry(durable, &pending[..covered]).is_none_or(|e| e < floor)
}

impl Journal {
    /// A FULL FLUSH OF DIRECTORY `dir`: each file in it keeps
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
        self.file_points.retain(|(_, p, _)| p != path);
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
    /// A CONTROL: a device that keeps every write (no power cut loses anything), so a failure only a lossy cut can cause vanishes under it.
    pub(crate) lossless: bool,
    /// POSIX's worst case.
    pub(crate) tears: bool,
    /// THE HIGHEST TOTAL any write of `logpin` named, in any boot: what the
    /// node's mirror had counted, which no later bridge's total
    /// may fall below, whatever a power cut did to the pin.
    pin_total: Mutex<u64>,
    /// WRITE ERRORS (sim item S7), once armed ([`Device::arm_write_errors`]).
    faults: Mutex<Option<Faults>>,
    /// The path whose next durable or ordered replace fails after its rename, once
    /// ([`Device::fail_after_its_rename`]): a test's exact fault.
    after_once: Mutex<Option<PathBuf>>,
    /// The directory whose next barrier fails before its effect, once: an ordered replace
    /// [`Device::after_once`] named made its rename, and this is its directory's flush.
    dir_once: Mutex<Option<PathBuf>>,
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
            dir_once: Mutex::new(None),
        }
    }

    /// The next replace of `path` has its rename made and its directory flush fail (`EIO`),
    /// once, whichever tier it takes: a durable replace's own flush of its directory, or the
    /// barrier of its directory that follows an ordered replace (a start's root barrier).
    pub(crate) fn fail_after_its_rename(&self, path: &Path) {
        *lock(&self.after_once) = Some(path.to_path_buf());
    }

    /// ARM WRITE ERRORS (sim item S7).
    pub(crate) fn arm_write_errors(&self, rng: Rng, per: u64) {
        *lock(&self.faults) = Some(Faults {
            rng,
            per: per.max(2),
            full: false,
            on: true,
            counts: WriteErrors::default(),
        });
    }

    /// The disk fills (`true`) or has room again (`false`).
    pub(crate) fn fill(&self, full: bool) {
        if let Some(f) = lock(&self.faults).as_mut().filter(|f| f.on) {
            f.full = full;
        }
    }

    /// Whether the disk is full now ([`Device::fill`]).
    pub(crate) fn is_full(&self) -> bool {
        lock(&self.faults).as_ref().is_some_and(|f| f.on && f.full)
    }

    /// A CLEAN DISK from now on.
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

    /// `w` as the trace names it.
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

    /// WHETHER `w` FAILS BEFORE ITS EFFECT ([`crate::fx::Disk::before`]).
    pub(crate) fn fails_before(&self, w: Write<'_>) -> Option<io::Error> {
        if let Write::SyncDir {
            path,
            flush: Flush::Ordered,
        } = w
        {
            let mut once = lock(&self.dir_once);
            if once.as_deref() == Some(path) {
                *once = None;
                return Some(io::Error::from_raw_os_error(EIO));
            }
        }
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
    /// and of the appends (part of its line written, `ENOSPC`).
    fn fails_after(&self, w: Write<'_>) -> Option<After> {
        if let Write::Replace {
            path,
            flush: Some(flush),
            ..
        } = w
        {
            let mut once = lock(&self.after_once);
            if once.as_deref() == Some(path) {
                *once = None;
                if flush.is_durable() {
                    return Some(After::Renamed);
                }
                *lock(&self.dir_once) = path.parent().map(Path::to_path_buf);
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

    /// THE STATE DIR MADE `w` ([`crate::fx::Disk::after`]).
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

    /// A file the world wrote before the bridge ever ran (the provisioned node id), on the media.
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
                let covered = j.files.get(path).map_or(0, |f| f.pending.len());
                if covered > 0 {
                    j.file_points.push((at, path.to_path_buf(), covered));
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

    /// A POWER CUT: every file under the state dir rewritten to one image the media may hold, drawn from `rng`.
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
        let file_points = std::mem::take(&mut j.file_points);
        let mut notes = Vec::new();
        for (path, f) in &mut j.files {
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
            let mut data_on_media = 0;
            for (at, flushed, covered) in &file_points {
                if *at < cut && flushed == path {
                    let covered = (*covered).min(keep);
                    if data_flushed(&f.durable, &f.pending, covered, floor) {
                        floor = floor.max(covered);
                        data_on_media = data_on_media.max(covered);
                    }
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
                // How many of its pending writes the picked version holds
                // ([`File::versions`]: the media's own first where `floor` is 0).
                let writes = if floor == 0 { pick } else { floor + pick };
                let tearable = self.tears
                    && writes > data_on_media
                    && (versions[pick].1.is_some() || versions[pick].2);
                (pick, tearable && rng.chance(1, 4))
            };
            let (version, torn_from, replace) = versions[pick].clone();
            let image = match (torn, version) {
                (true, Version::Bytes(bytes, mode)) if replace => {
                    let cut = usize::try_from(rng.below(bytes.len().max(1) as u64)).unwrap_or(0);
                    Version::Bytes(bytes[..cut.min(bytes.len())].to_vec(), mode)
                }
                (true, Version::Bytes(mut bytes, mode)) => {
                    let from = torn_from.unwrap_or(0);
                    let cut = from
                        + usize::try_from(rng.below((bytes.len() - from) as u64 + 1)).unwrap_or(0);
                    let cut = cut.min(bytes.len());
                    if rng.chance(1, 2) {
                        // The append's length reached the media and its bytes past
                        // the cut did not: they read as zeros (a tear inside it).
                        bytes[cut..].fill(0);
                        Version::Bytes(bytes, mode)
                    } else {
                        Version::Bytes(bytes[..cut].to_vec(), mode)
                    }
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
