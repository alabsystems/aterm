// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! macOS kernel resource coalitions ([`coalition_of`], [`CoalitionIds`],
//! [`crate::procs::processes_keeping`], [`coalitions`], [`self_check`], [`decode_usage`],
//! [`decode_list`]).

use std::collections::HashMap;

use crate::procs::ticks_to_ns;

/// The list read stops here.
pub const MAX_COALITIONS: usize = 16_384;

/// The entries a list buffer holds beyond the kernel's size answer.
pub const LIST_HEADROOM: usize = 64;

/// One list entry (`struct procinfo_coalinfo`).
pub const LIST_ENTRY: usize = 16;

/// The `u64` words a usage record is read into.
pub const USAGE_WORDS: usize = 64;

/// What each word of a usage buffer holds before the read.
pub const UNWRITTEN: u64 = u64::MAX;

/// The thread QoS classes a coalition's CPU time is split by (`COALITION_NUM_THREAD_QOS_TYPES`).
pub const QOS_CLASSES: usize = 7;

const TYPE_RESOURCE: u32 = 0;

const TASKS_STARTED: usize = 0;
const TASKS_EXITED: usize = 1;
const CPU_TIME: usize = 3;
const BYTES_READ: usize = 6;
const EQOS_LEN: usize = 23;
const EQOS: usize = 24;
const PHYS_FOOTPRINT: usize = 40;

/// One resource coalition's counters, read now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CoalitionUsage {
    /// Its id.
    pub id: u64,
    /// Tasks that joined it since it was made.
    pub tasks_started: u64,
    /// Tasks of it that have exited.
    pub tasks_exited: u64,
    /// Its members' CPU time, exited members' included, nanoseconds.
    pub cpu_ns: u64,
    /// Bytes its members read from storage, exited members' included.
    pub bytes_read: u64,
    /// [`Self::cpu_ns`] by the thread QoS it ran at, nanoseconds.
    pub eqos_ns: [u64; QOS_CLASSES],
    /// Its live members' footprint, bytes; `None` where this kernel's record ends before it.
    pub footprint: Option<u64>,
}

/// One read of every resource coalition ([`coalitions`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CoalitionRead {
    /// How many coalitions the kernel listed.
    pub listed: u32,
    /// The highest id the kernel listed, answered or not (`None` for an empty list).
    pub newest: Option<u64>,
    /// Whether the list filled its buffer, so coalitions past it (the newest) may be missing.
    pub truncated: bool,
    /// The counters of each listed coalition that answered.
    pub rows: Vec<CoalitionUsage>,
}

/// Every resource coalition and its counters; `None` where the list cannot be read, and off macOS.
#[must_use]
pub fn coalitions() -> Option<CoalitionRead> {
    imp::coalitions()
}

/// The id of `pid`'s resource coalition; `None` when the kernel does not say, and off macOS.
#[must_use]
pub fn coalition_of(pid: u32) -> Option<u64> {
    imp::coalition_of(pid)
}

/// Each swept process's resource coalition id, kept by pid under the pid version it was read at
/// ([`crate::procs::processes_keeping`]), so a sweep reads the coalition only of a process it has
/// not read under its current version.
///
/// A kept id stays the process's own while its version stays: xnu adopts a task into its
/// coalitions only where it makes the task (`task_create_internal`, for a fork, a spawn, or the
/// new task an exec makes; `coalition_adopt_task_internal` refuses a task already in one of that
/// type) and removes it only as the task ends, and nothing moves a live task; a fork or an exec
/// gives the process a new pid version (`p_idversion`), as a reused pid's new process has.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CoalitionIds {
    /// Each pid's kept id: the version it was read under, the id, and the sweep that last asked.
    kept: HashMap<u32, (i32, u64, u64)>,
    /// The sweeps begun.
    sweep: u64,
}

impl CoalitionIds {
    /// Starts a sweep: [`Self::end_sweep`] then drops every pid this one does not ask about.
    pub fn begin_sweep(&mut self) {
        self.sweep = self.sweep.wrapping_add(1);
    }

    /// `pid`'s coalition at pid `version`: the id kept under that version, else `read`'s, kept
    /// under it. A read that answers nothing, or with no version, is not kept.
    pub fn of(
        &mut self,
        pid: u32,
        version: Option<i32>,
        read: impl FnOnce() -> Option<u64>,
    ) -> Option<u64> {
        let Some(version) = version else {
            self.kept.remove(&pid);
            return read();
        };
        if let Some(kept) = self.kept.get_mut(&pid).filter(|k| k.0 == version) {
            kept.2 = self.sweep;
            return Some(kept.1);
        }
        let read = read();
        match read {
            Some(id) => {
                self.kept.insert(pid, (version, id, self.sweep));
            }
            None => {
                self.kept.remove(&pid);
            }
        }
        read
    }

    /// `pid`'s coalition and pid version as a sweep reads them: `version`'s read, then
    /// [`Self::of`] under it, where `coalition`'s read is kept only if `version` reads the same
    /// again after it (no exec, and no new process at the pid, between). A read the version
    /// moved across answers `None` and keeps nothing.
    pub fn read(
        &mut self,
        pid: u32,
        mut version: impl FnMut() -> Option<i32>,
        coalition: impl FnOnce() -> Option<u64>,
    ) -> (Option<u64>, Option<i32>) {
        let before = version();
        let id = self.of(pid, before, || {
            let id = coalition()?;
            (version() == before).then_some(id)
        });
        (id, before)
    }

    /// Ends a sweep: drops every pid it did not ask about (one that ended, or a zombie).
    pub fn end_sweep(&mut self) {
        let sweep = self.sweep;
        self.kept.retain(|_, k| k.2 == sweep);
    }

    /// `pid`'s kept id and the pid version it was read under.
    #[must_use]
    pub fn kept(&self, pid: u32) -> Option<(i32, u64)> {
        self.kept.get(&pid).map(|&(version, id, _)| (version, id))
    }

    /// How many pids' ids are kept.
    #[must_use]
    pub fn len(&self) -> usize {
        self.kept.len()
    }

    /// Whether no pid's id is kept.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.kept.is_empty()
    }
}

/// Whether the lane reads right on this OS build for live process `pid`.
///
/// # Errors
/// The word of the first check that missed.
pub fn self_check(pid: u32) -> Result<u64, &'static str> {
    imp::self_check(pid)
}

/// Decode coalition `id`'s counters from `raw` ([`crate::procs::kernel_timebase`], [`UNWRITTEN`]).
#[must_use]
pub fn decode_usage(
    id: u64,
    raw: &[u64; USAGE_WORDS],
    (numer, denom): (u32, u32),
) -> Option<CoalitionUsage> {
    if raw[EQOS_LEN] != QOS_CLASSES as u64 {
        return None;
    }
    let ns = |ticks: u64| ticks_to_ns(ticks, numer, denom);
    let mut eqos_ns = [0; QOS_CLASSES];
    for (out, ticks) in eqos_ns.iter_mut().zip(&raw[EQOS..EQOS + QOS_CLASSES]) {
        *out = ns(*ticks);
    }
    Some(CoalitionUsage {
        id,
        tasks_started: raw[TASKS_STARTED],
        tasks_exited: raw[TASKS_EXITED],
        cpu_ns: ns(raw[CPU_TIME]),
        bytes_read: raw[BYTES_READ],
        eqos_ns,
        footprint: (raw[PHYS_FOOTPRINT] != UNWRITTEN).then_some(raw[PHYS_FOOTPRINT]),
    })
}

/// Resource coalitions in `bytes` as `(id, live tasks)` ([`LIST_ENTRY`]).
#[must_use]
pub fn decode_list(bytes: &[u8]) -> Vec<(u64, u32)> {
    let (entries, _cut) = bytes.as_chunks::<LIST_ENTRY>();
    entries
        .iter()
        .filter_map(|e| {
            let id = u64::from_ne_bytes(e[..8].try_into().ok()?);
            let kind = u32::from_ne_bytes(e[8..12].try_into().ok()?);
            let tasks = u32::from_ne_bytes(e[12..].try_into().ok()?);
            (kind == TYPE_RESOURCE).then_some((id, tasks))
        })
        .collect()
}

/// List buffer entry count for size `need` ([`LIST_HEADROOM`], [`MAX_COALITIONS`]).
#[cfg(any(target_os = "macos", test))]
#[must_use]
pub(crate) fn list_room(need: i64) -> Option<usize> {
    let entries = usize::try_from(need).ok().filter(|n| *n > 0)? / LIST_ENTRY;
    Some((entries + LIST_HEADROOM).min(MAX_COALITIONS))
}

/// The read of the `listed` coalitions.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn read_of(
    listed: &[(u64, u32)],
    truncated: bool,
    usage: impl Fn(u64) -> Option<[u64; USAGE_WORDS]>,
    timebase: (u32, u32),
) -> CoalitionRead {
    CoalitionRead {
        listed: u32::try_from(listed.len()).unwrap_or(u32::MAX),
        newest: listed.iter().map(|&(id, _)| id).max(),
        truncated,
        rows: listed
            .iter()
            .filter_map(|&(id, _)| decode_usage(id, &usage(id)?, timebase))
            .collect(),
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::{c_int, c_void};

    use super::{CoalitionRead, LIST_ENTRY, UNWRITTEN, USAGE_WORDS};

    const PROC_PIDCOALITIONINFO: c_int = 20;
    const LISTCOALITIONS_SINGLE_TYPE: c_int = 2;
    const RESOURCE: c_int = super::TYPE_RESOURCE as c_int;

    unsafe extern "C" {
        fn proc_listcoalitions(
            flavor: c_int,
            coaltype: c_int,
            buffer: *mut c_void,
            buffersize: c_int,
        ) -> c_int;
        fn coalition_info_resource_usage(cid: u64, cru: *mut c_void, sz: usize) -> c_int;
    }

    pub(super) fn coalition_of(pid: u32) -> Option<u64> {
        let pid = c_int::try_from(pid).ok().filter(|p| *p > 0)?;
        let mut ids = [0u64; 5];
        let size = c_int::try_from(std::mem::size_of_val(&ids)).ok()?;
        // SAFETY: `ids` is `size` writable bytes, the flavor's whole record.
        let n = unsafe {
            libc::proc_pidinfo(pid, PROC_PIDCOALITIONINFO, 0, ids.as_mut_ptr().cast(), size)
        };
        (n == size && ids[0] != 0).then_some(ids[0])
    }

    /// The kernel's size answer for the resource list: the bytes it takes.
    pub(super) fn list_need() -> c_int {
        // SAFETY: A null buffer asks for the size only.
        unsafe {
            proc_listcoalitions(
                LISTCOALITIONS_SINGLE_TYPE,
                RESOURCE,
                std::ptr::null_mut(),
                0,
            )
        }
    }

    /// The resource list read into a buffer of `room` entries.
    pub(super) fn list_in(room: usize) -> Option<(Vec<(u64, u32)>, usize)> {
        let mut buf = vec![0u8; room * LIST_ENTRY];
        let bytes = c_int::try_from(buf.len()).ok()?;
        // SAFETY: `buf` is `bytes` writable bytes, the size passed.
        let n = unsafe {
            proc_listcoalitions(
                LISTCOALITIONS_SINGLE_TYPE,
                RESOURCE,
                buf.as_mut_ptr().cast(),
                bytes,
            )
        };
        let n = usize::try_from(n).ok().filter(|n| *n > 0)?.min(buf.len());
        Some((super::decode_list(&buf[..n]), n))
    }

    fn list() -> Option<(Vec<(u64, u32)>, bool)> {
        let room = super::list_room(i64::from(list_need()))?;
        list_in(room).map(|(list, wrote)| (list, wrote >= room * LIST_ENTRY))
    }

    fn raw_usage(id: u64) -> Option<[u64; USAGE_WORDS]> {
        let mut raw = [UNWRITTEN; USAGE_WORDS];
        // SAFETY: `raw` is writable for the size passed, and the kernel copies no more than that.
        let rc = unsafe {
            coalition_info_resource_usage(id, raw.as_mut_ptr().cast(), std::mem::size_of_val(&raw))
        };
        (rc == 0).then_some(raw)
    }

    pub(super) fn coalitions() -> Option<CoalitionRead> {
        let (listed, truncated) = list()?;
        let timebase = crate::procs::imp::timebase();
        Some(super::read_of(&listed, truncated, raw_usage, timebase))
    }

    pub(super) fn self_check(pid: u32) -> Result<u64, &'static str> {
        let id = coalition_of(pid).ok_or("no_coalition")?;
        let raw = raw_usage(id).ok_or("no_usage")?;
        let usage = super::decode_usage(id, &raw, crate::procs::imp::timebase()).ok_or("layout")?;
        if usage.tasks_started == 0 {
            return Err("no_tasks");
        }
        if usage.footprint == Some(0) {
            return Err("footprint");
        }
        if !list().ok_or("no_list")?.0.iter().any(|(c, _)| *c == id) {
            return Err("unlisted");
        }
        Ok(id)
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::CoalitionRead;

    pub(super) fn coalitions() -> Option<CoalitionRead> {
        None
    }

    pub(super) fn coalition_of(_pid: u32) -> Option<u64> {
        None
    }

    pub(super) fn self_check(_pid: u32) -> Result<u64, &'static str> {
        Err("unsupported")
    }
}

#[cfg(test)]
#[path = "coalitions_tests.rs"]
mod tests;
