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
//! * an unlink may or may not have reached the media (finding F2: a landed
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
//!   `sync_dir`): an unlink made before a durable write beside it is kept.
//!
//! Within what the directory flushes fixed, each file's image is drawn on its
//! own: nothing orders two files' unflushed writes against each other, and no
//! cross-log/state ordering of a real device is modelled. A process kill (not
//! a power cut) keeps everything: the page cache outlives the process.
//!
//! ## Torn files: POSIX's worst case, and finding F1
//!
//! POSIX promises nothing of an unflushed file's DATA across a power loss, so
//! a rename that reached the media may name a file whose bytes did not — an
//! empty or cut file. [`Device::tears`] lets a power cut leave that too (an
//! atomic-only replace cut at any byte short of its last, nothing included;
//! an append cut at any byte), and the committed worlds' devices do
//! (`world::Opts::seed`).
//!
//! FINDING F1 was what that did to the bridge: a torn `inflight/<seq>`
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
//! key and the lifetime total, rulings Q6 and Q13): a torn one is kept, and
//! the mirror stops for good (`world::World::mirror_stopped_by_its_torn_pin`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::{lock, Rng};
use crate::fx::Write;

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

/// One file's journal.
#[derive(Clone, Debug)]
struct File {
    /// What the media holds for sure.
    durable: Version,
    /// Every write since, oldest first.
    pending: Vec<Op>,
    /// How many of `pending`'s versions a flushed directory entry rules out:
    /// an image may hold version `k` only for `k + 1 >= floor` (`durable` is
    /// version `-1`).
    floor: usize,
}

impl File {
    /// The versions an image may hold, oldest first, each with how it may be
    /// torn: `(version, torn_from)` — a torn image of a replace is empty, of
    /// an append any prefix at or above `torn_from` bytes.
    fn versions(&self) -> Vec<(Version, Option<usize>, bool)> {
        let mut out = Vec::new();
        if self.floor == 0 {
            out.push((self.durable.clone(), None, false));
        }
        let mut now = self.durable.clone();
        for (k, op) in self.pending.iter().enumerate() {
            let (next, torn_from, replace) = match (op, &now) {
                (Op::Replace(bytes, mode), _) => (Version::Bytes(bytes.clone(), *mode), None, true),
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
            if k + 1 >= self.floor {
                out.push((next, torn_from, replace));
            }
        }
        out
    }
}

/// A FLUSH OF DIRECTORY `dir` (the module doc): each file in it keeps its
/// newest ENTRY change — a rename's version, an unlink, the file an append
/// created — and the versions before it can no longer come back; the appends
/// after it are data, and still may be lost.
fn flush_dir(files: &mut BTreeMap<PathBuf, File>, dir: &Path) {
    for (p, f) in files.iter_mut() {
        if p.parent() != Some(dir) {
            continue;
        }
        let mut absent = f.durable == Version::Absent;
        let mut entry = None;
        for (k, op) in f.pending.iter().enumerate() {
            if !matches!(op, Op::Append(_)) || absent {
                entry = Some(k);
            }
            absent = matches!(op, Op::Remove);
        }
        let Some(j) = entry else { continue };
        if j + 1 == f.pending.len() && matches!(f.pending[j], Op::Remove) {
            f.durable = Version::Absent;
            f.pending.clear();
            f.floor = 0;
        } else {
            f.floor = f.floor.max(j + 1);
        }
    }
}

/// THE DEVICE under one world's state dir.
#[derive(Debug)]
pub(crate) struct Device {
    root: PathBuf,
    files: Mutex<BTreeMap<PathBuf, File>>,
    /// A CONTROL: a device that keeps every write (no power cut loses
    /// anything), so a failure only a lossy cut can cause vanishes under it.
    pub(crate) lossless: bool,
    /// POSIX's worst case: a power cut may also TEAR an unflushed file (the
    /// module doc's finding F1). On in the committed worlds.
    pub(crate) tears: bool,
}

impl Device {
    pub(crate) fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            files: Mutex::new(BTreeMap::new()),
            lossless: false,
            tears: false,
        }
    }

    /// A file the world wrote before the bridge ever ran (the provisioned
    /// node id), on the media.
    pub(crate) fn seed(&self, path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).expect("seed a state file");
        lock(&self.files).insert(
            path.to_path_buf(),
            File {
                durable: Version::Bytes(bytes.to_vec(), 0o666),
                pending: Vec::new(),
                floor: 0,
            },
        );
    }

    /// Journal one write the state dir made ([`crate::fx::Disk::after`]).
    pub(crate) fn journal(&self, w: Write<'_>) {
        let mut files = lock(&self.files);
        let blank = || File {
            durable: Version::Absent,
            pending: Vec::new(),
            floor: 0,
        };
        match w {
            Write::Replace {
                path,
                bytes,
                durable,
                mode,
            } => {
                let f = files.entry(path.to_path_buf()).or_insert_with(blank);
                if durable {
                    f.durable = Version::Bytes(bytes.to_vec(), mode);
                    f.pending.clear();
                    f.floor = 0;
                    // The replace's directory flush is the directory's, for
                    // every entry in it.
                    if let Some(dir) = path.parent() {
                        flush_dir(&mut files, dir);
                    }
                } else {
                    f.pending.push(Op::Replace(bytes.to_vec(), mode));
                }
            }
            Write::Append { path, bytes } => {
                files
                    .entry(path.to_path_buf())
                    .or_insert_with(blank)
                    .pending
                    .push(Op::Append(bytes.to_vec()));
            }
            Write::Remove { path } => {
                files
                    .entry(path.to_path_buf())
                    .or_insert_with(blank)
                    .pending
                    .push(Op::Remove);
            }
            Write::SyncDir { path } => flush_dir(&mut files, path),
        }
    }

    /// A POWER CUT: every file under the state dir rewritten to one image the
    /// media may hold (the module doc), drawn from `rng`; each image is then
    /// what the media holds. Answers one line per file whose image is not its
    /// newest version, for the trace.
    pub(crate) fn power_cut(&self, rng: &mut Rng) -> Vec<String> {
        let mut files = lock(&self.files);
        let mut notes = Vec::new();
        for (path, f) in files.iter_mut() {
            let versions = f.versions();
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
            if pick != newest || torn {
                let rel = path.strip_prefix(&self.root).unwrap_or(path);
                notes.push(format!(
                    "power-cut {} -> version {pick}/{newest}{}",
                    rel.display(),
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
        notes
    }
}
