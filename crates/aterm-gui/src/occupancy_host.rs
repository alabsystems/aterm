// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE OCCUPANCY FACTS BOARD (`docs/DESIGN-harness-occupancy-2026-10-20.md`,
//! slice 0: observe, type nothing): where the machine's occupancy stands —
//! its cores and load, the memory it can hand out, its memory pressure, the
//! home volume's level and the build slots — taken at each look of the
//! harness's disk watch (once a [`crate::harness_host::DISK_TICK`]).
//!
//! The board keeps no reader of its own. The vitals probe thread's one slot
//! reader fills each snapshot ([`crate::vitals_host`]'s `set_occupancy_facts`),
//! and the board projects the newest whole snapshot the `sys` host holds
//! ([`crate::sys_host::Shared::held_now`], never a pull) beside the disk
//! board's home row: so the board and `await sys slots>=…` read one proof.
//! That reader reads the slots once a minute at rest, and every few seconds
//! while an `await sys` names them ([`SLOTS_ARMED`]; design §3.1, the
//! cadence).
//! The disk look tells the window it looked first, so the reading it takes
//! then lands for the next look; a board whose snapshot is older than
//! [`STALE_AFTER`] says so.
//!
//! Slice 0 stores the board and logs each change of its slots and disk word.
//! It decides nothing and types nothing; slice 1 maps it to the decider's
//! reading.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use aterm_agent::harness::disk::Level;
use aterm_sysprobe::slots::{SlotsRead, Unread};
use aterm_vitals::model::{Pressure, SlotsUnread, SlotsView, Snapshot};

/// A board whose snapshot is older than this (two disk looks) is stale.
pub(crate) const STALE_AFTER: Duration = crate::harness_host::DISK_TICK.saturating_mul(2);

/// The occupancy facts at one look.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FactsBoard {
    /// When the board was taken (unix ms).
    pub(crate) taken_unix_ms: i64,
    /// When the snapshot it projects landed (unix ms); 0 for none.
    pub(crate) as_of_unix_ms: i64,
    /// Logical cores.
    pub(crate) cores: Option<u16>,
    /// The 1-minute load average, times 100.
    pub(crate) load1_x100: Option<u32>,
    /// The 5-minute load average, times 100.
    pub(crate) load5_x100: Option<u32>,
    /// Memory available without paging (`build-slot`'s figure), MiB.
    pub(crate) avail_mib: Option<u64>,
    /// The kernel's memory pressure; `None` where unread.
    pub(crate) pressure: Option<Pressure>,
    /// The home volume's level on the disk watch; `None` where the watch has
    /// not judged it.
    pub(crate) disk: Option<Level>,
    /// The build slots, as the slot reader last proved them.
    pub(crate) slots: Option<SlotsView>,
    /// The snapshot is older than [`STALE_AFTER`], or there is none.
    pub(crate) stale: bool,
}

/// A load average times 100, rounded; `None` for a value no load is.
fn x100(load: f64) -> Option<u32> {
    (load.is_finite() && load >= 0.0)
        .then(|| (load * 100.0).round().min(f64::from(u32::MAX)) as u32)
}

/// The board of `snap`, which landed at `snap_unix_ms` (`None`: no snapshot
/// yet, `snap` then the empty one), with the home volume at `disk`, taken at
/// `now_unix_ms`. Pure.
pub(crate) fn board_of(
    snap: &Snapshot,
    snap_unix_ms: Option<i64>,
    disk: Option<Level>,
    now_unix_ms: i64,
) -> FactsBoard {
    let stale_ms = i64::try_from(STALE_AFTER.as_millis()).unwrap_or(i64::MAX);
    FactsBoard {
        taken_unix_ms: now_unix_ms,
        as_of_unix_ms: snap_unix_ms.unwrap_or(0),
        cores: snap.total_cores().and_then(|c| u16::try_from(c).ok()),
        load1_x100: snap.load.and_then(|l| x100(l[0])),
        load5_x100: snap.load.and_then(|l| x100(l[1])),
        avail_mib: snap.avail.map(|b| b >> 20),
        pressure: snap
            .mem
            .as_ref()
            .map(|m| m.sample.pressure)
            .filter(|p| *p != Pressure::Unknown),
        disk: disk.filter(|l| *l != Level::Unknown),
        slots: snap.slots,
        stale: snap_unix_ms.is_none_or(|at| now_unix_ms.saturating_sub(at) > stale_ms),
    }
}

/// The slot reader's reading as a snapshot carries it. Pure.
pub(crate) fn slots_view(r: &SlotsRead) -> SlotsView {
    match r {
        SlotsRead::Unread(why) => SlotsView::Unread(match why {
            Unread::Unsupported => SlotsUnread::Unsupported,
            Unread::NotSampled => SlotsUnread::NotSampled,
            Unread::Refused => SlotsUnread::Refused,
            Unread::UnreadFormat => SlotsUnread::UnreadFormat,
        }),
        SlotsRead::Read { counts, .. } => SlotsView::Read {
            n: counts.n,
            held: counts.held,
            free: counts.free,
            pending: counts.pending,
            unknown: counts.unknown,
            discounted: counts.discounted,
        },
    }
}

/// The words a change of the board is logged with: its slots and its disk
/// level, which are what the log follows.
fn change_words(b: &FactsBoard) -> String {
    let slots = match b.slots {
        None => "slots=unread why=not_sampled".to_string(),
        Some(SlotsView::Unread(why)) => format!("slots=unread why={}", why.word()),
        Some(SlotsView::Read {
            n,
            held,
            free,
            pending,
            unknown,
            discounted,
        }) => format!(
            "slots={}/{n} held={held} free={free} pending={pending} unknown={unknown} \
             discounted={discounted}",
            free.saturating_sub(discounted)
        ),
    };
    let disk = b.disk.map_or("unknown", Level::as_str);
    format!("{slots} disk={disk}")
}

/// How many waits name the build slots now ([`Arming::arm`]).
#[derive(Debug, Default)]
pub(crate) struct Arming {
    armed: AtomicUsize,
}

/// One armed wait; dropped, it is no longer counted.
#[derive(Debug)]
pub(crate) struct Armed<'a>(&'a Arming);

impl Arming {
    /// None armed.
    pub(crate) const fn new() -> Self {
        Self {
            armed: AtomicUsize::new(0),
        }
    }

    /// Count one wait as armed until the guard drops; the first armed and
    /// the last gone are logged, since the slots' cadence changes then.
    pub(crate) fn arm(&self) -> Armed<'_> {
        if self.armed.fetch_add(1, Ordering::AcqRel) == 0 {
            aterm_log::info!("{}", aterm_sysprobe::slots::ARMED_LINE);
        }
        Armed(self)
    }

    /// Whether any wait names the slots now.
    pub(crate) fn armed(&self) -> bool {
        self.armed.load(Ordering::Acquire) > 0
    }
}

impl Drop for Armed<'_> {
    fn drop(&mut self) {
        if self.0.armed.fetch_sub(1, Ordering::AcqRel) == 1 {
            aterm_log::info!("{}", aterm_sysprobe::slots::AT_REST_LINE);
        }
    }
}

/// THE WAITS ON THE BUILD SLOTS: while one is armed (`await sys` naming
/// `slots`, [`crate::sys_host`]), the vitals thread's reader reads them every
/// `aterm_sysprobe::slots::PERIOD`; at rest, every
/// `aterm_sysprobe::slots::IDLE_PERIOD`, which the board's look needs.
pub(crate) static SLOTS_ARMED: Arming = Arming::new();

/// The board as last taken ([`look`]); `None` before the first look.
static BOARD: Mutex<Option<FactsBoard>> = Mutex::new(None);

/// The home volume's level on the disk board, where the watch judged it.
fn home_level() -> Option<Level> {
    let home = aterm_types::dirs::home_dir()?;
    let board = crate::disk_board::SHELF.board_lock();
    board.row_at(&home).map(|r| r.level)
}

/// TAKE THE BOARD at `now_unix_ms` (the disk look's): the newest whole
/// snapshot as held, never pulled, and the home volume's level; stored, and
/// logged when its slots or its disk word moved. Takes each lock alone.
pub(crate) fn look(now_unix_ms: i64) {
    let held = crate::sys_host::SYS.held_now();
    let disk = home_level();
    let board = match held.as_ref() {
        Some(h) => board_of(
            &h.snap,
            Some(i64::try_from(h.unix_ms).unwrap_or(i64::MAX)),
            disk,
            now_unix_ms,
        ),
        None => board_of(&Snapshot::default(), None, disk, now_unix_ms),
    };
    let words = change_words(&board);
    let moved = {
        let mut kept = BOARD.lock().unwrap_or_else(PoisonError::into_inner);
        let moved = kept.as_ref().map(change_words).as_deref() != Some(words.as_str());
        *kept = Some(board);
        moved
    };
    if moved {
        aterm_log::info!("occupancy board: {words}");
    }
}

/// The board as last taken; `None` before the first look. Slice 1's decider
/// reads it; slice 0 only its tests.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "slice 1's decider reads the board")
)]
pub(crate) fn board() -> Option<FactsBoard> {
    BOARD.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

#[cfg(test)]
#[path = "occupancy_host_tests.rs"]
mod tests;
