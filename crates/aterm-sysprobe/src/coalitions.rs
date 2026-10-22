// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LANE L1: THE KERNEL'S RESOURCE COALITIONS (macOS), what the cause engine
//! splits the machine's CPU and memory by
//! (`docs/DESIGN-resource-details-2026-10-20.md` §2.4).
//!
//! A resource coalition is the kernel's group of one launchd job or app and
//! every process it started since. Its counters (CPU time, by thread QoS too,
//! and bytes read) keep the work of members that have exited; its footprint
//! is the live members' only, where this kernel's record reaches it. Every
//! read here answers unprivileged for every user's coalitions:
//!
//! * [`coalition_of`] — one process's resource coalition
//!   (`proc_pidinfo(PROC_PIDCOALITIONINFO)`); a sweep asked for it reads it
//!   per process ([`crate::procs::processes_with`]).
//! * [`coalitions`] — every resource coalition (`proc_listcoalitions`) and
//!   each one's counters (`coalition_info_resource_usage`). A listed
//!   coalition that answers nothing is dropped (coalition 2 answers `EINVAL`),
//!   never read as zero.
//! * [`self_check`] — whether this OS build answers those calls as the
//!   layouts here read them, for a caller to ask once before it trusts the
//!   lane.
//!
//! No SDK header declares the calls or their layouts: they are xnu's
//! (`bsd/sys/coalition.h`, `bsd/sys/proc_info.h`), so `tests/proc_info_sdk.rs`
//! cannot hold them. The self-check and the decode tests of recorded records
//! do, and a record whose QoS count is not 7 decodes to nothing. The decoders
//! ([`decode_usage`], [`decode_list`]) are pure and tested on every platform;
//! every other platform's readers answer `None`.

use crate::procs::ticks_to_ns;

/// The list read stops here.
pub const MAX_COALITIONS: usize = 16_384;

/// The entries a list buffer holds beyond the kernel's size answer, for the
/// coalitions made between the two calls.
pub const LIST_HEADROOM: usize = 64;

/// One list entry (`struct procinfo_coalinfo`): a `u64` id, then the `u32`
/// type and the `u32` count of live tasks.
pub const LIST_ENTRY: usize = 16;

/// The `u64` words a usage record is read into. `struct
/// coalition_resource_usage` holds fewer (41 through `phys_footprint`), and
/// the kernel copies the smaller of the two.
pub const USAGE_WORDS: usize = 64;

/// What each word of a usage buffer holds before the read: a word past the
/// record the kernel copied (a record that ends before `phys_footprint`)
/// still holds it, and decodes as not given, never as zero.
pub const UNWRITTEN: u64 = u64::MAX;

/// The thread QoS classes a coalition's CPU time is split by
/// (`COALITION_NUM_THREAD_QOS_TYPES`).
pub const QOS_CLASSES: usize = 7;

/// `COALITION_TYPE_RESOURCE` (`bsd/sys/coalition.h`).
const TYPE_RESOURCE: u32 = 0;

// `struct coalition_resource_usage`'s words read here (`bsd/sys/coalition.h`).
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
    /// [`Self::cpu_ns`] by the thread QoS it ran at, nanoseconds:
    /// unspecified, maintenance, background, utility, legacy (default),
    /// user-initiated, user-interactive.
    pub eqos_ns: [u64; QOS_CLASSES],
    /// Its live members' footprint, bytes (0 with no live member); `None`
    /// where this kernel's record ends before it.
    pub footprint: Option<u64>,
}

/// One read of every resource coalition ([`coalitions`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CoalitionRead {
    /// How many coalitions the kernel listed.
    pub listed: u32,
    /// Whether the list filled its buffer, so coalitions past it (the
    /// newest) may be missing.
    pub truncated: bool,
    /// The counters of each listed coalition that answered.
    pub rows: Vec<CoalitionUsage>,
}

/// Every resource coalition and its counters; `None` where the list cannot
/// be read, and off macOS.
#[must_use]
pub fn coalitions() -> Option<CoalitionRead> {
    imp::coalitions()
}

/// The id of `pid`'s resource coalition; `None` when the kernel does not say
/// (the process is gone or a zombie), and off macOS.
#[must_use]
pub fn coalition_of(pid: u32) -> Option<u64> {
    imp::coalition_of(pid)
}

/// Whether the lane reads right on this OS build, asked of `pid` (a live
/// process; the caller's own): its coalition answers, that coalition's
/// record has 7 QoS classes, at least one task started and, where it gives a
/// footprint, one above zero (`pid` is a live member), and the list holds
/// it. The coalition's id, or the check that missed: `no_coalition`,
/// `no_usage`, `layout`, `no_tasks`, `footprint`, `no_list`, `unlisted`, and
/// `unsupported` off macOS.
///
/// # Errors
/// The word of the first check that missed.
pub fn self_check(pid: u32) -> Result<u64, &'static str> {
    imp::self_check(pid)
}

/// Coalition `id`'s counters from its usage record `raw`, CPU times scaled
/// from Mach ticks by the kernel's timebase `(numer, denom)`
/// ([`crate::procs::kernel_timebase`]); `None` unless the record holds 7
/// QoS classes, the layout read here (a zeroed record, or one the kernel
/// wrote nothing to, never decodes). A footprint word still [`UNWRITTEN`]
/// is not given. PURE.
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

/// The resource coalitions in a list buffer's written `bytes`, as `(id, live
/// tasks)`: whole [`LIST_ENTRY`]-byte entries of the resource type, a
/// trailing part ignored. PURE.
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

/// The entries a list buffer holds for the kernel's size answer `need` (the
/// bytes the whole list takes): those plus [`LIST_HEADROOM`], at most
/// [`MAX_COALITIONS`]; `None` for an answer of none (libproc answers zero for
/// a failure too). PURE.
#[cfg(any(target_os = "macos", test))]
#[must_use]
pub(crate) fn list_room(need: i64) -> Option<usize> {
    let entries = usize::try_from(need).ok().filter(|n| *n > 0)? / LIST_ENTRY;
    Some((entries + LIST_HEADROOM).min(MAX_COALITIONS))
}

/// The read of the `listed` coalitions, each one's record from `usage`
/// (`None` where the kernel refuses): a coalition that answers nothing, or
/// whose record does not decode, is dropped, never a zero row. PURE.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn read_of(
    listed: &[(u64, u32)],
    truncated: bool,
    usage: impl Fn(u64) -> Option<[u64; USAGE_WORDS]>,
    timebase: (u32, u32),
) -> CoalitionRead {
    CoalitionRead {
        listed: u32::try_from(listed.len()).unwrap_or(u32::MAX),
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

    /// `PROC_PIDCOALITIONINFO` (`bsd/sys/proc_info.h`): `struct
    /// proc_pidcoalitioninfo`, the resource then the jetsam coalition's id
    /// and three reserved words.
    const PROC_PIDCOALITIONINFO: c_int = 20;
    /// `LISTCOALITIONS_SINGLE_TYPE` (`bsd/sys/proc_info.h`): the coalitions of
    /// one type.
    const LISTCOALITIONS_SINGLE_TYPE: c_int = 2;
    /// The type listed: [`super::TYPE_RESOURCE`].
    const RESOURCE: c_int = super::TYPE_RESOURCE as c_int;

    // Declared here: xnu declares `coalition_info_resource_usage` in
    // `bsd/sys/coalition.h` and `proc_listcoalitions` in libproc's private
    // header, and no SDK header ships either; the SDK's
    // `libsystem_kernel.tbd` exports both, so they link by name.
    unsafe extern "C" {
        /// `int proc_listcoalitions(int flavor, int coaltype, void *buffer,
        /// int buffersize)`: the coalitions `flavor`/`coaltype` select,
        /// written into `buffer`; returns the BYTES written, the bytes the
        /// whole list takes for a null buffer, and 0 for a failure.
        fn proc_listcoalitions(
            flavor: c_int,
            coaltype: c_int,
            buffer: *mut c_void,
            buffersize: c_int,
        ) -> c_int;
        /// `int coalition_info_resource_usage(uint64_t cid, struct
        /// coalition_resource_usage *cru, size_t sz)`: 0, or -1 with `errno`
        /// (`EINVAL` for a coalition that is not a resource one or has none
        /// to give). The kernel copies the smaller of `sz` and its record.
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
        // SAFETY: a null buffer asks for the size only.
        unsafe {
            proc_listcoalitions(
                LISTCOALITIONS_SINGLE_TYPE,
                RESOURCE,
                std::ptr::null_mut(),
                0,
            )
        }
    }

    /// The resource list read into a buffer of `room` entries: its entries
    /// and the bytes the kernel wrote; `None` when it wrote none.
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

    /// The resource list, and whether it filled its buffer.
    fn list() -> Option<(Vec<(u64, u32)>, bool)> {
        let room = super::list_room(i64::from(list_need()))?;
        list_in(room).map(|(list, wrote)| (list, wrote >= room * LIST_ENTRY))
    }

    /// Coalition `id`'s raw usage record, each word the kernel did not write
    /// still [`UNWRITTEN`]; `None` when the kernel refuses.
    fn raw_usage(id: u64) -> Option<[u64; USAGE_WORDS]> {
        let mut raw = [UNWRITTEN; USAGE_WORDS];
        // SAFETY: `raw` is writable for the size passed, and the kernel
        // copies no more than that.
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
