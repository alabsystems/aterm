// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The durable log's STORAGE SEAM: every byte [`BrokerLog`](crate::BrokerLog) reads or
//! writes on its own file, and every read and atomic replacement of its identity
//! sidecar, goes through the two enums here.
//!
//! In production each has one variant — `Os`, a real `File` and the log's real path —
//! so every match compiles away to the call it wraps. Under `cfg(test)` each gains a
//! `Sim` variant backed by the crash simulation's device (`crash_sim.rs`), which is how
//! the REAL writer (`stage_*` + `commit_batch`) and the REAL open (recovery, the
//! identity rules, both sidecar phases) run unchanged against a device that can lose
//! power after any operation. The seam covers the sidecar as well as the log on
//! purpose: a seam over the log alone would make a sidecar written in the wrong order
//! relative to the log's flush invisible, and that order is half of what recovery
//! rests on.
//!
//! Enums, not `dyn` traits: a caller-supplied `dyn` object here would be an open
//! principal for Trust's thread-spawn census (the same reason `Broker`'s `Shared` holds
//! no `dyn Fn`). Retention and anti-rollback stay `Os`-only and refuse `Unsupported` on
//! a simulated log, which the simulation never asks of them.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::store::{create_fresh, sync_parent_dir};

#[cfg(clean_islands)]
#[path = "storage_island.rs"]
mod storage_island;
#[cfg(not(clean_islands))]
pub use crate::twins::storage_len_advance;
#[cfg(clean_islands)]
pub use storage_island::storage_len_advance;

contract_fn! {
    /// Verified check that a storage truncation target does not exceed the current file length
    /// (`target <= current`), linked via `clean-kernel-defeq` to `storage_truncate_within_len_isl`.
    #[must_use]
    pub fn storage_truncate_within_len(target: u64, current: u64) -> bool
    clauses [ensures result == (target <= current)]
    {
        target <= current
    }
}

/// The log file itself.
pub(crate) enum Backing {
    /// The real file, exclusively locked by the open. SHARED — an `Arc`, never a second
    /// descriptor — with the [`FlushHandle`]s the writer takes to flush it outside the
    /// log's lock ([`flush_handle`](Self::flush_handle) says why).
    Os(Arc<File>),
    /// The crash simulation's device.
    #[cfg(test)]
    Sim(crate::crash_sim::Sim),
}

impl Drop for Backing {
    fn drop(&mut self) {
        // Release `flock(LOCK_UN)` explicitly whenever `Backing` drops (including on
        // early-return errors in `BrokerLog::open_core` before `BrokerLog` is constructed,
        // and on `compact_switch` replacement while `FlushHandle`s or concurrent
        // `fork`/`posix_spawn` children may still hold the open file description).
        self.unlock();
    }
}

impl Backing {
    /// The real file `file`.
    pub(crate) fn from_file(file: File) -> Backing {
        Backing::Os(Arc::new(file))
    }

    /// Read the whole file from its start.
    pub(crate) fn read_to_end(&mut self, out: &mut Vec<u8>) -> io::Result<usize> {
        match self {
            Backing::Os(f) => {
                let mut f: &File = f;
                f.seek(SeekFrom::Start(0))?;
                f.read_to_end(out)
            }
            #[cfg(test)]
            Backing::Sim(s) => Ok(s.read_log(out)),
        }
    }

    /// Append `bytes` at the end of the file (the log only ever appends; a rollback
    /// truncates with [`resize_file`](Self::resize_file) and re-seeks).
    pub(crate) fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        match self {
            Backing::Os(f) => (&**f).write_all(bytes),
            #[cfg(test)]
            Backing::Sim(s) => {
                s.append(bytes);
                Ok(())
            }
        }
    }

    /// Truncate (or extend with zeros) to `len` bytes.
    pub(crate) fn resize_file(&self, len: u64) -> io::Result<()> {
        match self {
            Backing::Os(f) => {
                let op: fn(&File, u64) -> io::Result<()> = File::set_len;
                op(f, len)
            }
            #[cfg(test)]
            Backing::Sim(s) => {
                let op: fn(&crate::crash_sim::Sim, u64) = crate::crash_sim::Sim::set_len;
                op(s, len);
                Ok(())
            }
        }
    }

    /// Position the next write at the end of the file.
    pub(crate) fn seek_end(&mut self) -> io::Result<()> {
        match self {
            Backing::Os(f) => {
                let _ = (&**f).seek(SeekFrom::End(0))?;
                Ok(())
            }
            #[cfg(test)]
            Backing::Sim(_) => Ok(()),
        }
    }

    /// A FULL flush of the file's data: `sync_data`, which on Apple is
    /// `F_FULLFSYNC` (the drive's cache emptied too) and elsewhere `fdatasync`.
    pub(crate) fn sync_data(&self) -> io::Result<()> {
        match self {
            Backing::Os(f) => astream_sys::full_sync_data(f),
            #[cfg(test)]
            Backing::Sim(s) => {
                s.full_flush();
                Ok(())
            }
        }
    }

    /// An ORDERING flush ([`astream_sys::barrier_sync`]): on Apple `F_BARRIERFSYNC`,
    /// which issues every write made so far to the device ahead of every write made
    /// after it, without waiting for the device to hold either; elsewhere a full flush.
    pub(crate) fn barrier(&self) -> io::Result<()> {
        match self {
            Backing::Os(f) => astream_sys::barrier_sync(f),
            #[cfg(test)]
            Backing::Sim(s) => {
                s.barrier();
                Ok(())
            }
        }
    }

    /// Whether this file can be asked for an ordering-only flush at all: a real file on
    /// Apple, where `F_BARRIERFSYNC` exists (whether its DEVICE honours it is the open's
    /// probe), and always the simulated device, which models one on every platform.
    pub(crate) fn barrier_exists(&self) -> bool {
        match self {
            Backing::Os(_) => cfg!(target_vendor = "apple"),
            #[cfg(test)]
            Backing::Sim(_) => true,
        }
    }

    /// A handle that flushes this file IN FULL from another place than the log's own
    /// lock: the file's own descriptor, shared, or the simulated device. `fail` stands in
    /// the fault-injection seam's failed fsync.
    ///
    /// SHARED, NOT DUPLICATED, so taking one cannot fail. It used to be a `try_clone` per
    /// flush — every escalated batch, every horizon (about once a second under traffic),
    /// every `flush_durable` — and a `dup` refused for want of a descriptor (`EMFILE`, a
    /// broker near the default 256 soft limit) was a full flush that could not be issued,
    /// so it was handled as a FAILED one: the run recorded lossy, the log latched, the
    /// waiting ops told "outcome unknown", though no flush had run and nothing could have
    /// been lost. The descriptor is the open file's; the `Arc` keeps it open for a flush
    /// in flight when retention replaces the file (the flush then reaches the unlinked
    /// original, which is harmless: see [`FlushHandle`]), exactly as the second
    /// descriptor did, and the exclusive lock rides the open file description either way.
    pub(crate) fn flush_handle(&self, fail: bool) -> FlushHandle {
        let backing = match self {
            Backing::Os(f) => HandleBacking::Os(Arc::clone(f)),
            #[cfg(test)]
            Backing::Sim(s) => HandleBacking::Sim(s.clone()),
        };
        FlushHandle { backing, fail }
    }

    /// What the writer writes and flushes a batch through with the log's lock RELEASED
    /// ([`BatchOut`]): the file's own descriptor, shared, or the simulated device.
    /// Shared for the reason a [`FlushHandle`] is ([`flush_handle`](Self::flush_handle)):
    /// taking one cannot fail and opens nothing, so a broker at its descriptor limit
    /// (`EMFILE`) fails no batch for it — where a `dup` per batch, or one kept and made
    /// again after every failed batch and every retention rewrite, could.
    pub(crate) fn batch_out(&self) -> BatchOut {
        BatchOut(match self {
            Backing::Os(f) => HandleBacking::Os(Arc::clone(f)),
            #[cfg(test)]
            Backing::Sim(s) => HandleBacking::Sim(s.clone()),
        })
    }

    /// A FULL flush of the file's data and metadata (`sync_all`).
    pub(crate) fn sync_all(&self) -> io::Result<()> {
        match self {
            Backing::Os(f) => astream_sys::full_sync(f),
            #[cfg(test)]
            Backing::Sim(s) => {
                s.full_flush();
                Ok(())
            }
        }
    }

    /// Release the exclusive advisory lock (the OS releases it at close anyway).
    pub(crate) fn unlock(&self) {
        match self {
            Backing::Os(f) => {
                let _ = f.unlock();
            }
            #[cfg(test)]
            Backing::Sim(_) => {}
        }
    }

    /// `len` bytes from byte `start` (for the newest frame a clean close
    /// fingerprints), leaving the next write at the end of the file.
    pub(crate) fn read_range(&mut self, start: u64, len: usize) -> io::Result<Vec<u8>> {
        match self {
            Backing::Os(f) => {
                let mut f: &File = f;
                let mut buf = Vec::new();
                f.seek(SeekFrom::Start(start))?;
                let read = f.take(len as u64).read_to_end(&mut buf);
                let mut f: &File = f;
                f.seek(SeekFrom::End(0))?;
                read?;
                if buf.len() != len {
                    return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
                }
                Ok(buf)
            }
            #[cfg(test)]
            Backing::Sim(s) => {
                let mut all = Vec::new();
                s.read_log(&mut all);
                let start = usize::try_from(start).unwrap_or(usize::MAX);
                all.get(start..start.saturating_add(len))
                    .map(<[u8]>::to_vec)
                    .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))
            }
        }
    }

    /// Every byte of the file as stored (empty when it cannot be read): the
    /// inspection path, never the hot one.
    pub(crate) fn stored(&self, path: &Path) -> Vec<u8> {
        match self {
            Backing::Os(_) => {
                let Ok(b) = std::fs::read(path) else {
                    return Vec::new();
                };
                b
            }
            #[cfg(test)]
            Backing::Sim(s) => {
                let mut all = Vec::new();
                s.read_log(&mut all);
                all
            }
        }
    }

    /// The real file, for the paths that exist only on disk (retention's rewrite and
    /// the permissions it copies); `Unsupported` on a simulated log.
    #[cfg_attr(not(feature = "retention"), allow(dead_code))]
    pub(crate) fn os(&self) -> io::Result<&File> {
        match self {
            Backing::Os(f) => Ok(f),
            #[cfg(test)]
            Backing::Sim(_) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "not on a simulated log",
            )),
        }
    }
}

/// A FULL flush of the log taken OUTSIDE its lock: what the writer issues for an
/// escalated record, for the durability horizon and for `flush_durable`
/// ([`BrokerLog::flush_handle`](crate::BrokerLog)), so that a reader asking for a page
/// never waits on a device flush.
///
/// It is the log's own descriptor, shared ([`Backing::flush_handle`]); a second
/// descriptor would be the same flush, since `F_FULLFSYNC` (and `fdatasync`) flush the
/// FILE — every dirty page of its vnode, through whichever descriptor wrote it — not the
/// descriptor. And why it is safe without the lock: the writer thread is the only
/// appender and it is the thread issuing this flush, so no `write()` overlaps it (a write
/// would block behind a flush of the same vnode); the one other thing that touches the
/// file, retention, replaces it with a copy it has itself flushed in full, so a flush of
/// the unlinked original afterwards is harmless and what it was meant to cover is
/// durable at the path either way.
pub(crate) struct FlushHandle {
    backing: HandleBacking,
    /// The fault-injection seam's failed fsync, as it stood when the handle was taken.
    fail: bool,
}

enum HandleBacking {
    Os(Arc<File>),
    #[cfg(test)]
    Sim(crate::crash_sim::Sim),
}

impl FlushHandle {
    /// Flush the file in full (`F_FULLFSYNC` on Apple, `fdatasync` elsewhere).
    pub(crate) fn full(&self) -> io::Result<()> {
        if self.fail {
            return Err(io::Error::other("injected fsync fault"));
        }
        match &self.backing {
            HandleBacking::Os(f) => astream_sys::full_sync_data(f),
            #[cfg(test)]
            HandleBacking::Sim(s) => {
                s.full_flush();
                Ok(())
            }
        }
    }

    /// The crash simulation's negative control NC7: an ORDERING flush where the full one
    /// belongs, so the product believes durable what is only ordered.
    #[cfg(test)]
    pub(crate) fn barrier_instead(&self) {
        if let HandleBacking::Sim(s) = &self.backing {
            s.barrier();
        }
    }
}

/// What a batch is written and flushed through OUTSIDE the log's lock
/// ([`Backing::batch_out`], lent to a batch by `BrokerLog::commit_begin`): the broker's
/// writer thread writes and flushes every batch on it while readers take the lock.
///
/// WHY THE LOG'S OWN DESCRIPTOR, SHARED. The write lands exactly where a write under the
/// lock would, at the end: the descriptor's file OFFSET is the one a rollback's
/// truncate-and-seek positions, and the flush — full, or the ordering barrier — covers
/// the vnode's dirty pages whichever descriptor wrote them, as a [`FlushHandle`]'s does.
/// Shared and not a second descriptor, so taking one cannot fail and opens nothing
/// ([`Backing::batch_out`]); and the `Arc` names the file the batch began on, which is
/// the log's file at its end too (see below).
///
/// WHY NOTHING ELSE MOVES THAT OFFSET WHILE A BATCH IS WRITTEN. The writer thread is the
/// only appender, and it is the thread writing here. What else touches the file under
/// the lock refuses while a batch is staged — retention's rewrite (the one thing that
/// replaces the file), the un-batched retirement, the fenced replica append, the clean
/// close and its read-back of the newest frame (`staged` stays non-empty, and `writing`
/// set, from the batch's begin to its end) — and the broker's retention calls also take
/// the writer's batch lock first, so they wait for the batch instead.
pub(crate) struct BatchOut(HandleBacking);

impl BatchOut {
    /// Append `bytes` at the shared offset (the end of the file).
    pub(crate) fn write_all(&self, bytes: &[u8]) -> io::Result<()> {
        match &self.0 {
            HandleBacking::Os(f) => (&**f).write_all(bytes),
            #[cfg(test)]
            HandleBacking::Sim(s) => {
                s.append(bytes);
                Ok(())
            }
        }
    }

    /// A FULL flush of the file's data, as [`Backing::sync_data`].
    pub(crate) fn sync_data(&self) -> io::Result<()> {
        match &self.0 {
            HandleBacking::Os(f) => astream_sys::full_sync_data(f),
            #[cfg(test)]
            HandleBacking::Sim(s) => {
                s.full_flush();
                Ok(())
            }
        }
    }

    /// An ORDERING flush, as [`Backing::barrier`].
    pub(crate) fn barrier(&self) -> io::Result<()> {
        match &self.0 {
            HandleBacking::Os(f) => astream_sys::barrier_sync(f),
            #[cfg(test)]
            HandleBacking::Sim(s) => {
                s.barrier();
                Ok(())
            }
        }
    }

    /// Whether this is `file`'s own descriptor — the same open file, not a second
    /// descriptor of it, and not another file's.
    #[cfg(test)]
    pub(crate) fn is_of(&self, file: &Backing) -> bool {
        match (&self.0, file) {
            (HandleBacking::Os(out), Backing::Os(f)) => Arc::ptr_eq(out, f),
            _ => false,
        }
    }
}

/// The log's directory, as far as the open's sidecar reads and atomic replacements go.
pub(crate) enum Dir {
    /// The real directory; holds the LOG's path, from which each sidecar's is derived.
    Os(PathBuf),
    /// The crash simulation's device, which keeps the sidecar beside its log.
    #[cfg(test)]
    Sim(crate::crash_sim::Sim),
}

impl Dir {
    /// The log's path: the stem sidecar names derive from, and what messages name.
    pub(crate) fn log_path(&self) -> &Path {
        match self {
            Dir::Os(p) => p,
            #[cfg(test)]
            Dir::Sim(s) => s.path(),
        }
    }

    /// Whether this is the real directory (the paths retention, anti-rollback, the
    /// replica marker and the base sidecar exist on).
    pub(crate) const fn is_os(&self) -> bool {
        matches!(self, Dir::Os(_))
    }

    /// The bytes of `<log>.id`, or `None` when there is none.
    pub(crate) fn read_id(&self) -> io::Result<Option<Vec<u8>>> {
        match self {
            Dir::Os(p) => match std::fs::read(crate::logid::identity_path(p)) {
                Ok(b) => Ok(Some(b)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e),
            },
            #[cfg(test)]
            Dir::Sim(s) => Ok(s.read_id()),
        }
    }

    /// Durably replace `<log>.id` with `bytes`: tmp file, fsync, rename, directory
    /// fsync, never following a link planted at either name. Durable when it returns.
    pub(crate) fn replace_id(&self, bytes: &[u8]) -> io::Result<()> {
        match self {
            Dir::Os(p) => {
                let path = crate::logid::identity_path(p);
                let mut tmp = path.clone();
                tmp.as_mut_os_string().push(".tmp");
                {
                    let mut f = create_fresh(&tmp, &mut OpenOptions::new())?;
                    f.write_all(bytes)?;
                    astream_sys::full_sync(&f)?;
                }
                crate::store::rename_path(&tmp, &path)?;
                sync_parent_dir(&path)
            }
            #[cfg(test)]
            Dir::Sim(s) => {
                s.replace_id(bytes);
                Ok(())
            }
        }
    }
}
