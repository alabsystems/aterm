// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One thread's counters by CPU performance level (`proc_pidinfo(PROC_PIDTHREADCOUNTS)`,
//! macOS 13 on): the instructions it retired, the cycles it ran, its user and system time in
//! Mach ticks and the energy the kernel charged it, one row for each performance level (Apple
//! silicon has two or three). The flavor and its records are xnu-private
//! (`bsd/sys/proc_info_private.h`) and absent from the SDK, so no header holds their layout:
//! [`decode`] reads the kernel's bytes, bounded by how many it wrote, and decode tests of a
//! recorded shape and of malformed records hold it.

/// The most performance levels a reading holds: the buffer has room for four rows.
pub const MAX_LEVELS: usize = 4;

/// `struct proc_threadcounts`'s header: `u16` row count, `u16` and `u32` reserved.
const HEADER_BYTES: usize = 8;

/// `struct proc_threadcounts_data`: five `u64` counters.
const ROW_BYTES: usize = 40;

/// The buffer handed to the kernel: the header and [`MAX_LEVELS`] rows.
pub const BUFFER_BYTES: usize = HEADER_BYTES + MAX_LEVELS * ROW_BYTES;

/// `PROC_PIDTHREADCOUNTS` (`proc_info_private.h`).
#[cfg(target_os = "macos")]
const PROC_PIDTHREADCOUNTS: libc::c_int = 34;

/// One performance level's counters for a thread (`struct proc_threadcounts_data`), each
/// cumulative over the thread's life on that level.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LevelCounts {
    /// Instructions retired.
    pub instructions: u64,
    /// Cycles run.
    pub cycles: u64,
    /// User-mode time, Mach ticks (convert with [`crate::procs::kernel_timebase`]).
    pub user_time_mach: u64,
    /// Kernel-mode time, Mach ticks.
    pub system_time_mach: u64,
    /// Energy charged, nanojoules.
    pub energy_nj: u64,
}

/// What one read answered: the rows the kernel filled, in its order of performance levels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreadCounts {
    rows: [LevelCounts; MAX_LEVELS],
    len: usize,
}

impl ThreadCounts {
    /// The filled rows, one per performance level, at least one.
    #[must_use]
    pub fn levels(&self) -> &[LevelCounts] {
        &self.rows[..self.len]
    }

    /// Every level's counters summed, each saturating.
    #[must_use]
    pub fn total(&self) -> LevelCounts {
        self.levels()
            .iter()
            .fold(LevelCounts::default(), |a, r| LevelCounts {
                instructions: a.instructions.saturating_add(r.instructions),
                cycles: a.cycles.saturating_add(r.cycles),
                user_time_mach: a.user_time_mach.saturating_add(r.user_time_mach),
                system_time_mach: a.system_time_mach.saturating_add(r.system_time_mach),
                energy_nj: a.energy_nj.saturating_add(r.energy_nj),
            })
    }
}

/// Read the record the kernel wrote into `bytes`, of which it reported writing `written`: `None`
/// where the header names no row or more than [`MAX_LEVELS`], or where fewer bytes were written
/// (or exist) than the header's rows need. The record is in the host's byte order.
#[must_use]
pub fn decode(bytes: &[u8], written: usize) -> Option<ThreadCounts> {
    let len = usize::from(u16::from_ne_bytes(bytes.get(..2)?.try_into().ok()?));
    let need = HEADER_BYTES + len * ROW_BYTES;
    if len == 0 || len > MAX_LEVELS || written < need || bytes.len() < need {
        return None;
    }
    let word = |row: usize, field: usize| {
        let at = HEADER_BYTES + row * ROW_BYTES + field * 8;
        bytes
            .get(at..at + 8)
            .and_then(|b| b.try_into().ok())
            .map(u64::from_ne_bytes)
    };
    let mut rows = [LevelCounts::default(); MAX_LEVELS];
    for (i, row) in rows.iter_mut().enumerate().take(len) {
        *row = LevelCounts {
            instructions: word(i, 0)?,
            cycles: word(i, 1)?,
            user_time_mach: word(i, 2)?,
            system_time_mach: word(i, 3)?,
            energy_nj: word(i, 4)?,
        };
    }
    Some(ThreadCounts { rows, len })
}

/// Thread `tid` (its system-wide unique id, as `PROC_PIDLISTTHREADIDS` or
/// [`crate::self_cpu::ThreadReading::id`] gives it) of process `pid`: `None` where the kernel
/// answers no record (an older macOS, a thread that exited, another user's process, a machine
/// without the counters) or one [`decode`] refuses. Off macOS, always `None`.
#[must_use]
pub fn read(pid: u32, tid: u64) -> Option<ThreadCounts> {
    #[cfg(target_os = "macos")]
    {
        let mut buf = [0u8; BUFFER_BYTES];
        let size = libc::c_int::try_from(buf.len()).ok()?;
        let pid = libc::c_int::try_from(pid).ok()?;
        // SAFETY: `buf` is a writable buffer of exactly `size` bytes, which the call fills with
        // at most that many.
        let got = unsafe {
            libc::proc_pidinfo(
                pid,
                PROC_PIDTHREADCOUNTS,
                tid,
                buf.as_mut_ptr().cast(),
                size,
            )
        };
        decode(&buf, usize::try_from(got).ok()?)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (pid, tid);
        None
    }
}

#[cfg(test)]
#[path = "thread_counts_tests.rs"]
mod tests;
